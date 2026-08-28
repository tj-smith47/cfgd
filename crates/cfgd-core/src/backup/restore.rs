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
//! prune. The safety copy a restore-to-source takes of what it overwrites is
//! not a snapshot of the unit either: it lands beside the source as the same
//! `.cfgd-backup` sidecar cfgd leaves beside every target it displaces
//! ([`crate::reconciler::backup_file`]), outside the unit's destination, its
//! counts, its retention and its ledger. A restore is an event, and an event's
//! side effect must not read as a run.

use std::path::{Path, PathBuf};

use crate::errors::{BackupError, CfgdError, Result};
use crate::output::{Printer, collapse_to_subject_line};
use crate::reconciler::{ScriptPhase, SidecarOutcome};
use crate::state::StateStore;

use super::{BackupOperation, BackupUnit};

/// What a restore sets out to do, counted as a run counts actions: the one
/// overlay onto the target, however many hooks run around it.
///
/// Read by the header's `Actions {n} planned` row and by
/// [`report_restore`]'s [`crate::reconciler::RunTally::planned_total`], so the
/// two ends of one restore cannot state different amounts of work.
/// [`super::rollback_backup`] is the same one overlay and reads the same
/// constant, so the two verbs cannot promise different amounts of work for the
/// same shape of run.
pub const RESTORE_ACTION_COUNT: usize = 1;

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
    /// `backup_runs` row id of the run that wrote it. Selection happens before
    /// the restore takes the unit's lock, so [`restore_backup`] re-resolves the
    /// chosen snapshot by this id once the lock is held.
    pub run_id: i64,
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
    /// Where the snapshot landed, posix-folded. A symlinked target reports the
    /// path it resolved to, because that is where the bytes went.
    pub restored_to: String,
    /// Whether the overlay actually ran and completed.
    pub restored: bool,
    /// Size recorded for the restored snapshot.
    pub size_bytes: u64,
    /// The sidecar copy taken of the source's previous contents — where it
    /// landed (posix-folded) and whether a copy already holding those bytes
    /// was reused rather than written. `None` when the restore was redirected
    /// away from the live source (the source was never touched) or the source
    /// did not exist yet.
    pub safety_copy: Option<SidecarOutcome>,
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

/// Report a completed restore in the shape [`super::run_backup_group`] reports
/// a completed backup: an owner section headed `backup:<name>`, one status row
/// for the restore itself, and the one hint a regretted restore needs.
///
/// `backup run` and `backup restore` are the two mutating verbs of one command,
/// and the restore used to settle as a bare title plus a single status line —
/// no owner, no verdict — so the same operator reading the same command's two
/// halves had to learn two layouts. Returns the [`RunTally`] the caller closes
/// with, so the verdict counts the line that was actually printed.
///
/// The role and the detail slot are [`super::outcome_role`]'s and
/// [`super::outcome_detail`]'s — the same two [`super::report_backup_record`]
/// settles a backup through, because the two outcomes are the same three: a
/// clean restore is Ok, a restore whose overlay landed but whose hooks failed
/// is Warn (the data is back, something still needs attention), and a restore
/// that did not happen is Fail. `Partial` on the tally for the middle case
/// likewise matches what a dirty backup run rolls up to.
pub fn report_restore(printer: &Printer, outcome: &RestoreOutcome) -> crate::reconciler::RunTally {
    let group = printer.section_owner(&crate::output::OwnerLabel::new("backup", &outcome.name));
    let role = super::outcome_role(outcome.is_clean(), outcome.restored);
    let subject = super::restore_subject(&outcome.restored_to, &outcome.snapshot);
    let detail = super::outcome_detail(
        outcome.error.as_deref(),
        Some(crate::format_bytes(outcome.size_bytes)),
    );
    match detail {
        Some(detail) => {
            group.status(role, subject).detail(detail);
        }
        None => {
            group.status_simple(role, subject);
        }
    }
    if let Some(safety) = &outcome.safety_copy {
        // `hint`, not `note`: where the overwritten data went is the one thing
        // an operator needs after a restore they regret, and `note` is
        // Verbose-only. The sentence's verb is the sidecar's own: a copy that
        // was reused must not read as one written this time.
        group.hint(format!(
            "Previous contents {}; put them back with `cfgd backup rollback {}`",
            safety.detail(),
            outcome.name
        ));
    }
    crate::reconciler::RunTally {
        succeeded: usize::from(outcome.restored),
        skipped: 0,
        not_attempted: Vec::new(),
        failed: usize::from(!outcome.restored),
        planned_total: RESTORE_ACTION_COUNT,
        status: if outcome.is_clean() {
            crate::state::ApplyStatus::Success
        } else if outcome.restored {
            crate::state::ApplyStatus::Partial
        } else {
            crate::state::ApplyStatus::Failed
        },
        aborted: None,
    }
}

