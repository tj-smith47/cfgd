//! Collecting the snapshots a `destination:` change orphaned.
//!
//! The retention prune meets such a snapshot as a `backup_runs` row whose
//! `destination_path` is no longer inside the unit's destination, and marks it
//! [`BackupRunStatus::Orphaned`] rather than deleting the row: the row is the
//! only proof the payload was ever cfgd's, and without it nothing could ever
//! reclaim the bytes. This module is the other half — it removes the recorded
//! path and then the row.
//!
//! **Only a path the store recorded is ever touched.** Nothing here lists a
//! directory, so a file the operator put in an old destination by hand is not
//! cfgd's to find and not cfgd's to delete.

use std::path::Path;

use crate::output::{OwnerLabel, Printer, Role, collapse_to_subject_line};
use crate::reconciler::RunTally;
use crate::state::{ApplyStatus, BackupRunStatus, StateStore};

use super::BackupUnit;

/// One `backup_runs` row the prune re-classified: a snapshot outside its
/// unit's current destination, waiting to be collected.
///
/// Read once, before the run's header states how many actions it has, and
/// carried into [`collect_orphans`] — a second read of the same rows is a
/// second answer to a question the run already asked.
#[derive(Debug, Clone)]
pub struct OrphanedSnapshot {
    /// The `spec.backups[]` unit that recorded it.
    pub unit: String,
    /// The `backup_runs` row id, deleted once the payload is gone.
    pub run_id: i64,
    /// The recorded path, posix-folded as the row holds it.
    pub path: String,
    /// Bytes the snapshot occupied when it was written; `0` for a row that
    /// recorded no size.
    pub size_bytes: u64,
}

/// What [`collect_orphans`] did to one orphaned row.
#[derive(Debug, Clone)]
pub struct CollectedSnapshot {
    /// The unit the snapshot belonged to.
    pub name: String,
    /// The recorded path, posix-folded.
    pub path: String,
    pub size_bytes: u64,
    /// Why the removal failed. Set only on a [`CollectOutcome::failed`] entry;
    /// that row is left in place so a later `cfgd backup gc` retries it.
    pub error: Option<String>,
}

/// Everything one `cfgd backup gc` did, split by what happened to each row.
///
/// The three lists ARE the `-o json` payload's three keys, so the CLI maps
/// rather than re-derives, and [`CollectOutcome::tally`] counts the same
/// entries the screen was written from.
#[derive(Debug, Default)]
pub struct CollectOutcome {
    /// Rows whose payload this run removed.
    pub collected: Vec<CollectedSnapshot>,
    /// Rows whose payload was already gone: the row is dropped, but nothing on
    /// the machine changed.
    pub skipped: Vec<CollectedSnapshot>,
    /// Rows whose payload could not be removed. Each keeps its row.
    pub failed: Vec<CollectedSnapshot>,
}

impl CollectOutcome {
    /// How many rows the run set out to collect — the number its header
    /// promised and its rollup reconciles against.
    fn planned_total(&self) -> usize {
        self.collected.len() + self.skipped.len() + self.failed.len()
    }

    /// The rollup's view: a removal is a success, an already-gone payload a
    /// skip (nothing changed), and a removal that failed a failure.
    pub fn tally(&self) -> RunTally {
        let status = if !self.failed.is_empty() && self.collected.is_empty() {
            ApplyStatus::Failed
        } else if !self.failed.is_empty() {
            ApplyStatus::Partial
        } else {
            ApplyStatus::Success
        };
        RunTally {
            succeeded: self.collected.len(),
            skipped: self.skipped.len(),
            not_attempted: Vec::new(),
            failed: self.failed.len(),
            planned_total: self.planned_total(),
            status,
            aborted: None,
        }
    }
}

