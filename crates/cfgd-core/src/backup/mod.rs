//! Declarative backup engine for `spec.backups[]`.
//!
//! One [`BackupUnit`] is one `spec.backups[]` entry bound to the runtime paths
//! it needs (config dir, profile name, state dir). [`run_backup`] executes it:
//! `preBackup` hooks, then an atomic copy of the source into the destination,
//! then `postBackup` hooks, then a recorded run and a retention prune.
//!
//! Snapshot payloads live on the filesystem, never in the state DB — the DB
//! only holds run records, and those records (not a filename glob) are what
//! retention pruning walks.

use std::path::{Path, PathBuf};

use crate::PathDisplayExt;
use crate::config::{BackupSpec, ScriptEntry, render_backup_name_pattern};
use crate::errors::{BackupError, Result};
use crate::output::{Printer, Role, collapse_to_subject_line};
use crate::reconciler::{
    ReconcileContext, ScriptEnvContext, ScriptPhase, ScriptReport, ScriptSubject, build_script_env,
    effective_continue_on_error, execute_script, script_default_workdir,
};
use crate::state::{BackupRunDraft, BackupRunRecord, BackupRunStatus, StateStore};

pub mod restore;
pub mod schedule;

#[cfg(test)]
mod tests;

pub use restore::{
    RestoreOutcome, RestoreTarget, SnapshotInfo, list_snapshots, restore_backup, restore_target,
    select_snapshot,
};
pub use schedule::next_run_at;

/// One `spec.backups[]` entry bound to the runtime context it needs.
///
/// The spec alone is not runnable: the destination default and the hook
/// execution environment both depend on values the config parser cannot know
/// (state dir, config dir, profile name, whether this is an apply or a
/// reconcile). Binding them once here is what keeps every dispatch site — CLI,
/// apply, daemon — from re-deriving them differently.
pub struct BackupUnit<'a> {
    spec: &'a BackupSpec,
    config_dir: &'a Path,
    profile_name: &'a str,
    state_dir: &'a Path,
    context: ReconcileContext,
    abort: Option<&'a crate::AbortFlag>,
}

impl<'a> BackupUnit<'a> {
    /// Bind `spec` to the runtime paths, in the [`ReconcileContext::Apply`]
    /// context.
    pub fn new(
        spec: &'a BackupSpec,
        config_dir: &'a Path,
        profile_name: &'a str,
        state_dir: &'a Path,
    ) -> Self {
        Self {
            spec,
            config_dir,
            profile_name,
            state_dir,
            context: ReconcileContext::Apply,
            abort: None,
        }
    }

    /// Run under [`ReconcileContext::Reconcile`] instead — the daemon path.
    /// Only affects `$CFGD_CONTEXT` inside the hooks.
    pub fn with_context(mut self, context: ReconcileContext) -> Self {
        self.context = context;
        self
    }

    /// Let SIGINT/SIGTERM cooperatively kill an in-flight hook.
    ///
    /// A `preBackup` that stops a service can run for minutes; without the flag
    /// the executor has no way to learn the operator asked to stop, and Ctrl-C
    /// is ignored until the hook's own timeout fires.
    pub fn with_abort(mut self, abort: &'a crate::AbortFlag) -> Self {
        self.abort = Some(abort);
        self
    }

    /// The spec this unit runs.
    pub fn spec(&self) -> &BackupSpec {
        self.spec
    }

    /// The source path, with a leading `~` expanded.
    pub fn source(&self) -> PathBuf {
        crate::expand_tilde(&self.spec.source)
    }

    /// The directory snapshots are written into: `spec.destination` when set,
    /// otherwise `<state_dir>/backups/<name>/`. The default is derived here
    /// rather than at parse time because the state dir depends on runtime
    /// scope and `CFGD_STATE_DIR`.
    pub fn destination_dir(&self) -> PathBuf {
        match &self.spec.destination {
            Some(dir) => crate::expand_tilde(dir),
            None => self.state_dir.join("backups").join(&self.spec.name),
        }
    }
}

/// A snapshot that landed on disk.
struct Artifact {
    path: PathBuf,
    size_bytes: u64,
}

/// One rendered line of a backup unit's run, in the order it was emitted.
///
/// NOT persisted — `backup_runs` keeps its columns; this is what the
/// pseudo-phase's rollup counts and what the run's `RunTally` is built from.
/// A [`BackupRunRecord`] is one row per *unit* and carries no hook counts, so
/// a rollup built from records alone would report one action for a unit that
/// rendered three lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupItem {
    pub label: String,
    pub ok: bool,
}

/// What one unit produced: the row that was written (absent only on the two
/// record-less arms — a `Busy` skip and a state-store failure) and the items
/// that were rendered under it.
#[derive(Debug, Default)]
pub struct BackupRunReport {
    pub record: Option<BackupRunRecord>,
    pub items: Vec<BackupItem>,
    /// Set on the `Busy` skip, so the caller's `-o json` payload keeps the
    /// `"skipped"` status it emits today.
    pub skipped: Option<String>,
    /// Set on the OTHER record-less arm — the state store refused the row — so
    /// the payload can name the failure. Without it the one arm that has no
    /// record would also be the one arm whose `-o json` entry could not say
    /// what went wrong.
    pub error: Option<String>,
}

