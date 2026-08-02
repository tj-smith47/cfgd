//! Restore a snapshot [`run_backup`](super::run_backup) took back over the
//! unit it was taken from.
//!
//! The engine's two halves are deliberately split: [`list_snapshots`] +
//! [`select_snapshot`] are pure reads a caller runs BEFORE prompting, and
//! [`restore_backup`] is the mutation it runs after. A confirmation prompt has
//! to name the snapshot and the target it is about to overwrite, and resolving
//! them twice would let the answer describe a different snapshot than the one
//! restored.
//!
//! Restores are not recorded in the state DB. The `backup_runs` table is the
//! ledger retention pruning walks, and a restore produces no artifact for it to
//! prune — the safety backup a restore-to-source takes IS recorded, as an
//! ordinary run.

use std::path::{Path, PathBuf};

use crate::errors::{BackupError, Result};
use crate::output::{Printer, collapse_to_subject_line};
use crate::reconciler::ScriptPhase;
use crate::state::StateStore;

use super::BackupUnit;

/// One snapshot on disk, as `cfgd backup list <name> --snapshots` lists it and
/// `cfgd backup restore --at` selects it.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    /// The snapshot's path relative to the unit's destination, posix-folded.
    /// This is the name `--at` matches and the table renders — a nested
    /// `namePattern` therefore yields `daily/notes.txt.20260801T031500Z`.
    pub name: String,
    /// Absolute path to the snapshot payload.
    pub path: PathBuf,
    /// ISO 8601 UTC time the run that wrote it finished — the moment the
    /// snapshot became complete on disk, and the same clock
    /// `cfgd backup list`'s Last Run column reads.
    pub created: String,
    /// Size recorded for the snapshot when it was taken.
    pub size_bytes: u64,
}

/// What a [`restore_backup`] call did.
///
/// Shaped like a [`crate::state::BackupRunRecord`] rather than a bare
/// `Result<()>` for the same reason: an overlay that completed but left a
/// `postBackup` hook failing is neither a success the caller can ignore nor a
/// failure that undoes the restore, and only a record can say both.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    /// The unit that was restored.
    pub name: String,
    /// The snapshot it was restored from, as [`SnapshotInfo::name`].
    pub snapshot: String,
    /// Where the snapshot landed, posix-folded.
    pub restored_to: String,
    /// Whether the overlay actually ran and completed.
    pub restored: bool,
    /// Size recorded for the restored snapshot.
    pub size_bytes: u64,
    /// Path of the safety snapshot taken of the target's previous contents,
    /// posix-folded. `None` when `--to` redirected the restore (the live
    /// source was never touched) or the source did not exist yet.
    pub safety_snapshot: Option<String>,
    /// Every failure of the restore, joined with `; ` — the same shape
    /// [`crate::state::BackupRunRecord::error`] carries.
    pub error: Option<String>,
}

impl RestoreOutcome {
    /// Whether the overlay completed AND every hook succeeded — the predicate
    /// a caller gates its exit code on, matching
    /// [`crate::state::BackupRunRecord::is_clean`].
    pub fn is_clean(&self) -> bool {
        self.restored && self.error.is_none()
    }
}

/// Every restorable snapshot of `unit`, newest first.
///
/// Read from the run records, not a directory glob, so the list agrees with
/// what retention pruning walks. Two gates apply on top: a record whose path is
/// not demonstrably inside *this* unit's destination is ignored (the same
/// [`super::is_snapshot_within`] check pruning uses, so a stale or foreign row
/// can never be offered as a restore source), and a record whose payload is no
/// longer on disk is ignored too — a snapshot you cannot restore is not one.
pub fn list_snapshots(unit: &BackupUnit<'_>, store: &StateStore) -> Result<Vec<SnapshotInfo>> {
    let destination = unit.destination_dir();
    let folded_destination = super::posix_path(&destination);
    let runs = store.backup_runs(&unit.spec().name)?;

    let mut snapshots = Vec::new();
    for run in runs {
        let Some(raw) = run.destination_path.as_deref() else {
            continue;
        };
        let path = Path::new(raw);
        if !super::is_snapshot_within(path, &destination) {
            continue;
        }
        if path.symlink_metadata().is_err() {
            continue;
        }
        let folded = super::posix_path(path);
        let Ok(relative) = folded.strip_prefix(&folded_destination) else {
            continue;
        };
        snapshots.push(SnapshotInfo {
            name: crate::to_posix_string(relative),
            path: path.to_path_buf(),
            created: run.finished_at,
            size_bytes: run.size_bytes.unwrap_or(0),
        });
    }
    Ok(snapshots)
}