/// Every orphaned row of every unit, in unit order and newest row first.
///
/// A unit whose history cannot be read contributes nothing rather than failing
/// the whole run: the other units' payloads are still collectable, and the
/// unreadable one's rows stay exactly where they are.
pub fn orphaned_snapshots(
    store: &StateStore,
    units: &[BackupUnit<'_>],
    printer: &Printer,
) -> Vec<OrphanedSnapshot> {
    let mut out = Vec::new();
    for unit in units {
        let name = &unit.spec().name;
        let runs = match store.backup_runs(name) {
            Ok(runs) => runs,
            Err(e) => {
                printer
                    .status(
                        Role::Warn,
                        format!(
                            "{}: history unavailable",
                            OwnerLabel::new("backup", name).plain()
                        ),
                    )
                    .detail(collapse_to_subject_line(&e));
                continue;
            }
        };
        out.extend(
            runs.into_iter()
                .filter(|run| run.status == BackupRunStatus::Orphaned)
                .filter_map(|run| {
                    Some(OrphanedSnapshot {
                        unit: name.clone(),
                        run_id: run.id,
                        path: run.destination_path?,
                        size_bytes: run.size_bytes.unwrap_or(0),
                    })
                }),
        );
    }
    out
}

/// Remove each orphaned snapshot's recorded payload, then its row, rendering
/// one `backup:<name>` owner group per unit that has one.
///
/// The order is payload first, row second: a row deleted ahead of its payload
/// is a payload nothing can ever find again, which is the failure this whole
/// feature exists to undo. A removal that fails therefore keeps its row.
pub fn collect_orphans(
    store: &StateStore,
    orphans: &[OrphanedSnapshot],
    printer: &Printer,
) -> CollectOutcome {
    let mut outcome = CollectOutcome::default();
    let mut cursor = 0;
    while cursor < orphans.len() {
        let unit = &orphans[cursor].unit;
        let end = cursor
            + orphans[cursor..]
                .iter()
                .take_while(|o| &o.unit == unit)
                .count();
        let group = printer.section_owner(&OwnerLabel::new("backup", unit));
        for orphan in &orphans[cursor..end] {
            let path = Path::new(&orphan.path);
            let removal = remove_orphan(store, orphan, path);
            // Every row states why it exists: the payload is not where its unit
            // keeps snapshots any more, which is the whole reason gc can see it.
            let size = match &removal {
                Removal::AlreadyGone => "already gone, destination changed".to_string(),
                _ => format!(
                    "{}, destination changed",
                    crate::format_bytes(orphan.size_bytes)
                ),
            };
            let error = match &removal {
                Removal::Failed(e) => Some(e.clone()),
                _ => None,
            };
            group
                .status(removal.role(), super::collect_subject(path))
                .detail_opt(super::outcome_detail(error.as_deref(), Some(size)).as_deref());
            let entry = CollectedSnapshot {
                name: orphan.unit.clone(),
                path: orphan.path.clone(),
                size_bytes: orphan.size_bytes,
                error,
            };
            match removal {
                Removal::Collected => outcome.collected.push(entry),
                Removal::AlreadyGone => outcome.skipped.push(entry),
                Removal::Failed(_) => outcome.failed.push(entry),
            }
        }
        cursor = end;
    }
    outcome
}

/// What removing one orphan's payload did.
enum Removal {
    Collected,
    AlreadyGone,
    Failed(String),
}

impl Removal {
    /// The role its row settles as: a removal that changed nothing is a skip,
    /// never a success, so the rollup's counts match the glyphs on screen.
    fn role(&self) -> Role {
        match self {
            Removal::Collected => Role::Ok,
            Removal::AlreadyGone => Role::Skipped,
            Removal::Failed(_) => Role::Fail,
        }
    }
}

/// Remove one orphan's payload and, when that succeeded, its row.
fn remove_orphan(store: &StateStore, orphan: &OrphanedSnapshot, path: &Path) -> Removal {
    let existed = path.symlink_metadata().is_ok();
    if let Err(e) = super::remove_existing(path) {
        return Removal::Failed(e.to_string());
    }
    if let Err(e) = store.delete_backup_run(orphan.run_id) {
        return Removal::Failed(collapse_to_subject_line(&e));
    }
    if existed {
        Removal::Collected
    } else {
        Removal::AlreadyGone
    }
}