impl BackupRunReport {
    /// Whether this unit's work completed with an intact snapshot. A skip is
    /// not clean: the unit the caller asked for did not run here.
    pub fn is_clean(&self) -> bool {
        self.skipped.is_none() && self.record.as_ref().is_some_and(BackupRunRecord::is_clean)
    }
}

/// Which side of the backup a hook list is running for.
///
/// Reaches `preBackup` / `postBackup` hooks as `CFGD_OPERATION`. The two
/// operations share one hook list — the unit declares it once — so a hook that
/// must behave differently when data is coming *back* has nothing else to
/// branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupOperation {
    /// A snapshot is being taken (`cfgd backup run`, apply, the daemon timer,
    /// and the safety snapshot a restore takes).
    Backup,
    /// A snapshot is being put back (`cfgd backup restore`).
    Restore,
}

impl BackupOperation {
    /// The `CFGD_OPERATION` value hooks see.
    pub fn display_name(self) -> &'static str {
        match self {
            BackupOperation::Backup => "backup",
            BackupOperation::Restore => "restore",
        }
    }
}

/// Run one backup unit end to end and record the outcome.
///
/// # Failure semantics
///
/// An **operational** failure is reported through the returned record, not
/// through `Err`: the caller needs the run's id and destination either way, and
/// a `Err` would throw both away. `Err` is reserved for failures that prevent
/// the run from being *recorded at all* (a state-DB error) — at that point
/// there is no record to return.
///
/// - A `preBackup` hook failure aborts the **snapshot**: no copy is attempted
///   and the run is recorded `Failed` with no artifact. It does not abort the
///   unit — see the next rule.
/// - `postBackup` hooks are attempted on **every** path: after a good copy,
///   after a failed copy, and after a failed `preBackup`. They are normally the
///   counterpart that restarts whatever `preBackup` stopped, and a hook list
///   that failed halfway (service already down, flush failed) is exactly when
///   leaving them unrun strands the machine.
/// - A `postBackup` failure *after a good copy* leaves the run `Success` with
///   [`BackupRunRecord::error`] set. The snapshot is complete and restorable,
///   so it must stay retention-eligible; marking the run `Failed` would strand
///   a valid artifact that pruning can never reclaim. Callers gate their exit
///   code on [`BackupRunRecord::is_clean`], which is false for such a run.
///
/// Every failure of a run is joined into `error` with `; ` — a pre-hook, copy,
/// and post-hook failure in one run all reach the record.
///
/// Retention pruning runs after the record is written, so the run just taken
/// counts toward `spec.retention`.
///
/// # Serialization
///
/// The unit's lock (`<state_dir>/locks/backup-<name>.lock`) is held for the
/// whole run, hooks included. One unit therefore has exactly one writer no
/// matter which surface dispatched it — a scheduled fire that lands on top of a
/// `cfgd backup run` gets [`BackupError::Busy`] rather than a second staging
/// tree in the same directory. Different units are unaffected: each takes its
/// own lock and they still run concurrently.
pub fn run_backup(
    unit: &BackupUnit<'_>,
    store: &StateStore,
    printer: &Printer,
    items: &mut Vec<BackupItem>,
) -> Result<BackupRunRecord> {
    let _lock = acquire_unit_lock(unit)?;
    let spec = unit.spec;
    let started_at = crate::utc_now_iso8601();
    let source = unit.source();

    let pre_error = run_hooks(
        unit,
        &spec.pre_backup,
        ScriptPhase::PreBackup,
        BackupOperation::Backup,
        printer,
        items,
    )
    .err();
    // A failed `preBackup` means the source is not in the state the hook was
    // meant to put it in, so a snapshot of it would be untrustworthy.
    let copy = match pre_error {
        Some(_) => None,
        None => Some(take_snapshot(unit, &source)),
    };
    // No snapshot item here: the snapshot's LINE is rendered after this
    // function returns — by `report_backup_record` on a recorded run, or by
    // `run_backup_group`'s failure arm when the row could not be written at
    // all — and the rollup counts lines, not intentions. Minting the item at
    // the site that emits the line is what keeps the two equal on the arm
    // where this function returns `Err` after the copy already succeeded.
    let post_error = run_hooks(
        unit,
        &spec.post_backup,
        ScriptPhase::PostBackup,
        BackupOperation::Backup,
        printer,
        items,
    )
    .err();

    record_run(
        store,
        unit,
        printer,
        &source,
        started_at,
        RunOutcome {
            pre_error,
            copy,
            post_error,
        },
    )
}

/// What the three mutating steps of a run produced, before any of it is
/// collapsed into a record. `copy` is `None` when the snapshot was skipped
/// outright rather than attempted and failed.
struct RunOutcome {
    pre_error: Option<BackupError>,
    copy: Option<std::result::Result<Artifact, BackupError>>,
    post_error: Option<BackupError>,
}