/// Pick the snapshot `--at` asks for out of a [`list_snapshots`] result.
///
/// `at = None` picks the newest. A value is matched as the full snapshot name
/// first, then as a fragment of one — so `--at 20260730T120000Z` reaches a
/// snapshot named `notes.txt.20260730T120000Z` without the caller knowing the
/// unit's `namePattern`. A fragment that matches more than one snapshot is
/// refused rather than resolved to the newest match: a restore overwrites live
/// data, so an ambiguous selection must be an error, never a guess.
pub fn select_snapshot<'s>(
    name: &str,
    snapshots: &'s [SnapshotInfo],
    at: Option<&str>,
) -> std::result::Result<&'s SnapshotInfo, BackupError> {
    let Some(newest) = snapshots.first() else {
        return Err(BackupError::NoSnapshots {
            name: name.to_string(),
        });
    };
    let Some(requested) = at.map(str::trim) else {
        return Ok(newest);
    };

    let not_found = || BackupError::SnapshotNotFound {
        name: name.to_string(),
        requested: requested.to_string(),
        available: snapshots.iter().map(|s| s.name.clone()).collect(),
    };
    // An empty `--at` would match every name as a fragment and report the
    // whole list as ambiguous, which reads as a cfgd bug rather than an
    // unusable argument.
    if requested.is_empty() {
        return Err(not_found());
    }
    if let Some(exact) = snapshots.iter().find(|s| s.name == requested) {
        return Ok(exact);
    }

    let matches: Vec<&SnapshotInfo> = snapshots
        .iter()
        .filter(|s| s.name.contains(requested))
        .collect();
    match matches.as_slice() {
        [] => Err(not_found()),
        [single] => Ok(single),
        many => Err(BackupError::AmbiguousSnapshot {
            name: name.to_string(),
            requested: requested.to_string(),
            matches: many.iter().map(|s| s.name.clone()).collect(),
        }),
    }
}

