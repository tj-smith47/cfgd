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
    ReconcileContext, ScriptPhase, build_script_env, effective_continue_on_error, execute_script,
    script_default_workdir,
};
use crate::state::{BackupRunDraft, BackupRunRecord, BackupRunStatus, StateStore};

#[cfg(test)]
mod tests;

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
pub fn run_backup(
    unit: &BackupUnit<'_>,
    store: &StateStore,
    printer: &Printer,
) -> Result<BackupRunRecord> {
    let spec = unit.spec;
    let started_at = crate::utc_now_iso8601();
    let source = unit.source();

    let pre_error = run_hooks(unit, &spec.pre_backup, ScriptPhase::PreBackup, printer).err();
    // A failed `preBackup` means the source is not in the state the hook was
    // meant to put it in, so a snapshot of it would be untrustworthy.
    let copy_outcome = match pre_error {
        Some(_) => None,
        None => Some(take_snapshot(unit, &source)),
    };
    let post_error = run_hooks(unit, &spec.post_backup, ScriptPhase::PostBackup, printer).err();

    let mut failures = Vec::new();
    if let Some(e) = pre_error {
        failures.push(collapse_to_subject_line(&e));
    }
    let artifact = match copy_outcome {
        Some(Ok(artifact)) => Some(artifact),
        Some(Err(e)) => {
            failures.push(collapse_to_subject_line(&e));
            None
        }
        None => None,
    };
    if let Some(e) = post_error {
        failures.push(collapse_to_subject_line(&e));
    }

    let status = match artifact {
        Some(_) => BackupRunStatus::Success,
        None => BackupRunStatus::Failed,
    };
    let error = (!failures.is_empty()).then(|| failures.join("; "));

    let record = finish(store, unit, &source, started_at, artifact, error, status)?;
    prune_retention(store, unit, printer);
    Ok(record)
}

/// Write the run record. Split out so every exit path records the same shape.
fn finish(
    store: &StateStore,
    unit: &BackupUnit<'_>,
    source: &Path,
    started_at: String,
    artifact: Option<Artifact>,
    error: Option<String>,
    status: BackupRunStatus,
) -> Result<BackupRunRecord> {
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
    store.record_backup_run(&draft)
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
    printer: &Printer,
) -> std::result::Result<(), BackupError> {
    if entries.is_empty() {
        return Ok(());
    }
    let env = build_script_env(
        unit.config_dir,
        unit.profile_name,
        unit.context,
        &phase,
        None,
        None,
    );
    let working_dir = script_default_workdir(unit.config_dir);
    let mut first_error: Option<BackupError> = None;

    for entry in entries {
        let outcome = execute_script(
            entry,
            unit.config_dir,
            &working_dir,
            &env,
            crate::PROFILE_SCRIPT_TIMEOUT,
            printer,
            None,
            unit.abort,
        );
        if let Err(e) = outcome {
            let failure = BackupError::HookFailed {
                name: unit.spec.name.clone(),
                phase: phase.display_name(),
                message: collapse_to_subject_line(&e),
            };
            if !effective_continue_on_error(entry, &phase) {
                return Err(failure);
            }
            printer.status_simple(Role::Warn, collapse_to_subject_line(&failure));
            first_error.get_or_insert(failure);
        }
    }

    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
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

    let target = destination.join(snapshot_name(spec, source)?);
    // A snapshot path that is (or contains) the source would have the source
    // deleted by `remove_existing` on the way to publishing the snapshot, and
    // would later be deleted outright by retention pruning.
    if is_at_or_within(&real_source, &resolve_for_containment(&target)) {
        return Err(BackupError::SnapshotCollidesWithSource {
            name: spec.name.clone(),
            source_path: source.to_path_buf(),
            snapshot: target,
        });
    }

    let copy_failed = |e: std::io::Error| BackupError::CopyFailed {
        name: spec.name.clone(),
        path: target.clone(),
        source: e,
    };

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(copy_failed)?;
    }
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::warn!(
                dir = %dir.posix(),
                error = %e,
                "backup: could not restrict the default destination to owner-only",
            );
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
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
    let path = PathBuf::from(&rendered);
    if path.is_absolute() {
        return Err(invalid(
            "it is absolute; namePattern is relative to the destination".to_string(),
        ));
    }
    crate::validate_plain_name(&rendered).map_err(invalid)?;
    Ok(path)
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
/// cleared first. Only reachable when two runs of one backup render the same
/// name — same second, same pattern.
fn remove_existing(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
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