/// Take the snapshot, record the run, and prune — with **no hooks**, and with
/// the unit's lock already held by the caller.
///
/// The safety backup [`restore::restore_backup`] takes needs exactly this half
/// of [`run_backup`]: an ordinary row and an ordinary retention prune, inside
/// the lock and inside the hook envelope the restore already holds open.
/// Re-entering [`run_backup`] would do neither — the `flock` is per open file
/// description, so the nested acquire would report the restore as the holder of
/// the unit it is restoring, and the unit's hooks would run a second time around
/// a source the restore has already quiesced.
fn snapshot_and_record(
    unit: &BackupUnit<'_>,
    store: &StateStore,
    printer: &Printer,
) -> Result<BackupRunRecord> {
    let started_at = crate::utc_now_iso8601();
    let source = unit.source();
    let copy = Some(take_snapshot(unit, &source));
    // No items out-parameter: a restore's safety snapshot renders no owner
    // group, so there is no pseudo-phase rollup to count its lines.
    record_run(
        store,
        unit,
        printer,
        &source,
        started_at,
        RunOutcome {
            pre_error: None,
            copy,
            post_error: None,
        },
    )
}

/// The snapshot line's subject: `snapshot <destination file name>`, or the bare
/// `snapshot` when nothing was captured.
///
/// The file-name COMPONENT, never the whole path: `namePattern` may nest
/// (`daily/{filename}.{timestamp}`), and the file name is also what `cfgd
/// backup restore --at` asks the operator to type. The ONE derivation, shared
/// by the rendered line, by the item the rollup counts, and — through
/// [`predicted_snapshot_subject`] — by the alignment column the pseudo-phase
/// sizes before the first hook runs.
fn snapshot_subject(destination: Option<&Path>) -> String {
    match destination.and_then(|p| p.file_name()) {
        Some(name) => format!("snapshot {}", name.to_string_lossy()),
        None => "snapshot".to_string(),
    }
}

/// The width the snapshot line will occupy, derived before the run.
///
/// `namePattern`, the unit name and the source's file name all come from the
/// spec, and `{timestamp}` is fixed-width (`BACKUP_TIMESTAMP_FORMAT` renders 16
/// characters for every instant), so the subject is exact for every run that
/// takes the name it rendered. The `{filename}` fallback is [`snapshot_name`]'s
/// own — the unit's name when the source has no final component — because the
/// empty string a `file_name()`-only derivation would substitute is narrower
/// than what the run will print, and narrower is the direction that mis-aligns
/// the column.
///
/// **The one exception is a [`uniquify`] collision.** When the rendered name is
/// already taken — two runs of one unit inside the same second, or a
/// `namePattern` carrying no `{timestamp}` — the run publishes `<name>-1`…
/// `<name>-N` instead, and that line's subject is 2 to 5 characters wider than
/// the column it was measured for, so its detail sits one step past the other
/// lines'. Deliberately not budgeted for: reserving the widest suffix would
/// widen every group in every run for a name that is otherwise never printed,
/// and the collision announces itself in the name. Whether the collision fires
/// depends on what is on disk when the copy runs, which is after the column has
/// to exist, so no prediction can be exact here.
fn predicted_snapshot_subject(unit: &BackupUnit<'_>) -> String {
    let spec = unit.spec;
    let source = unit.source();
    let filename = source
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec.name.clone());
    let rendered = crate::config::render_backup_name_pattern(
        &spec.name_pattern,
        &spec.name,
        &filename,
        // Any instant renders the same width, so the value is irrelevant and
        // only its shape matters.
        &crate::utc_now_backup_stamp(),
    );
    snapshot_subject(Some(Path::new(&rendered)))
}

/// The labels the `Backups` pseudo-phase's alignment column is measured over:
/// one per hook entry, plus the unit's predicted snapshot line.
///
/// The same strings the run will emit — the hook subjects come from
/// [`hook_script_subject`](crate::reconciler::hook_script_subject), which is
/// also what `ScriptStatus::new` composes each hook's line from, so the column
/// cannot be sized against a label no line reaches.
pub fn backup_unit_labels(unit: &BackupUnit<'_>) -> Vec<String> {
    let spec = unit.spec;
    let mut labels: Vec<String> = Vec::new();
    for (phase, entries) in [
        (ScriptPhase::PreBackup, &spec.pre_backup),
        (ScriptPhase::PostBackup, &spec.post_backup),
    ] {
        for entry in entries {
            labels.push(hook_item_label(&phase, entry));
        }
    }
    labels.push(predicted_snapshot_subject(unit));
    labels
}

/// One hook's line, as both the alignment column and the rollup's item see it.
fn hook_item_label(phase: &ScriptPhase, entry: &ScriptEntry) -> String {
    crate::reconciler::hook_script_subject(phase.display_name(), entry.run_str()).to_string()
}