/// Restore `snapshot` over `unit`'s source, or over `to` when it is given.
///
/// # Sequence
///
/// The order is load-bearing:
///
/// 1. take the unit's lock, so no run of this unit can prune or rewrite
///    underneath the restore;
/// 2. stage the snapshot into a temp directory beside the target — **before**
///    the safety backup, because that backup prunes to `spec.retention` and the
///    snapshot being restored can be the one it evicts;
/// 3. take a safety backup of the target's current contents through the normal
///    [`super::run_backup`] path, so it is an ordinary run with an ordinary
///    record that ordinary retention prunes. Skipped when `to` redirects the
///    restore (the live source is untouched) or the source does not exist yet
///    (there is nothing to protect). A safety backup that produces no artifact
///    aborts the restore;
/// 4. `preBackup` hooks, the overlay, then `postBackup` hooks;
/// 5. staging is removed on every path, success or failure.
///
/// # Overlay semantics
///
/// A directory snapshot is overlaid, not mirrored: every file it holds
/// overwrites its counterpart, and files present only in the target are left
/// alone. Modes come across with the copy. Symlinks are absent by construction
/// — the backup writer skips them, so no snapshot contains one.
///
/// # Failure semantics
///
/// Mirrors [`super::run_backup`]: an operational failure is reported through
/// the returned [`RestoreOutcome`], and `Err` is reserved for failures that
/// stop the restore before it can begin (a held lock, a vanished snapshot, a
/// failed safety backup). A `preBackup` failure skips the overlay; `postBackup`
/// hooks run on every path.
pub fn restore_backup(
    unit: &BackupUnit<'_>,
    store: &StateStore,
    printer: &Printer,
    snapshot: &SnapshotInfo,
    to: Option<&Path>,
) -> Result<RestoreOutcome> {
    let spec = unit.spec();
    let name = spec.name.clone();
    let _lock = super::acquire_unit_lock(unit)?;

    let target = match to {
        Some(path) => crate::expand_tilde(path),
        None => unit.source(),
    };
    let destination = unit.destination_dir();
    if super::is_at_or_within(
        &super::resolve_for_containment(&target),
        &super::resolve_for_containment(&destination),
    ) {
        return Err(BackupError::RestoreTargetInsideDestination {
            name,
            target,
            destination,
        }
        .into());
    }

    let snapshot_kind = payload_kind(&name, &snapshot.path)?;
    check_target_kind(&name, &target, snapshot_kind)?;
    let staged = stage_snapshot(&name, snapshot, &target)?;

    let mut failures: Vec<String> = Vec::new();
    let safety = take_safety_backup(unit, store, printer, to, &mut failures)?;
    warn_if_safety_replaced_snapshot(printer, &name, snapshot, safety.as_deref());

    let pre_error = super::run_hooks(unit, &spec.pre_backup, ScriptPhase::PreBackup, printer).err();
    // A failed `preBackup` leaves the target in whatever state the hook stopped
    // in — a service still writing to the file being replaced, most of the
    // time. Overwriting it then is exactly what the hook existed to prevent.
    let overlay = match pre_error {
        Some(_) => None,
        None => Some(overlay_restore(&name, &staged.payload, &target)),
    };
    let post_error =
        super::run_hooks(unit, &spec.post_backup, ScriptPhase::PostBackup, printer).err();

    if let Some(e) = pre_error {
        failures.push(collapse_to_subject_line(&e));
    }
    let restored = match overlay {
        Some(Ok(())) => true,
        Some(Err(e)) => {
            failures.push(collapse_to_subject_line(&e));
            false
        }
        None => false,
    };
    if let Some(e) = post_error {
        failures.push(collapse_to_subject_line(&e));
    }

    Ok(RestoreOutcome {
        name,
        snapshot: snapshot.name.clone(),
        restored_to: crate::to_posix_string(&target),
        restored,
        size_bytes: snapshot.size_bytes,
        safety_snapshot: safety,
        error: (!failures.is_empty()).then(|| failures.join("; ")),
    })
}

/// Snapshot the target's current contents before it is overwritten, returning
/// the artifact's posix path.
///
/// `Ok(None)` means no safety backup was warranted: `to` redirected the restore
/// away from the live source, or the source does not exist yet (a bare-metal
/// restore has nothing to protect). Anything else that fails to produce an
/// artifact is an `Err` — the restore must not proceed over data that was not
/// captured.
///
/// A safety backup that *did* write its snapshot but hit a `postBackup` hook on
/// the way out is not fatal: the protection is on disk. Its failure is pushed
/// onto the restore's own failure list so the caller still reports an unclean
/// restore.
fn take_safety_backup(
    unit: &BackupUnit<'_>,
    store: &StateStore,
    printer: &Printer,
    to: Option<&Path>,
    failures: &mut Vec<String>,
) -> Result<Option<String>> {
    if to.is_some() || unit.source().symlink_metadata().is_err() {
        return Ok(None);
    }
    let record = super::run_backup_locked(unit, store, printer)?;
    let Some(path) = record.destination_path.clone() else {
        return Err(BackupError::SafetyBackupFailed {
            name: unit.spec().name.clone(),
            message: record
                .error
                .unwrap_or_else(|| "no snapshot was written".to_string()),
        }
        .into());
    };
    if let Some(e) = record.error {
        failures.push(e);
    }
    Ok(Some(path))
}

/// Say so when the safety backup rendered the same snapshot name as the one
/// being restored and therefore replaced it on disk.
///
/// `namePattern` stamps to the second, so a restore run inside the same second
/// as the snapshot it selects collides with it under the engine's documented
/// "newest wins" rule. The restore itself is unaffected — the payload was
/// staged before the safety backup ran, which is the whole reason staging comes
/// first — but the operator's snapshot store quietly loses an entry, and silence
/// there is worse than a warning.
fn warn_if_safety_replaced_snapshot(
    printer: &Printer,
    name: &str,
    snapshot: &SnapshotInfo,
    safety: Option<&str>,
) {
    let Some(safety) = safety else { return };
    if safety != crate::to_posix_string(&snapshot.path) {
        return;
    }
    printer.status_simple(
        crate::output::Role::Warn,
        format!(
            "backup '{name}': the safety backup rendered the same snapshot name as {} and replaced it \
             — the restore is unaffected, but give the unit a namePattern with finer resolution to keep both",
            snapshot.name
        ),
    );
}

