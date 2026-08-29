//! Put back the pre-restore copy a displacement left beside a unit's source.
//!
//! [`restore_backup`](super::restore_backup) copies the source's current
//! contents aside as the `<source>.cfgd-backup` sidecar cfgd leaves beside
//! every target it displaces ([`crate::reconciler::backup_file`]), and so does
//! `cfgd apply` when it adopts a file. Nothing listed those copies and nothing
//! put one back, so a restore aimed at the wrong snapshot was recoverable only
//! by hand.
//!
//! The verb runs through the SAME envelope a restore does — the unit's lock,
//! its one `preBackup`/`postBackup` hook list (with `CFGD_OPERATION=rollback`),
//! staging beside the target, and the same overlay — because it is the same
//! kind of write onto the same live data. The two differences are deliberate:
//!
//! - **The payload is a sidecar, not a snapshot.** It has no `backup_runs` row,
//!   so there is nothing to re-resolve under the lock; the newest copy beside
//!   the source is looked up again there instead.
//! - **The payload is staged before the safety copy is taken.** A rollback
//!   takes one exactly as a restore does, through the same sidecar writer, so
//!   the bytes it displaces are recoverable — an operator who edits the source
//!   after a restore and then rolls back would otherwise lose work no snapshot
//!   and no sidecar holds. The retention rule keeps one stamped copy per
//!   target, so writing that copy may prune the very sidecar being put back;
//!   staging runs first, which is what makes the order safe.
//!
//! A rollback is therefore its own inverse: it leaves the displaced contents in
//! the sidecar it just wrote, so rolling back twice returns the machine to
//! where it started rather than being a second no-op.

use std::path::{Path, PathBuf};

use crate::PathDisplayExt;
use crate::errors::{BackupError, Result};
use crate::output::{Printer, collapse_to_subject_line};
use crate::reconciler::{ScriptPhase, cfgd_backup_path};

use super::{BackupOperation, BackupUnit};

/// One retained sidecar beside a unit's source — what a rollback would put
/// back.
#[derive(Debug, Clone)]
pub struct RollbackCopy {
    /// Absolute path to the copy.
    pub path: PathBuf,
    /// ISO 8601 UTC time the copy was last written, read off the filesystem.
    /// The sidecar carries no record of its own, so its mtime is the only
    /// account of when the displacement happened.
    pub created: String,
    /// Size of the copy: the file's own length, or the whole tree's for a
    /// directory target.
    pub size_bytes: u64,
}

/// What a [`rollback_backup`] call did.
///
/// Shaped like [`super::RestoreOutcome`] and for the same reason: an overlay
/// that completed but left a `postBackup` hook failing is neither a success the
/// caller can ignore nor a failure that undoes the write.
#[derive(Debug, Clone)]
pub struct RollbackOutcome {
    /// The unit that was rolled back.
    pub name: String,
    /// The copy it was rolled back from, posix-folded.
    pub copy: String,
    /// Where the copy landed, posix-folded — the unit's source, with a
    /// top-level symlink followed.
    pub restored_to: String,
    /// Whether the overlay actually ran and completed.
    pub restored: bool,
    /// Size of the copy that was put back.
    pub size_bytes: u64,
    /// Where the contents the rollback displaced were copied aside. `None`
    /// when the source did not exist to be copied.
    pub safety_copy: Option<crate::reconciler::SidecarOutcome>,
    /// Every failure of the rollback, joined with `; `.
    pub error: Option<String>,
}

impl RollbackOutcome {
    /// Whether the overlay completed AND every hook succeeded — the predicate
    /// the caller's exit code gates on, matching
    /// [`super::RestoreOutcome::is_clean`].
    pub fn is_clean(&self) -> bool {
        self.restored && self.error.is_none()
    }

    /// The copy's file name, as the action row names it.
    fn copy_name(&self) -> String {
        Path::new(&self.copy)
            .file_name()
            .map_or_else(|| self.copy.clone(), |n| n.to_string_lossy().into_owned())
    }
}

/// The copy a rollback of `unit` would put back, or `None` when nothing ever
/// displaced its source.
///
/// The NEWEST sidecar beside the resolved source, by modification time: the
/// retention rule at the write keeps at most one stamped copy, so the choice is
/// between that copy and the primary `<source>.cfgd-backup` holding whatever
/// predated cfgd. A tie goes to the stamped name, which sorts after the primary
/// it extends.
///
/// The RESOLVED source, matching where
/// [`restore_backup`](super::restore_backup) takes its safety copy — a unit
/// whose `source:` is a link holds its sidecar beside the file the link points
/// at, not beside the link.
pub fn rollback_copy(unit: &BackupUnit<'_>) -> Option<RollbackCopy> {
    let source = super::restore::resolve_target_link(&unit.source());
    newest_sidecar(&source)
}

fn newest_sidecar(source: &Path) -> Option<RollbackCopy> {
    let dir = source.parent()?;
    let base = cfgd_backup_path(source, "")
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)?;
    let mut best: Option<(std::time::SystemTime, String, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // The pruner's own predicate: a name cfgd did not write is a file it
        // will not delete, so it must not be a file it publishes over live
        // data either. One meaning of "a sidecar cfgd wrote", both directions.
        if !(name == base || crate::reconciler::is_stamped_sidecar_name(&name, &base)) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let key = (modified, name, entry.path());
        if best.as_ref().is_none_or(|b| (b.0, &b.1) < (key.0, &key.1)) {
            best = Some(key);
        }
    }
    let (modified, _, path) = best?;
    Some(RollbackCopy {
        created: modified.duration_since(std::time::UNIX_EPOCH).map_or_else(
            |_| crate::utc_now_iso8601(),
            |d| crate::unix_secs_to_iso8601(d.as_secs()),
        ),
        size_bytes: sidecar_size(&path),
        path,
    })
}