/// Render one unit's owner group and return what it produced.
///
/// The ONE backup renderer: `cfgd backup run`, apply's pending backups and the
/// daemon's scheduled fire all call it, so a unit cannot look different
/// depending on which surface dispatched it — the property
/// [`report_backup_record`] already carried, extended to the group and to the
/// items its rollup counts.
///
/// The two record-less arms render inside the group like every other row, with
/// the same `snapshot` subject the no-artifact row uses: naming the unit in the
/// group heading and again in the subject is the duplication the owner model
/// exists to remove. A `Busy` skip is `Role::Skipped` and moves no count — it
/// is the one-writer-per-unit rule working, not a failure — while a
/// state-store failure is `Role::Fail` and does.
pub fn run_backup_group(
    unit: &BackupUnit<'_>,
    store: &StateStore,
    phase: &crate::reconciler::PseudoPhase<'_>,
    width: usize,
    printer: &Printer,
) -> BackupRunReport {
    let group = phase.owner(&crate::reconciler::Owner::backup(&unit.spec.name), width);
    let mut report = BackupRunReport::default();
    match run_backup(unit, store, printer, &mut report.items) {
        Ok(record) => {
            report.items.push(report_backup_record(printer, &record));
            report.record = Some(record);
        }
        Err(crate::errors::CfgdError::Backup(BackupError::Busy { holder, .. })) => {
            group
                .status(Role::Skipped, snapshot_subject(None))
                .detail(format!("already running ({holder})"));
            report.skipped = Some(holder);
        }
        Err(e) => {
            let detail = collapse_to_subject_line(&e);
            group
                .status(Role::Fail, snapshot_subject(None))
                .detail(&detail);
            report.items.push(BackupItem {
                label: snapshot_subject(None),
                ok: false,
            });
            report.error = Some(detail);
        }
    }
    report
}

/// Join a run's failures, write its record, and prune to `spec.retention`.
///
/// Report a completed run on the status line every surface shares.
///
/// `is_clean()` is the exit-code predicate, and the human line uses the same
/// three-way split: a fully clean run is Ok, a Success run with a failed
/// `postBackup` hook is Warn (the snapshot is fine, but something needs
/// attention), and no artifact at all is Fail. `cfgd apply`, `cfgd backup run`,
/// and the daemon's scheduled fire all render through here so a run cannot look
/// different depending on which surface produced it.
///
/// **Size versus error in the one detail slot — the error wins, and the size
/// joins it.** Both can be present at once: `record_run` sets `Success`
/// whenever an artifact exists and still joins any hook failures into `error`,
/// so a `postBackup` failure after a good snapshot carries both. The error
/// leads because it is what the reader must act on; the size stays because it
/// is the evidence that the snapshot itself landed.
///
/// Returns the [`BackupItem`] for the line it just emitted, so the rollup counts
/// from the same value the screen was written from. A recorded run always
/// renders exactly one line here — the run whose `preBackup` hook aborted the
/// snapshot included, which reports `Fail` against a bare `snapshot` subject —
/// and the item is `ok` exactly when that line is not a failure.
#[must_use = "the item is what the rollup counts; push it onto the report"]
pub fn report_backup_record(printer: &Printer, record: &BackupRunRecord) -> BackupItem {
    let subject = snapshot_subject(record.destination_path.as_deref().map(Path::new));
    let role = if record.is_clean() {
        Role::Ok
    } else if record.status == BackupRunStatus::Success {
        Role::Warn
    } else {
        Role::Fail
    };
    let size = record.size_bytes.map(crate::format_bytes);
    let detail = match (&record.error, size) {
        (Some(e), Some(size)) => Some(format!("{} ({size})", collapse_to_subject_line(e))),
        (Some(e), None) => Some(collapse_to_subject_line(e)),
        (None, Some(size)) => Some(size),
        (None, None) => None,
    };
    match detail {
        Some(detail) => {
            printer.status(role, subject.as_str()).detail(detail);
        }
        None => printer.status_simple(role, subject.as_str()),
    }
    BackupItem {
        label: subject,
        ok: role != Role::Fail,
    }
}

/// The shared tail of every path that produces a `backup_runs` row, so the
/// hook-carrying [`run_backup`] and the hook-free [`snapshot_and_record`] can
/// never disagree about a run's recorded status, error joining, or pruning.
fn record_run(
    store: &StateStore,
    unit: &BackupUnit<'_>,
    printer: &Printer,
    source: &Path,
    started_at: String,
    outcome: RunOutcome,
) -> Result<BackupRunRecord> {
    let mut failures = Vec::new();
    if let Some(e) = outcome.pre_error {
        failures.push(collapse_to_subject_line(&e));
    }
    let artifact = match outcome.copy {
        Some(Ok(artifact)) => Some(artifact),
        Some(Err(e)) => {
            failures.push(collapse_to_subject_line(&e));
            None
        }
        None => None,
    };
    if let Some(e) = outcome.post_error {
        failures.push(collapse_to_subject_line(&e));
    }

    let status = match artifact {
        Some(_) => BackupRunStatus::Success,
        None => BackupRunStatus::Failed,
    };
    let error = (!failures.is_empty()).then(|| failures.join("; "));

    let draft = BackupRunDraft {
        name: unit.spec.name.clone(),
        source: crate::to_posix_string(source),
        destination_path: artifact
            .as_ref()
            .map(|a| crate::to_posix_string(a.path.as_path())),
        size_bytes: artifact.as_ref().map(|a| a.size_bytes),
        status,
        error,
        started_at,
        finished_at: crate::utc_now_iso8601(),
    };
    let record = store.record_backup_run(&draft)?;
    prune_retention(store, unit, printer);
    Ok(record)
}

