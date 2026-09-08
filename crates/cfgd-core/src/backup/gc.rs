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
/// The four lists ARE the `-o json` payload's four keys, so the CLI maps
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
    /// Units whose history could not be read at all, carried over from the
    /// scan so the run's tally, its exit code and its payload state what cfgd
    /// could not ask about.
    pub unreadable: Vec<UnreadableUnit>,
}

impl CollectOutcome {
    /// How many rows the run set out to collect — the number its header
    /// promised and its rollup reconciles against.
    fn planned_total(&self) -> usize {
        self.collected.len() + self.skipped.len() + self.failed_count()
    }

    /// Every failure of the run: a payload that would not go, plus every unit
    /// nothing could be asked about. A history cfgd could not read is not a
    /// count of zero orphans, so it settles as a failure rather than letting
    /// the run claim it collected everything there was.
    fn failed_count(&self) -> usize {
        self.failed.len() + self.unreadable.len()
    }

    /// The rollup's view: a removal is a success, an already-gone payload a
    /// skip (nothing changed), and a removal that failed a failure.
    pub fn tally(&self) -> RunTally {
        let failed = self.failed_count();
        let status = if failed > 0 && self.collected.is_empty() {
            ApplyStatus::Failed
        } else if failed > 0 {
            ApplyStatus::Partial
        } else {
            ApplyStatus::Success
        };
        RunTally {
            succeeded: self.collected.len(),
            skipped: self.skipped.len(),
            not_attempted: Vec::new(),
            failed,
            planned_total: self.planned_total(),
            status,
            aborted: None,
        }
    }
}

/// A unit whose `backup_runs` history could not be read, and why.
///
/// Carried out of [`orphaned_snapshots`] rather than printed there: the read
/// answers how many actions the run has, so it runs before the header, and a
/// row rendered from inside it would land above the heading it belongs under.
#[derive(Debug, Clone)]
pub struct UnreadableUnit {
    /// The `spec.backups[]` unit whose history could not be read.
    pub unit: String,
    /// The store's own reason, collapsed to one physical row.
    pub error: String,
}

/// What one read of every unit's history found: the rows to collect, and the
/// units that could not be asked.
#[derive(Debug, Default)]
pub struct OrphanScan {
    /// Every orphaned row of every readable unit, in unit order and newest row
    /// first.
    pub orphans: Vec<OrphanedSnapshot>,
    /// The units whose history the read could not open.
    pub unreadable: Vec<UnreadableUnit>,
}

impl OrphanScan {
    /// How many actions this run sets out to perform: one per orphaned row,
    /// plus one per unit nothing could be asked about.
    ///
    /// The header states this number and [`CollectOutcome::tally`] settles the
    /// same one, so both readers take it from here: an unreadable unit is
    /// priced as a failure at the end, and a run that planned only the rows it
    /// could see would close on more outcomes than it opened with.
    pub fn action_count(&self) -> usize {
        self.orphans.len() + self.unreadable.len()
    }

    /// Render one failed row per unit whose history could not be read.
    ///
    /// The caller places this AFTER the run's header, which is why the read
    /// itself prints nothing. The role is the one
    /// [`CollectOutcome::tally`] counts the unit as, because a row's glyph and
    /// the rollup that prices it must say the same thing about the same unit.
    pub fn report_unreadable(&self, printer: &Printer) {
        for unit in &self.unreadable {
            printer
                .status(
                    Role::Fail,
                    format!(
                        "{}: history unavailable",
                        OwnerLabel::new("backup", &unit.unit).plain()
                    ),
                )
                .detail(&unit.error);
        }
    }
}

/// Every orphaned row of every unit, and every unit that could not be asked.
///
/// A unit whose history cannot be read contributes nothing to the rows rather
/// than failing the read: the other units' payloads are still collectable, and
/// the unreadable one's rows stay exactly where they are. The run still
/// reports it, because a history nobody could read is not a unit with nothing
/// to collect.
///
/// A row must fail BOTH questions to be collected: its stored status says
/// orphaned AND its recorded path is outside the destination in force. The
/// containment question is asked here rather than trusted from the status
/// alone, so a status write the prune could not land — a locked or full
/// database — can never cost a snapshot the destination still holds. Such a
/// row is left standing for the next prune to correct.
pub fn orphaned_snapshots(store: &StateStore, units: &[BackupUnit<'_>]) -> OrphanScan {
    let mut scan = OrphanScan::default();
    for unit in units {
        let name = &unit.spec().name;
        let destination = unit.destination_dir();
        let runs = match store.backup_runs(name) {
            Ok(runs) => runs,
            Err(e) => {
                scan.unreadable.push(UnreadableUnit {
                    unit: name.clone(),
                    error: collapse_to_subject_line(&e),
                });
                continue;
            }
        };
        scan.orphans.extend(
            runs.into_iter()
                .filter(|run| run.status == BackupRunStatus::Orphaned)
                .filter_map(|run| {
                    let path = run.destination_path?;
                    if super::is_snapshot_within(Path::new(&path), &destination) {
                        return None;
                    }
                    Some(OrphanedSnapshot {
                        unit: name.clone(),
                        run_id: run.id,
                        path,
                        size_bytes: run.size_bytes.unwrap_or(0),
                    })
                }),
        );
    }
    scan
}

/// Remove each orphaned snapshot's recorded payload, then its row, rendering
/// one `backup:<name>` owner group per unit that has one.
///
/// The order is payload first, row second: a row deleted ahead of its payload
/// is a payload nothing can ever find again, which is the failure this whole
/// feature exists to undo. A removal that fails therefore keeps its row.
///
/// Takes the whole scan so the units it could not ask about travel into the
/// outcome with the rows it could: one value then answers the rollup, the exit
/// code and the `-o json` payload.
pub fn collect_orphans(store: &StateStore, scan: OrphanScan, printer: &Printer) -> CollectOutcome {
    let OrphanScan {
        orphans,
        unreadable,
    } = scan;
    let mut outcome = CollectOutcome {
        unreadable,
        ..CollectOutcome::default()
    };
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
