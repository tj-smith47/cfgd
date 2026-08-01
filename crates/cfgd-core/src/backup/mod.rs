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
        }
    }

    /// Run under [`ReconcileContext::Reconcile`] instead — the daemon path.
    /// Only affects `$CFGD_CONTEXT` inside the hooks.
    pub fn with_context(mut self, context: ReconcileContext) -> Self {
        self.context = context;
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
/// - A `preBackup` hook failure aborts the unit. No copy is attempted, no
///   `postBackup` hook runs, and the run is recorded `Failed` with no artifact.
/// - `postBackup` hooks are attempted after the copy step **whether or not the
///   copy succeeded**, because they typically restart whatever `preBackup`
///   stopped; skipping them on a failed copy would leave the machine down.
/// - A `postBackup` failure *after a good copy* leaves the run `Success` with
///   [`BackupRunRecord::error`] set. The snapshot is complete and restorable,
///   so it must stay retention-eligible; marking the run `Failed` would strand
///   a valid artifact that pruning can never reclaim. Callers gate their exit
///   code on [`BackupRunRecord::is_clean`], which is false for such a run.
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

    if let Err(e) = run_hooks(unit, &spec.pre_backup, ScriptPhase::PreBackup, printer) {
        let record = finish(
            store,
            unit,
            &source,
            started_at,
            None,
            Some(collapse_to_subject_line(&e)),
            BackupRunStatus::Failed,
        )?;
        prune_retention(store, spec, printer);
        return Ok(record);
    }

    let copy_outcome = take_snapshot(unit, &source);

    // Always attempted: `postBackup` is the counterpart that restarts what
    // `preBackup` stopped, so a failed copy must not leave it unrun.
    let post_error = run_hooks(unit, &spec.post_backup, ScriptPhase::PostBackup, printer)
        .err()
        .map(|e| collapse_to_subject_line(&e));

    let (status, artifact, error) = match copy_outcome {
        Ok(artifact) => (BackupRunStatus::Success, Some(artifact), post_error),
        Err(copy_error) => {
            let mut message = collapse_to_subject_line(&copy_error);
            if let Some(post) = post_error {
                message.push_str("; ");
                message.push_str(&post);
            }
            (BackupRunStatus::Failed, None, Some(message))
        }
    };

    let record = finish(store, unit, &source, started_at, artifact, error, status)?;
    prune_retention(store, spec, printer);
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
/// `ModifyBinding::profile` uses for a profile-declared file. Entries honour
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
            None,
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

    let target = unit.destination_dir().join(snapshot_name(spec, source)?);
    let copy_failed = |e: std::io::Error| BackupError::CopyFailed {
        name: spec.name.clone(),
        path: target.clone(),
        source: e,
    };

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(copy_failed)?;
    }

    let size_bytes = if meta.is_dir() {
        copy_dir_snapshot(source, &target).map_err(copy_failed)?
    } else {
        copy_file_snapshot(source, &target).map_err(copy_failed)?
    };

    Ok(Artifact {
        path: target,
        size_bytes,
    })
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

    let invalid = |message: String| BackupError::InvalidSnapshotName {
        name: spec.name.clone(),
        rendered: rendered.clone(),
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
    crate::validate_no_traversal(&path).map_err(invalid)?;
    Ok(path)
}

/// Copy a file to `target` atomically: stream into a sibling temp file, fsync,
/// then rename over the destination.
fn copy_file_snapshot(source: &Path, target: &Path) -> std::io::Result<u64> {
    let parent = target.parent().unwrap_or(Path::new("."));
    let mut input = std::fs::File::open(source)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    let size = std::io::copy(&mut input, tmp.as_file_mut())?;
    tmp.as_file().sync_all()?;
    if let Ok(meta) = std::fs::metadata(source)
        && let Err(e) = tmp.as_file().set_permissions(meta.permissions())
    {
        tracing::warn!(
            target = %target.posix(),
            error = %e,
            "backup: failed to copy source permissions onto the snapshot",
        );
    }
    remove_existing(target)?;
    tmp.persist(target).map_err(|e| e.error)?;
    Ok(size)
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
fn prune_retention(store: &StateStore, spec: &BackupSpec, printer: &Printer) {
    let runs = match store.backup_runs(&spec.name) {
        Ok(runs) => runs,
        Err(e) => {
            tracing::warn!(
                backup = %spec.name,
                error = %e,
                "backup: retention prune skipped — could not read run history",
            );
            return;
        }
    };

    let keep = spec.retention as usize;
    let mut kept_artifacts = 0;
    let mut kept_failures = 0;
    // `backup_runs` is newest-first, so the first `keep` of each class survive.
    for run in &runs {
        let counter = if run.has_artifact() {
            &mut kept_artifacts
        } else {
            &mut kept_failures
        };
        if *counter < keep {
            *counter += 1;
            continue;
        }
        if let Some(path) = &run.destination_path
            && let Err(e) = remove_existing(Path::new(path))
        {
            printer.status_simple(
                Role::Warn,
                format!(
                    "backup '{}': could not prune snapshot {path}: {}",
                    spec.name,
                    collapse_to_subject_line(&e)
                ),
            );
            continue;
        }
        if let Err(e) = store.delete_backup_run(run.id) {
            tracing::warn!(
                backup = %spec.name,
                run_id = run.id,
                error = %e,
                "backup: pruned snapshot but could not delete its run record",
            );
        }
    }
}