/// Take the unit's exclusive lock, translating a held lock into the
/// backup-shaped [`BackupError::Busy`].
///
/// `ApplyLockHeld` is the generic primitive's error; surfacing it verbatim
/// would tell an operator their *apply* lock is held, which is a different
/// mutex with different advice.
fn acquire_unit_lock(unit: &BackupUnit<'_>) -> Result<crate::FileLockGuard> {
    crate::acquire_backup_lock(unit.state_dir, &unit.spec.name).map_err(|e| match e {
        crate::errors::CfgdError::State(crate::errors::StateError::ApplyLockHeld { holder }) => {
            BackupError::Busy {
                name: unit.spec.name.clone(),
                holder,
            }
            .into()
        }
        other => other,
    })
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// Run one hook list through the shared script executor.
///
/// `script_dir` is the config directory: `spec.backups[]` is profile-declared,
/// so a relative `run:` resolves against the config tree — the same anchoring
/// `PatchBinding::profile` uses for a profile-declared file. Entries honour
/// `continueOnError`, so a hook list can be told to press on past a failure;
/// the first failure is still what the caller sees.
fn run_hooks(
    unit: &BackupUnit<'_>,
    entries: &[ScriptEntry],
    phase: ScriptPhase,
    operation: BackupOperation,
    printer: &Printer,
    items: &mut Vec<BackupItem>,
) -> std::result::Result<(), BackupError> {
    if entries.is_empty() {
        return Ok(());
    }
    let path_dirs: Vec<String> = crate::bootstrapped_path_dirs()
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut env = build_script_env(&ScriptEnvContext {
        config_dir: unit.config_dir,
        profile_name: unit.profile_name,
        context: unit.context,
        phase: &phase,
        module_name: None,
        module_dir: None,
        path_dirs: &path_dirs,
    });
    // `CFGD_PHASE` names the hook list (`preBackup`/`postBackup`) and is the
    // same for both operations — the list is declared once and reused. A hook
    // that must quiesce for a snapshot but drop-and-recreate for a restore can
    // only branch if something distinguishes them, so the operation rides
    // alongside rather than overloading the phase.
    env.push((
        "CFGD_OPERATION".to_string(),
        operation.display_name().to_string(),
    ));
    let working_dir = script_default_workdir(unit.config_dir);
    let mut first_error: Option<BackupError> = None;

    for entry in entries {
        // Computed once, before the call: the value IS the arm's whole
        // meaning, and `execute_script` needs it to decide whether its single
        // line reads as fatal. Evaluating it again after the failure is what
        // let the role and the control flow disagree.
        let non_fatal = effective_continue_on_error(entry, &phase);
        let outcome = execute_script(
            entry,
            unit.config_dir,
            &working_dir,
            &env,
            crate::PROFILE_SCRIPT_TIMEOUT,
            printer,
            None,
            unit.abort,
            ScriptReport {
                subject: ScriptSubject::Hook(phase.display_name()),
                non_fatal,
            },
        );
        // One item per hook, pushed as it completes — the fatal one included,
        // because its line was rendered before this function returned.
        items.push(BackupItem {
            label: hook_item_label(&phase, entry),
            ok: outcome.is_ok(),
        });
        if let Err(e) = outcome {
            let failure = BackupError::HookFailed {
                name: unit.spec.name.clone(),
                phase: phase.display_name(),
                message: collapse_to_subject_line(&e),
            };
            if !non_fatal {
                return Err(failure);
            }
            // No second line: `execute_script` has already emitted this hook's
            // one status, at `Role::Warn` because `non_fatal` rode in on the
            // report. A softer re-render here would make one hook two lines
            // and repeat what the owner heading now says.
            first_error.get_or_insert(failure);
        }
    }

    first_error.map_or(Ok(()), Err)
}

// ---------------------------------------------------------------------------
// Copy
// ---------------------------------------------------------------------------

/// Copy `source` into the destination directory under its rendered name.
fn take_snapshot(
    unit: &BackupUnit<'_>,
    source: &Path,
) -> std::result::Result<Artifact, BackupError> {
    let spec = unit.spec;
    let meta = match std::fs::metadata(source) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(BackupError::SourceMissing {
                name: spec.name.clone(),
                path: source.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(BackupError::SourceUnreadable {
                name: spec.name.clone(),
                path: source.to_path_buf(),
                source: e,
            });
        }
    };

    let destination = unit.destination_dir();
    // A destination under the source turns the copy into a tree that grows into
    // itself: the walk finds its own output and descends forever. Both operands
    // are symlink-resolved, because a `destination` that is lexically disjoint
    // from the source (`~/link/backups` where `~/link` -> `~/Pictures`) is still
    // physically inside it. Checked before any filesystem write, so a
    // misconfiguration costs nothing.
    let real_source = resolve_for_containment(source);
    if is_at_or_within(&resolve_for_containment(&destination), &real_source) {
        return Err(BackupError::DestinationInsideSource {
            name: spec.name.clone(),
            source_path: source.to_path_buf(),
            destination,
        });
    }

    let rendered = destination.join(snapshot_name(spec, source)?);
    // A snapshot path that is (or contains) the source would have the source
    // deleted by `remove_existing` on the way to publishing the snapshot, and
    // would later be deleted outright by retention pruning. Judged on the
    // rendered name, before uniquifying: a pattern that names the source is a
    // misconfiguration to report, not something to quietly sidestep by writing
    // `<source>-1` beside it on every run.
    if is_at_or_within(&real_source, &resolve_for_containment(&rendered)) {
        return Err(BackupError::SnapshotCollidesWithSource {
            name: spec.name.clone(),
            source_path: source.to_path_buf(),
            snapshot: rendered,
        });
    }
    // Only ever yields a path nothing occupies, so it can never select the
    // source either — the source exists by the time this runs.
    let target = uniquify(&spec.name, rendered)?;

    let copy_failed = |e: std::io::Error| BackupError::CopyFailed {
        name: spec.name.clone(),
        path: target.clone(),
        source: e,
    };

    crate::ensure_parent_dir(&target).map_err(copy_failed)?;
    // The default destination lives under the state dir and holds copies of
    // whatever the user pointed at — possibly `~/.ssh`. Owner-only keeps a
    // sensitive snapshot from being readable through a 0755 parent even though
    // the snapshot itself carries the source's mode. An explicit `destination:`
    // is the user's own directory and keeps their umask.
    if spec.destination.is_none() {
        restrict_to_owner(&destination);
    }

    let size_bytes = if meta.is_dir() {
        copy_dir_snapshot(source, &target).map_err(copy_failed)?
    } else {
        copy_file_snapshot(source, &meta, &target).map_err(copy_failed)?
    };

    Ok(Artifact {
        path: target,
        size_bytes,
    })
}

/// How many `-N` suffixes [`uniquify`] will try before giving up.
const SNAPSHOT_NAME_ATTEMPTS: u32 = 1000;

/// The first free path in the `<name>`, `<name>-1`, `<name>-2`, … series.
///
/// `{timestamp}` renders at one-second resolution, so two runs of one unit
/// inside the same second render the same name. Publishing both there would
/// leave two `backup_runs` rows pointing at a single payload: the snapshot lists
/// twice, and when the older row falls out of retention the prune deletes the
/// payload the *newer* row still claims — the surviving row then names a
/// snapshot that is gone. One payload per row is what makes retention and
/// restore agree, so a collision takes a new name instead of overwriting.
fn uniquify(name: &str, target: PathBuf) -> std::result::Result<PathBuf, BackupError> {
    if target.symlink_metadata().is_err() {
        return Ok(target);
    }
    // A path with no final component cannot be suffixed; `snapshot_name` already
    // rejects the patterns that produce one, so this is unreachable in practice.
    let Some(stem) = target.file_name().map(|n| n.to_owned()) else {
        return Ok(target);
    };
    for attempt in 1..=SNAPSHOT_NAME_ATTEMPTS {
        let mut candidate = stem.clone();
        candidate.push(format!("-{attempt}"));
        let path = target.with_file_name(&candidate);
        if path.symlink_metadata().is_err() {
            return Ok(path);
        }
    }
    Err(BackupError::CopyFailed {
        name: name.to_string(),
        path: target,
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("no free snapshot name after {SNAPSHOT_NAME_ATTEMPTS} attempts"),
        ),
    })
}