/// Where a restore will write.
///
/// Two paths rather than one because they can differ, and a caller needs both:
/// the confirmation prompt has to name what is actually about to be
/// overwritten, while an operator who typed `~/dotfiles` needs to recognise
/// their own words in it.
#[derive(Debug, Clone)]
pub struct RestoreTarget {
    /// The path as the caller named it: `--to`, or the unit's `source`, with a
    /// leading `~` expanded.
    pub requested: PathBuf,
    /// Where the bytes actually land — [`RestoreTarget::requested`] with a
    /// top-level symlink followed, matching how the writer stats the source.
    /// Equal to `requested` whenever the target is not a link.
    pub resolved: PathBuf,
}

impl RestoreTarget {
    /// Whether following the link changed where the restore writes.
    pub fn was_redirected_by_a_link(&self) -> bool {
        self.resolved != self.requested
    }

    /// [`RestoreTarget::resolved`] as every user-facing surface must render it.
    ///
    /// Not `posix()`: `resolved` comes from `canonicalize`, which on Windows
    /// hands back a `\\?\`-prefixed path that folds to `//?/C:/…`. Every place
    /// a restore path reaches a human — the confirmation prompt, the declined
    /// payload, `restoredTo` — goes through here, or the same path renders two
    /// different ways depending on which one printed it.
    pub fn resolved_display(&self) -> String {
        report_path(&self.resolved)
    }

    /// [`RestoreTarget::requested`] rendered the same way, for the prompt's
    /// `(via …)` clause.
    pub fn requested_display(&self) -> String {
        report_path(&self.requested)
    }
}

/// Resolve where a restore will write, without touching anything.
///
/// Exported so the caller that renders the confirmation prompt names the same
/// path [`restore_backup`] will overwrite, rather than re-deriving it and
/// prompting about a path that is not the one written.
pub fn restore_target(unit: &BackupUnit<'_>, to: Option<&Path>) -> RestoreTarget {
    let requested = match to {
        Some(path) => crate::expand_tilde(path),
        None => unit.source(),
    };
    let resolved = resolve_target_link(&requested);
    RestoreTarget {
        requested,
        resolved,
    }
}

