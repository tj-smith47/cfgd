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
//! - **No safety copy of its own.** Taking one would write a second sidecar
//!   beside the very copy being consumed, and the retention rule keeps one
//!   stamped copy per target — so the rollback would either supersede its own
//!   payload or leave two copies where the rule says one. What a rollback
//!   displaces is what a restore just wrote, which is still in the unit's
//!   snapshot store; the confirmation prompt is the gate.
//!
//! The sidecar is LEFT in place afterwards. It is what `cfgd profile update`
//! and module removal restore from when it is the primary copy, and leaving it
//! makes a repeated rollback a no-op rather than a flip-flop.

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
        if !(name == base || name.starts_with(&format!("{base}."))) {
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
/// 2. stage the copy into a temp directory beside the target;
/// 3. `preBackup` hooks;
/// 4. the overlay, then `postBackup` hooks;
/// 5. staging is removed on every path, success or failure.
///
/// # Failure semantics
///
/// [`super::run_backup`]'s: an operational failure is reported through the
/// returned [`RollbackOutcome`], and `Err` is reserved for failures that stop
/// the rollback before it can begin (a held lock, no copy to put back, a
/// kind mismatch, a failed staging). A `preBackup` failure skips the overlay;
/// `postBackup` hooks run on every path, because they are the counterpart that
/// restarts whatever `preBackup` stopped.
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
    let overlay = match pre_error {
        Some(_) => None,
        None => {
            // Retired silently: the rollback's own status row is rendered after
            // this function returns.
            let sp = printer.spinner(format!("Rolling back {name}: overlaying files"));
            let done = super::restore::overlay_restore(&name, &staged.payload, &target);
            sp.finish_silent();
            Some(done)
        }
    };

    let post_error = super::run_hooks(
        unit,
        &spec.post_backup,
        ScriptPhase::PostBackup,
        BackupOperation::Rollback,
        printer,
        &mut items,
    )
    .err();

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
        error: (!failures.is_empty()).then(|| failures.join("; ")),
    })
}

/// Report a completed rollback the way [`super::report_restore`] reports a
/// completed restore: an owner section headed `backup:<name>`, one status row,
/// and the [`crate::reconciler::RunTally`] the caller closes with.
///
/// The role and the detail slot are [`super::outcome_role`]'s and
/// [`super::outcome_detail`]'s — the three verbs of one command settle through
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