/// Posix-folded view of a path, so containment comparisons behave identically
/// on Windows (`\`) and Unix (`/`) and against the posix-folded strings the
/// state store holds.
fn posix_path(path: &Path) -> PathBuf {
    PathBuf::from(crate::to_posix_string(path))
}

/// Whether `path` is `root` or a descendant of it, judged lexically.
///
/// Lexical rather than `canonicalize`: the paths compared here routinely do not
/// exist yet (a snapshot target) or no longer exist (a pruning candidate), and
/// `canonicalize` fails on both. Callers that must also catch containment
/// established through a symlink resolve their operands with
/// [`resolve_for_containment`] first.
fn is_at_or_within(path: &Path, root: &Path) -> bool {
    posix_path(path).starts_with(posix_path(root))
}

/// A path with its existing portion resolved through symlinks, so two paths that
/// are lexically disjoint but physically nested compare as nested.
///
/// `canonicalize` alone cannot be used: a destination directory is routinely
/// created by the very run being guarded, and a snapshot target never exists
/// yet. The deepest ancestor that *does* exist is resolved and the missing tail
/// re-appended, which is enough — a path cannot be moved into the source tree by
/// components that do not exist.
fn resolve_for_containment(path: &Path) -> PathBuf {
    let mut missing_tail = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(real) = cursor.canonicalize() {
            let mut resolved =
                PathBuf::from(crate::strip_windows_verbatim(&crate::to_posix_string(real)));
            for segment in missing_tail.iter().rev() {
                resolved.push(segment);
            }
            return resolved;
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) if parent != cursor => {
                missing_tail.push(name.to_owned());
                cursor = parent;
            }
            // A root, or a relative path whose first component does not exist:
            // there is nothing left to resolve, so the lexical form is the
            // best available answer.
            _ => return posix_path(path),
        }
    }
}