/// Every restorable snapshot of `unit`, newest first.
///
/// Read from the run records, not a directory glob, so the list agrees with
/// what retention pruning walks. Two gates apply on top: a record whose path is
/// not demonstrably inside *this* unit's destination is ignored (the same
/// `is_snapshot_within` check pruning uses, so a stale or foreign row
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
            run_id: run.id,
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
///    underneath the restore, and re-resolve the selected snapshot inside it —
///    selection happened before the lock, so a concurrent run may have pruned
///    it in between;
/// 2. stage the snapshot into a temp directory beside the target, so the
///    overlay is a local copy whatever happens to the snapshot store meanwhile;
/// 3. `preBackup` hooks;
/// 4. a safety copy of the source's current contents, taken through the
///    sidecar writer ([`crate::reconciler::backup_file`]) so it lands beside
///    the source as `<source>.cfgd-backup`: not in the unit's destination, not
///    in `backup_runs`, not counted or pruned as one of its snapshots. Skipped
///    when the target is not the live source (nothing of the unit's is being
///    overwritten) or the source does not exist yet (there is nothing to
///    protect). A safety copy that cannot be written aborts the restore;
/// 5. the overlay, then `postBackup` hooks;
/// 6. staging is removed on every path, success or failure.
///
/// The hooks wrap the safety copy rather than it running its own pair: the
/// unit declares one `preBackup`/`postBackup` list, and running it twice
/// around a source the restore has already quiesced is both surprising and,
/// for a hook that is not idempotent, wrong. Hooks can tell the two operations
/// apart through `$CFGD_OPERATION` (`backup` or `restore`).
///
/// # Overlay semantics
///
/// A directory snapshot is overlaid, not mirrored: every file it holds
/// overwrites its counterpart, and files present only in the target are left
/// alone. Modes come across with the copy. A target entry whose kind differs
/// from the snapshot's — most importantly a **symlink** — is removed and
/// replaced rather than written through, so a restore can never modify a file
/// outside the target that the safety copy did not capture. The overlay is
/// not atomic as a whole: individual files are replaced atomically, but an
/// interrupted directory restore leaves the target part old and part new, and
/// the safety copy is what recovers it.
///
/// # Failure semantics
///
/// Mirrors [`super::run_backup`]: an operational failure is reported through
/// the returned [`RestoreOutcome`], and `Err` is reserved for failures that
/// stop the restore before it can begin (a held lock, a vanished snapshot, a
/// failed safety copy). A `preBackup` failure skips both the safety copy and
/// the overlay; `postBackup` hooks run on every path, including
/// the one that ends in `Err`, because they are the counterpart that restarts
/// whatever `preBackup` stopped.
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
    let snapshot = reresolve_snapshot(unit, store, snapshot)?;

    // The writer stats the source *through* symlinks, so a unit whose `source:`
    // is a link to a directory holds a directory snapshot. Restoring it has to
    // follow the same link, or the snapshot would replace the link itself with
    // the tree it points at.
    let target = restore_target(unit, to).resolved;
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
    // Narrated: extracting an archive snapshot is the restore's first
    // multi-second wait and prints nothing of its own until it is done.
    let staged = printer.narrate(format!("Restoring {name}: staging snapshot"), |_| {
        stage_payload(&name, &snapshot.path, &target)
    })?;

    let mut failures: Vec<String> = Vec::new();
    // A restore renders no owner group and rolls nothing up, so its hook items
    // are collected and dropped: the out-parameter exists for the pseudo-phase
    // that counts the lines it emitted, and a restore has none.
    let mut items: Vec<super::BackupItem> = Vec::new();
    let pre_error = super::run_hooks(
        unit,
        &spec.pre_backup,
        ScriptPhase::PreBackup,
        BackupOperation::Restore,
        printer,
        &mut items,
    )
    .err();

    // A failed `preBackup` leaves the target in whatever state the hook stopped
    // in — a service still writing to the file being replaced, most of the
    // time. Overwriting it then is exactly what the hook existed to prevent.
    let mut safety = None;
    let mut overlay = None;
    let mut fatal = None;
    if pre_error.is_none() {
        // Retired silently: every outcome below already has its own line —
        // the restore status, or the fatal error the caller renders.
        let mut sp = printer.spinner(format!("Restoring {name}: safety copy"));
        match take_safety_copy(unit, &target) {
            Ok(taken) => {
                safety = taken;
                sp.set_message(format!("Restoring {name}: overlaying files"));
                overlay = Some(overlay_restore(&name, &staged.payload, &target));
            }
            Err(e) => fatal = Some(e),
        }
        sp.finish_silent();
    }

    let post_error = super::run_hooks(
        unit,
        &spec.post_backup,
        ScriptPhase::PostBackup,
        BackupOperation::Restore,
        printer,
        &mut items,
    )
    .err();

    if let Some(fatal) = fatal {
        // A `postBackup` failure on the way out must reach the structured
        // error itself, not just stderr — a status line never reaches a
        // `-o json` consumer.
        if let Some(e) = post_error {
            return Err(CfgdError::Backup(BackupError::RestoreAbortHookFailed {
                fatal: Box::new(fatal),
                post_message: collapse_to_subject_line(&e),
            }));
        }
        return Err(fatal);
    }

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
        restored_to: report_path(&target),
        restored,
        size_bytes: snapshot.size_bytes,
        safety_copy: safety,
        error: (!failures.is_empty()).then(|| failures.join("; ")),
    })
}

/// Look the selected snapshot up again now that the unit's lock is held.
///
/// [`list_snapshots`] and [`select_snapshot`] run before the prompt, outside the
/// lock, so a concurrent `cfgd backup run` can retire the chosen snapshot in the
/// window between the operator reading it and answering `y`. Matching on the run
/// id rather than the path also catches the case where the row survived but now
/// names a different payload.
fn reresolve_snapshot(
    unit: &BackupUnit<'_>,
    store: &StateStore,
    selected: &SnapshotInfo,
) -> Result<SnapshotInfo> {
    let snapshots = list_snapshots(unit, store)?;
    snapshots
        .into_iter()
        .find(|s| s.run_id == selected.run_id && s.path == selected.path)
        .ok_or_else(|| {
            BackupError::SnapshotMissing {
                name: unit.spec().name.clone(),
                path: selected.path.clone(),
            }
            .into()
        })
}