/// A staged copy of the snapshot, beside the restore target.
///
/// The [`tempfile::TempDir`] is what makes cleanup unconditional: it is
/// removed when this value drops, on the success path and on every `?` that
/// leaves the restore early.
struct StagedSnapshot {
    _dir: tempfile::TempDir,
    payload: PathBuf,
}

/// Copy the snapshot into a temp directory beside `target`.
///
/// Beside the target rather than beside the snapshot for two reasons: the
/// staging copy must survive the retention prune the safety backup runs (which
/// only ever deletes inside the unit's destination), and landing it on the
/// target's filesystem keeps the overlay a local copy rather than a
/// cross-device one.
fn stage_snapshot(
    name: &str,
    snapshot: &SnapshotInfo,
    target: &Path,
) -> std::result::Result<StagedSnapshot, BackupError> {
    let staging_failed = |e: std::io::Error| BackupError::StagingFailed {
        name: name.to_string(),
        path: snapshot.path.clone(),
        source: e,
    };
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(staging_failed)?;
    let dir = tempfile::Builder::new()
        .prefix(".cfgd-restore-")
        .tempdir_in(parent)
        .map_err(staging_failed)?;

    let payload = dir.path().join("payload");
    let meta = std::fs::symlink_metadata(&snapshot.path).map_err(staging_failed)?;
    if meta.is_dir() {
        crate::copy_dir_recursive(&snapshot.path, &payload).map_err(staging_failed)?;
    } else {
        std::fs::copy(&snapshot.path, &payload).map_err(staging_failed)?;
    }
    Ok(StagedSnapshot { _dir: dir, payload })
}

/// Publish the staged payload over `target`.
///
/// A file goes through the backup writer's own
/// [`super::copy_file_snapshot`] — a fsynced temp file renamed into place, so
/// an interrupted restore never leaves the target half-written. A directory is
/// overlaid with [`crate::copy_dir_recursive`], the same helper the writer used
/// to take it, which creates what is missing, overwrites what collides, and
/// leaves everything else alone.
fn overlay_restore(
    name: &str,
    payload: &Path,
    target: &Path,
) -> std::result::Result<(), BackupError> {
    let restore_failed = |e: std::io::Error| BackupError::RestoreFailed {
        name: name.to_string(),
        path: target.to_path_buf(),
        source: e,
    };
    let meta = std::fs::symlink_metadata(payload).map_err(restore_failed)?;
    if meta.is_dir() {
        crate::copy_dir_recursive(payload, target).map_err(restore_failed)?;
    } else {
        super::copy_file_snapshot(payload, &meta, target).map_err(restore_failed)?;
    }
    Ok(())
}

/// Whether the snapshot payload is a `directory` or a `file`.
fn payload_kind(name: &str, path: &Path) -> Result<&'static str> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok("directory"),
        Ok(_) => Ok("file"),
        Err(_) => Err(BackupError::SnapshotMissing {
            name: name.to_string(),
            path: path.to_path_buf(),
        }
        .into()),
    }
}

/// Refuse a restore whose target is the opposite kind to the snapshot.
///
/// Checked before staging, so a mismatch costs nothing: publishing a file over
/// a directory would delete the entire directory on the way to the rename, and
/// overlaying a directory onto a file fails partway with a bare `ENOTDIR`.
fn check_target_kind(name: &str, target: &Path, snapshot_kind: &'static str) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(target) else {
        return Ok(());
    };
    let target_kind = if meta.is_dir() {
        "directory"
    } else if meta.is_symlink() {
        "symlink"
    } else {
        "file"
    };
    if target_kind == snapshot_kind {
        return Ok(());
    }
    Err(BackupError::RestoreKindMismatch {
        name: name.to_string(),
        target: target.to_path_buf(),
        snapshot_kind,
        target_kind,
    }
    .into())
}