/// Whether `path` is a snapshot **inside** `destination`: strictly below it, and
/// reached only through plain name components.
///
/// The gate before any pruning delete. A row naming the destination itself, a
/// path outside it, or a path that walks through `.`/`..` fails — so a stale
/// row (the user changed `destination:`), a same-named backup from another
/// profile, or a hand-edited `state.db` can never make pruning delete something
/// this unit did not write.
fn is_snapshot_within(path: &Path, destination: &Path) -> bool {
    let folded = posix_path(path);
    let Ok(rest) = folded.strip_prefix(posix_path(destination)) else {
        return false;
    };
    let mut components = rest.components().peekable();
    if components.peek().is_none() {
        return false;
    }
    components.all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Best-effort `0700` on a directory cfgd owns. No-op on Windows, and a failure
/// is logged rather than raised — the snapshot itself is unaffected.
fn restrict_to_owner(dir: &Path) {
    if let Err(e) = crate::set_file_permissions(dir, 0o700) {
        tracing::warn!(
            dir = %dir.posix(),
            error = %e,
            "backup: could not restrict the default destination to owner-only",
        );
    }
}

/// Render `namePattern` into the snapshot's path component(s).
///
/// A pattern may legitimately contain a literal `/` (nesting snapshots under
/// the destination), so the rendered value is validated as a relative path
/// rather than a single filename — but it must stay inside the destination and
/// must not be empty.
fn snapshot_name(spec: &BackupSpec, source: &Path) -> std::result::Result<PathBuf, BackupError> {
    let filename = source
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        // A source with no final component (`/`, or a bare drive root) has no
        // filename to borrow; the backup's own name is the only stable label.
        .unwrap_or_else(|| spec.name.clone());
    let rendered = render_backup_name_pattern(
        &spec.name_pattern,
        &spec.name,
        &filename,
        &crate::utc_now_backup_stamp(),
    );

    // The filename rides along on every rejection because the default pattern
    // interpolates it: a source named `notes:2026.md` is a legal unix filename
    // that renders a name the plain-name rule refuses, and without this the
    // error reads as a complaint about drive letters in a pattern the user
    // never wrote.
    let invalid = |message: String| BackupError::InvalidSnapshotName {
        name: spec.name.clone(),
        rendered: rendered.clone(),
        source_filename: filename.clone(),
        message,
    };
    if rendered.trim().is_empty() {
        return Err(invalid("it is empty".to_string()));
    }
    // Rootedness is left to `validate_plain_name`, which judges it from the
    // path's components on every host. `Path::is_absolute` does not: on Windows
    // `/etc/passwd` is drive-relative rather than absolute, so a pre-check here
    // would reject the same pattern with a different message depending on which
    // OS parsed it.
    crate::validate_plain_name(&rendered).map_err(invalid)?;
    Ok(PathBuf::from(&rendered))
}

/// Copy a file to `target` atomically: stream into a sibling temp file, fsync,
/// then rename over the destination and fsync the directory.
///
/// `meta` is the source metadata the caller already stat'd; re-reading it here
/// would both cost a syscall and widen the window in which the source's mode
/// could change between the check and the copy.
fn copy_file_snapshot(
    source: &Path,
    meta: &std::fs::Metadata,
    target: &Path,
) -> std::io::Result<u64> {
    let parent = target.parent().unwrap_or(Path::new("."));
    let mut input = std::fs::File::open(source)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    let size = std::io::copy(&mut input, tmp.as_file_mut())?;
    tmp.as_file().sync_all()?;
    if let Err(e) = tmp.as_file().set_permissions(meta.permissions()) {
        tracing::warn!(
            target = %target.posix(),
            error = %e,
            "backup: failed to copy source permissions onto the snapshot",
        );
    }
    remove_existing(target)?;
    tmp.persist(target).map_err(|e| e.error)?;
    sync_dir(parent);
    Ok(size)
}

/// fsync a directory so a just-completed rename survives a power loss.
///
/// Best-effort: the snapshot's bytes are already durable, and a filesystem that
/// refuses to open a directory (Windows, some network mounts) simply keeps the
/// weaker ordering guarantee its rename already provides.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        tracing::debug!(
            dir = %dir.posix(),
            error = %e,
            "backup: could not fsync the destination directory after rename",
        );
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Copy a directory tree to `target`, publishing it with a single rename.
///
/// The tree is built under a sibling `.<name>.partial` staging directory so an
/// interrupted copy never leaves a half-populated snapshot under the name a
/// restore would trust. Symlinks are skipped by
/// [`crate::copy_dir_recursive`] — a snapshot must not follow a link out of the
/// source tree.
fn copy_dir_snapshot(source: &Path, target: &Path) -> std::io::Result<u64> {
    let staging = staging_path(target);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    if let Err(e) = crate::copy_dir_recursive(source, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    if let Err(e) = remove_existing(target) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&staging, target) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    if let Some(parent) = target.parent() {
        sync_dir(parent);
    }
    crate::dir_size(target)
}