/// Follow a target that is itself a symlink, so the restore writes what the
/// backup read.
///
/// Only the target's own final component is followed. Links found *inside* a
/// directory overlay are replaced, not followed — those are incidental entries
/// the snapshot never captured, and writing through one escapes the target.
/// A broken link is left as-is for the overlay to replace.
///
/// `canonicalize`'s own [`PathBuf`] is returned untouched, verbatim prefix and
/// all. This value is a WRITE destination, and posix-folding it would turn a
/// Windows UNC canonicalization (`\\?\UNC\server\share\x`) into the relative
/// path `UNC/server/share/x` — the overlay would then build that tree under the
/// working directory. Folding happens once, in [`report_path`], on the way out.
pub(super) fn resolve_target_link(target: &Path) -> PathBuf {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.is_symlink() => target
            .canonicalize()
            .unwrap_or_else(|_| target.to_path_buf()),
        _ => target.to_path_buf(),
    }
}

/// A path as the restore reports it: posix-folded, with the Windows verbatim
/// prefix `canonicalize` adds dropped so `restoredTo` reads as the path the
/// operator knows rather than `//?/C:/...`.
pub(super) fn report_path(path: &Path) -> String {
    crate::strip_windows_verbatim(&crate::to_posix_string(path)).to_string()
}

/// Copy the source's current contents aside before it is overwritten,
/// returning what the sidecar writer did, its path posix-folded.
///
/// The sidecar writer, not the snapshot writer: a restore is cfgd about to
/// displace data it did not write, which is exactly what the `.cfgd-backup`
/// beside an adopted target already preserves, and a copy stored as one of the
/// unit's snapshots was a copy that counted, listed, pruned and re-anchored
/// like a backup of the unit. Copied from the RESOLVED source (the tree the
/// overlay actually writes into) rather than the target, so `--to` aimed at a
/// path inside the source still captures the whole of what the unit owns.
///
/// `Ok(None)` means no safety copy was warranted: the restore was redirected
/// away from the live source, or the source does not exist yet (a bare-metal
/// restore has nothing to protect). Anything else that fails to produce the
/// copy is an `Err` — the restore must not proceed over data that was not
/// captured.
///
/// The sidecar writer reads a regular file whole to verify the copy's hash.
/// ponytail: a multi-gigabyte single-file unit is held in memory here;
/// stream-and-hash it if one ever turns up.
fn take_safety_copy(unit: &BackupUnit<'_>, target: &Path) -> Result<Option<SidecarOutcome>> {
    if !overwrites_source(unit, target) {
        return Ok(None);
    }
    let source = resolve_target_link(&unit.source());
    let outcome = crate::reconciler::backup_file(&source).map_err(|e| {
        CfgdError::Backup(BackupError::SafetyBackupFailed {
            name: unit.spec().name.clone(),
            message: collapse_to_subject_line(&e),
        })
    })?;
    Ok(Some(SidecarOutcome {
        path: PathBuf::from(report_path(&outcome.path)),
        reused: outcome.reused,
    }))
}

/// Whether restoring into `target` overwrites data the unit's own source owns.
///
/// Keyed on the resolved paths, not on whether `--to` was passed: `--to` aimed
/// back at the source (or at a directory inside it) overwrites exactly what a
/// plain restore would, and skipping the safety copy there would destroy
/// live data on the operator's behalf. A source that does not exist yet has
/// nothing to protect.
fn overwrites_source(unit: &BackupUnit<'_>, target: &Path) -> bool {
    let source = unit.source();
    if source.symlink_metadata().is_err() {
        return false;
    }
    super::is_at_or_within(
        &super::resolve_for_containment(target),
        &super::resolve_for_containment(&source),
    )
}

/// A staged copy of the snapshot, beside the restore target.
///
/// The [`tempfile::TempDir`] is what makes cleanup unconditional: it is
/// removed when this value drops, on the success path and on every `?` that
/// leaves the restore early.
pub(super) struct StagedSnapshot {
    _dir: tempfile::TempDir,
    pub(super) payload: PathBuf,
}