/// The copy's size: a tree's total for a directory sidecar, the file's own
/// length otherwise. `0` when it cannot be read, which is the same answer a
/// snapshot with no recorded size gives.
fn sidecar_size(path: &Path) -> u64 {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => crate::dir_size(path).unwrap_or(0),
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

/// Put `unit`'s retained pre-restore copy back over its source.
///
/// # Sequence
///
/// The order mirrors [`restore_backup`](super::restore_backup)'s, minus the
/// step that produces a sidecar:
///
/// 1. take the unit's lock, so no run of this unit can rewrite the source
///    underneath the rollback, and look the copy up again inside it — the
///    caller resolved one before prompting, and a concurrent displacement may
///    have superseded it since;
/// 2. stage the copy into a temp directory beside the target, which is also
///    what puts it out of reach of the retention prune the next step runs;
/// 3. `preBackup` hooks;
/// 4. copy the source's CURRENT contents aside through the sidecar writer, so
///    the rollback is itself reversible, then the overlay, then `postBackup`
///    hooks;
/// 5. staging is removed on every path, success or failure.
///
/// # Failure semantics
///
/// [`super::run_backup`]'s: an operational failure is reported through the
/// returned [`RollbackOutcome`], and `Err` is reserved for failures that stop
/// the rollback before it can begin (a held lock, no copy to put back, a
/// kind mismatch, a failed staging, a safety copy that would not verify). A
/// `preBackup` failure skips the overlay; `postBackup` hooks run on every path,
/// because they are the counterpart that restarts whatever `preBackup` stopped.
pub fn rollback_backup(unit: &BackupUnit<'_>, printer: &Printer) -> Result<RollbackOutcome> {
    let spec = unit.spec();
    let name = spec.name.clone();
    let _lock = super::acquire_unit_lock(unit)?;

    let target = super::restore::resolve_target_link(&unit.source());
    let copy = rollback_copy(unit).ok_or_else(|| BackupError::NoRollbackCopy {
        name: name.clone(),
        source_path: target.clone(),
    })?;

    let copy_kind = super::restore::payload_kind(&name, &copy.path)?;
    super::restore::check_target_kind(&name, &target, copy_kind)?;
    let staged = printer.narrate(format!("Rolling back {name}: staging copy"), |_| {
        super::restore::stage_payload(&name, &copy.path, &target)
    })?;

    let mut failures: Vec<String> = Vec::new();
    // Collected and dropped: the out-parameter exists for the pseudo-phase that
    // counts the lines it emitted, and a rollback renders no owner group of its
    // own around the hooks.
    let mut items: Vec<super::BackupItem> = Vec::new();
    let pre_error = super::run_hooks(
        unit,
        &spec.pre_backup,
        ScriptPhase::PreBackup,
        BackupOperation::Rollback,
        printer,
        &mut items,
    )
    .err();

    // A failed `preBackup` leaves the source in whatever state the hook stopped
    // in — a service still writing to the file being replaced, most of the
    // time. Overwriting it then is exactly what the hook existed to prevent.
    let mut safety = None;
    let mut fatal = None;
    let mut overlay = None;
    if pre_error.is_none() {
        // Retired silently: the rollback's own status row is rendered after
        // this function returns.
        let mut sp = printer.spinner(format!(
            "Rolling back {name}: copying current contents aside"
        ));
        match super::restore::take_safety_copy(unit, &target) {
            Ok(taken) => {
                safety = taken;
                sp.set_message(format!("Rolling back {name}: overlaying files"));
                overlay = Some(super::restore::overlay_restore(
                    &name,
                    &staged.payload,
                    &target,
                ));
            }
            Err(e) => fatal = Some(e),
        }
        sp.finish_silent();
    }

    let post_error = super::run_hooks(
        unit,
        &spec.post_backup,
        ScriptPhase::PostBackup,
        BackupOperation::Rollback,
        printer,
        &mut items,
    )
    .err();

    if let Some(fatal) = fatal {
        // A `postBackup` failure on the way out must reach the structured
        // error itself, not just stderr, exactly as a restore's does.
        if let Some(e) = post_error {
            return Err(crate::errors::CfgdError::Backup(
                BackupError::RestoreAbortHookFailed {
                    fatal: Box::new(fatal),
                    post_message: collapse_to_subject_line(&e),
                },
            ));
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

    Ok(RollbackOutcome {
        name,
        copy: copy.path.posix().to_string(),
        restored_to: super::restore::report_path(&target),
        restored,
        size_bytes: copy.size_bytes,
        safety_copy: safety,
        error: (!failures.is_empty()).then(|| failures.join("; ")),
    })
}

/// Report a completed rollback the way [`super::report_restore`] reports a
/// completed restore: an owner section headed `backup:<name>`, one status row,
/// and the [`crate::reconciler::RunTally`] the caller closes with.
///
/// The role and the detail slot are `outcome_role`'s and `outcome_detail`'s —
/// the three verbs of one command settle through
/// one pair, so a fourth outcome cannot be worded twice.
pub fn report_rollback(
    printer: &Printer,
    outcome: &RollbackOutcome,
) -> crate::reconciler::RunTally {
    let group = printer.section_owner(&crate::output::OwnerLabel::new("backup", &outcome.name));
    let role = super::outcome_role(outcome.is_clean(), outcome.restored);
    let subject = super::rollback_subject(&outcome.restored_to, &outcome.copy_name());
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
        // The same slot a restore closes on, for the same reason: a rollback
        // displaces live data too, and the operator who regrets it needs to be
        // told where it went. Worded by the sidecar's own outcome.
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
        planned_total: super::RESTORE_ACTION_COUNT,
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