/// Sibling staging path for a directory snapshot. Dot-prefixed so it sorts and
/// globs away from real snapshots if a crash strands it.
fn staging_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "snapshot".to_string());
    parent.join(format!(".{name}.partial"))
}

/// Delete whatever currently occupies `path`, if anything.
///
/// A rename over an existing entry is not portable (Windows refuses, and no
/// platform renames a directory onto a non-empty one), so the destination is
/// cleared first. [`uniquify`] has already picked a free name, so this normally
/// finds nothing; it still runs because the name was checked before the copy and
/// something outside cfgd could have taken it in between.
///
/// The metadata is read **unfollowed**, so a symlink occupying the path is
/// deleted itself rather than having its target truncated — the property the
/// restore overlay depends on to never write outside its target.
fn remove_existing(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        // Windows reports a directory symlink or junction as a symlink, not a
        // directory, and `remove_file` refuses to unlink one.
        #[cfg(windows)]
        Ok(meta) if meta.is_symlink() && path.is_dir() => std::fs::remove_dir(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// Enforce `spec.retention` from the recorded runs, deleting pruned artifacts
/// from disk before their rows.
///
/// Retention is counted **per outcome class**: the newest N runs that produced
/// a snapshot are kept, and independently the newest N that did not. Counting
/// both classes together would let a burst of failures evict — and delete —
/// every good snapshot, which is the one thing a backup feature must never do.
/// Bounding the failure rows too keeps a persistently broken backup from
/// growing the table without limit.
///
/// Never fails the run: the snapshot is already safely on disk, and a pruning
/// problem (a busy file, a permission change) must not turn a good backup into
/// a reported failure. A row whose artifact could not be deleted is left in
/// place so the next run retries it.
///
/// Nothing is deleted from disk unless [`is_snapshot_within`] confirms the
/// recorded path is inside *this unit's* destination. A row that fails that gate
/// is dropped from the table without a single filesystem call, so a stale or
/// foreign row bounds the table instead of aiming a recursive delete.
fn prune_retention(store: &StateStore, unit: &BackupUnit<'_>, printer: &Printer) {
    let spec = unit.spec;
    let destination = unit.destination_dir();
    let runs = match store.backup_runs(&spec.name) {
        Ok(runs) => runs,
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "backup '{}': retention prune skipped — could not read run history: {}",
                    spec.name,
                    collapse_to_subject_line(&e)
                ),
            );
            return;
        }
    };

    let warn = |message: String| printer.status_simple(Role::Warn, message);
    let drop_row = |id: i64| {
        if let Err(e) = store.delete_backup_run(id) {
            warn(format!(
                "backup '{}': could not delete run record {id}: {}",
                spec.name,
                collapse_to_subject_line(&e)
            ));
        }
    };

    let keep = spec.retention as usize;
    let mut kept_artifacts = 0;
    let mut kept_failures = 0;
    // `backup_runs` is newest-first, so the first `keep` of each class survive.
    for run in &runs {
        // Containment first, ahead of the retention accounting: a foreign row
        // must neither reach a delete nor occupy a slot that a real snapshot
        // needs, or stale rows would crowd out the runs the user asked to keep.
        if let Some(path) = &run.destination_path
            && !is_snapshot_within(Path::new(path), &destination)
        {
            warn(format!(
                "backup '{}': run {} records a snapshot outside the destination {} ({path}); \
                 dropping the record and leaving the path untouched — delete it yourself if it is stale",
                spec.name,
                run.id,
                destination.posix(),
            ));
            drop_row(run.id);
            continue;
        }

        let counter = if run.has_artifact() {
            &mut kept_artifacts
        } else {
            &mut kept_failures
        };
        if *counter < keep {
            *counter += 1;
            continue;
        }

        if let Some(path) = &run.destination_path {
            let path = Path::new(path);
            if let Err(e) = remove_existing(path) {
                warn(format!(
                    "backup '{}': could not prune snapshot {}: {}",
                    spec.name,
                    path.posix(),
                    collapse_to_subject_line(&e)
                ));
                continue;
            }
            prune_empty_parents(path, &destination);
        }
        drop_row(run.id);
    }
}

/// Remove now-empty directories a nested `namePattern` left behind, walking up
/// to but never including `destination`.
///
/// `remove_dir` only succeeds on an empty directory, so a parent still holding
/// another snapshot stops the walk on its own — no emptiness check is needed,
/// and no non-empty directory can be removed by this path.
fn prune_empty_parents(snapshot: &Path, destination: &Path) {
    let mut current = snapshot.parent();
    while let Some(dir) = current {
        if !is_snapshot_within(dir, destination) || std::fs::remove_dir(dir).is_err() {
            return;
        }
        current = dir.parent();
    }
}