/// Copy the payload a restore or a rollback will publish into a temp directory
/// beside `target`.
///
/// Beside the target rather than beside the payload, so landing it on the
/// target's filesystem keeps the overlay a local copy rather than a
/// cross-device one, and so nothing walking the unit's destination can take it
/// for a snapshot.
///
/// Staging goes in the nearest ancestor of `target` that already exists.
/// Creating the missing ones here would leave an empty tree behind on every
/// path that aborts before the overlay; the overlay creates them itself, once
/// it is certain it is going to write.
///
/// Taken by path rather than by [`SnapshotInfo`] because a rollback's payload
/// is a sidecar beside the source and has no run record at all; the two verbs
/// publish through one staging step so an interrupted rollback recovers the
/// way an interrupted restore does.
pub(super) fn stage_payload(
    name: &str,
    source: &Path,
    target: &Path,
) -> std::result::Result<StagedSnapshot, BackupError> {
    let staging_failed = |e: std::io::Error| BackupError::StagingFailed {
        name: name.to_string(),
        path: source.to_path_buf(),
        source: e,
    };
    let dir = tempfile::Builder::new()
        .prefix(".cfgd-restore-")
        .tempdir_in(existing_ancestor(target))
        .map_err(staging_failed)?;

    let payload = dir.path().join("payload");
    let meta = std::fs::symlink_metadata(source).map_err(staging_failed)?;
    if meta.is_dir() {
        crate::copy_dir_recursive(source, &payload).map_err(staging_failed)?;
    } else {
        std::fs::copy(source, &payload).map_err(staging_failed)?;
    }
    Ok(StagedSnapshot { _dir: dir, payload })
}

/// The nearest ancestor of `target` that exists as a directory.
///
/// `.` when there is none — which is the right answer for a relative target
/// whose parent is the empty path, and the only answer left for an absolute one
/// whose root is unreadable.
fn existing_ancestor(target: &Path) -> PathBuf {
    let mut cursor = target.parent();
    while let Some(dir) = cursor {
        if dir.is_dir() {
            return dir.to_path_buf();
        }
        cursor = dir.parent();
    }
    PathBuf::from(".")
}

/// Publish the staged payload over `target`.
///
/// A file goes through the backup writer's own [`super::copy_file_snapshot`] —
/// a fsynced temp file renamed into place, so an interrupted restore never
/// leaves the target half-written, and whatever occupied the name (including a
/// symlink) is unlinked rather than written through. A directory is overlaid
/// entry by entry with the same guarantee per file.
pub(super) fn overlay_restore(
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
        overlay_dir(payload, target).map_err(restore_failed)?;
    } else {
        crate::ensure_parent_dir(target).map_err(restore_failed)?;
        super::copy_file_snapshot(payload, &meta, target).map_err(restore_failed)?;
    }
    Ok(())
}

/// Overlay one directory of the staged payload onto its counterpart in the
/// target.
///
/// Written here rather than reusing [`crate::copy_dir_recursive`] because the
/// two walk in opposite directions. That helper writes into a destination cfgd
/// just created, so following a link on the way in is impossible; a restore
/// writes into **live user data**, where a link at a name the snapshot owns is
/// entirely ordinary. `std::fs::copy` through such a link would truncate the
/// file it points at — outside the target, and outside what the safety copy
/// captured — and descending into a linked directory would write a whole subtree
/// there. Every destination entry is therefore stat'd unfollowed and replaced
/// when its kind does not match, which is the same rule the writer applies to
/// the snapshot side.
fn overlay_dir(payload: &Path, target: &Path) -> std::io::Result<()> {
    ensure_dir(target)?;
    for entry in std::fs::read_dir(payload)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let source = entry.path();
        let destination = target.join(entry.file_name());
        if kind.is_dir() {
            overlay_dir(&source, &destination)?;
        } else if kind.is_file() {
            let meta = entry.metadata()?;
            super::copy_file_snapshot(&source, &meta, &destination)?;
        }
        // Nothing else can appear: the payload is a copy of a snapshot, and the
        // snapshot writer skips symlinks and special files outright.
    }
    crate::carry_dir_mode(payload, target);
    Ok(())
}

/// Make `path` a real directory, removing whatever else occupies it.
///
/// A symlink is *not* a directory here even when it points at one:
/// `symlink_metadata` reports the link itself, so a linked directory falls into
/// the replace arm and the overlay stays inside the target.
fn ensure_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => {
            super::remove_existing(path)?;
            std::fs::create_dir_all(path)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(path),
        Err(e) => Err(e),
    }
}

/// Whether the snapshot payload is a `directory` or a `file`.
pub(super) fn payload_kind(name: &str, path: &Path) -> Result<&'static str> {
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
///
/// Stat'd *through* links, matching how the writer stats the source. The target
/// has already had its own final component resolved by [`resolve_target_link`],
/// so the only path that lands here unresolved is a broken link — which reads as
/// absent and is replaced rather than refused, exactly as a missing target is.
pub(super) fn check_target_kind(
    name: &str,
    target: &Path,
    snapshot_kind: &'static str,
) -> Result<()> {
    let Ok(meta) = std::fs::metadata(target) else {
        return Ok(());
    };
    let target_kind = if meta.is_dir() { "directory" } else { "file" };
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
