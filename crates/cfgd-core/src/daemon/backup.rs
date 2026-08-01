// Scheduled `spec.backups[]` timers for the daemon loop.
//
// This module owns SCHEDULING only. A fire dispatches the same
// `crate::backup::run_backup` a `cfgd backup run` does, against the same
// `BackupUnit`, so a scheduled run and a CLI-triggered run write identical
// `backup_runs` rows, honour the same retention, and run the same hooks.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

use crate::backup::{BackupUnit, run_backup};
use crate::config::{self, BackupSpec, CfgdConfig};
use crate::output::{Printer, Role, collapse_to_subject_line};
use crate::reconciler::ReconcileContext;
use crate::state::{BackupRunStatus, StateStore};

/// Floor for an interval schedule. `parse_duration_str` accepts `0`, which
/// would turn the loop's timer branch into a spin.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// How many missed fires `BackupTask::advance` walks before jumping straight to
/// the next fire after now. A per-second schedule and a daemon that was blocked
/// for a day would otherwise be walked one occurrence at a time.
const MAX_MISSED_CATCHUP: u32 = 1000;

/// Deadline used when a schedule cannot produce a next occurrence at all. Long
/// enough not to spin, short enough that a schedule which only fails for a
/// transient reason (a DST boundary the search stumbled on) recovers.
const SCHEDULE_STALL_RETRY: Duration = Duration::from_secs(3600);

/// How a scheduled backup decides when to fire next.
pub(super) enum BackupSchedule {
    /// A fixed period between runs (`6h`, `30m`, `1d`).
    Interval(Duration),
    /// A cron expression, evaluated against the machine's LOCAL time so
    /// `0 3 * * *` means 3am where the machine sits — the same reading a
    /// crontab entry gets.
    Cron(Box<croner::Cron>),
}

impl BackupSchedule {
    /// Parse a `spec.backups[].schedule`.
    ///
    /// Interval first, then cron: the identical precedence
    /// `config::validate_backup_schedule` applies, so a value the config parser
    /// accepted is interpreted here exactly as it was validated. `None` means
    /// neither form parsed, which the parser should already have rejected.
    pub(super) fn parse(schedule: &str) -> Option<Self> {
        if let Ok(d) = crate::parse_duration_str(schedule) {
            return Some(Self::Interval(d.max(MIN_INTERVAL)));
        }
        schedule
            .parse::<croner::Cron>()
            .ok()
            .map(|c| Self::Cron(Box::new(c)))
    }

    /// The first fire strictly after `from`, as both a monotonic deadline and
    /// the wall-clock time it corresponds to.
    ///
    /// Both clocks are threaded through because the two schedule kinds need
    /// different ones: an interval is a pure monotonic offset (immune to the
    /// operator setting the clock), while cron is defined on wall time and has
    /// nothing to anchor to without it.
    fn next_after(
        &self,
        from: Instant,
        from_wall: DateTime<Local>,
    ) -> Option<(Instant, DateTime<Local>)> {
        match self {
            Self::Interval(period) => {
                let wall =
                    from_wall.checked_add_signed(chrono::TimeDelta::from_std(*period).ok()?)?;
                Some((from + *period, wall))
            }
            Self::Cron(cron) => {
                let next_wall = cron.find_next_occurrence(&from_wall, false).ok()?;
                let delta = (next_wall - from_wall).to_std().ok()?;
                Some((from + delta, next_wall))
            }
        }
    }
}

/// One scheduled `spec.backups[]` entry bound to its next deadline.
///
/// The spec is cloned rather than re-resolved on each fire so the timer set is
/// a stable snapshot of the config: a spec edit reaches the daemon through the
/// SIGHUP rebuild, the same way a schedule edit does, instead of one field
/// changing under a timer that still carries the old cadence.
pub(crate) struct BackupTask {
    pub(super) spec: BackupSpec,
    /// The profile the spec was resolved from — `BackupUnit` puts it in
    /// `$CFGD_PROFILE` for the hooks.
    pub(super) profile_name: String,
    schedule: BackupSchedule,
    /// The raw schedule string, so a reload can tell an unchanged unit (carry
    /// the pending deadline over) from a rescheduled one (restart the clock)
    /// without reconstructing it from the parsed form.
    schedule_str: String,
    next_fire: Instant,
}

impl BackupTask {
    /// Build a timer for `spec`, or `None` when it has no `schedule` (an
    /// apply-time backup) or the schedule does not parse.
    pub(super) fn new(spec: &BackupSpec, profile_name: &str, now: Instant) -> Option<Self> {
        let schedule_str = spec.schedule.clone()?;
        let Some(schedule) = BackupSchedule::parse(&schedule_str) else {
            tracing::warn!(
                backup = %spec.name,
                schedule = %schedule_str,
                "backup timer: schedule is neither an interval nor a cron expression — no timer installed"
            );
            return None;
        };
        let next_fire = schedule
            .next_after(now, Local::now())
            .map(|(instant, _)| instant)
            .unwrap_or_else(|| {
                tracing::warn!(
                    backup = %spec.name,
                    schedule = %schedule_str,
                    "backup timer: schedule has no upcoming occurrence — retrying later"
                );
                now + SCHEDULE_STALL_RETRY
            });
        Some(Self {
            spec: spec.clone(),
            profile_name: profile_name.to_string(),
            schedule,
            schedule_str,
            next_fire,
        })
    }

    /// The deadline the loop's timer branch waits on.
    pub(super) fn next_fire(&self) -> Instant {
        self.next_fire
    }

    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.next_fire <= now
    }

    /// Consume the fire that is due and arm the next one, returning how many
    /// occurrences were passed over on the way.
    ///
    /// A nonzero return is the skip half of the overlap guard: the loop is
    /// serial, so a run that outlives its own next fire delays this call until
    /// it finishes, and every occurrence that elapsed meanwhile is dropped
    /// rather than queued behind it.
    pub(super) fn advance(&mut self, now: Instant) -> u32 {
        let now_wall = Local::now();
        // Wall time of the fire being consumed, reconstructed from how long ago
        // the deadline passed. Both clocks advance together, so this is the
        // anchor a cron search needs to enumerate what it missed.
        let behind = now.saturating_duration_since(self.next_fire);
        let mut cursor = self.next_fire;
        let mut cursor_wall = chrono::TimeDelta::from_std(behind)
            .ok()
            .and_then(|d| now_wall.checked_sub_signed(d))
            .unwrap_or(now_wall);

        let mut missed = 0u32;
        loop {
            let Some((next, next_wall)) = self.schedule.next_after(cursor, cursor_wall) else {
                tracing::warn!(
                    backup = %self.spec.name,
                    schedule = %self.schedule_str,
                    "backup timer: schedule has no upcoming occurrence — retrying later"
                );
                self.next_fire = now + SCHEDULE_STALL_RETRY;
                return missed;
            };
            if next > now {
                self.next_fire = next;
                return missed;
            }
            missed += 1;
            if missed >= MAX_MISSED_CATCHUP {
                self.next_fire = self
                    .schedule
                    .next_after(now, now_wall)
                    .map(|(instant, _)| instant)
                    .unwrap_or(now + SCHEDULE_STALL_RETRY);
                return missed;
            }
            cursor = next;
            cursor_wall = next_wall;
        }
    }
}

/// Build one timer per SCHEDULED backup. A schedule-less entry gets no timer —
/// it runs during `cfgd apply`.
pub(super) fn build_backup_tasks(
    specs: &[BackupSpec],
    profile_name: &str,
    now: Instant,
) -> Vec<BackupTask> {
    specs
        .iter()
        .filter_map(|spec| BackupTask::new(spec, profile_name, now))
        .collect()
}

/// What a SIGHUP rebuild did to the timer set, for the reload report.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct BackupReloadSummary {
    pub(super) added: usize,
    pub(super) removed: usize,
    pub(super) rescheduled: usize,
}

impl BackupReloadSummary {
    pub(super) fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.rescheduled == 0
    }
}

/// Replace `current` with `next`, carrying the pending deadline of every unit
/// whose schedule string is unchanged.
///
/// Without the carry-over, a reload prompted by an unrelated edit would restart
/// the clock on every backup, and a daily backup on a machine whose config is
/// reloaded more than once a day would never fire.
pub(super) fn reload_backup_tasks(
    current: &mut Vec<BackupTask>,
    mut next: Vec<BackupTask>,
) -> BackupReloadSummary {
    let mut summary = BackupReloadSummary::default();
    for task in next.iter_mut() {
        match current
            .iter()
            .find(|existing| existing.spec.name == task.spec.name)
        {
            Some(existing) if existing.schedule_str == task.schedule_str => {
                task.next_fire = existing.next_fire;
            }
            Some(_) => summary.rescheduled += 1,
            None => summary.added += 1,
        }
    }
    summary.removed = current
        .iter()
        .filter(|existing| !next.iter().any(|task| task.spec.name == existing.spec.name))
        .count();
    *current = next;
    summary
}

/// Resolve the machine's scheduled backups into a fresh timer set.
///
/// Composition is cache-only (the sync task owns fetch cadence), so a
/// source-delivered backup gets a timer like a locally-declared one. Unlike the
/// reconcile tick, a compose failure degrades to the locally-declared set
/// instead of skipping: a backup only ever writes into its own destination, so
/// running the local set cannot damage source-delivered state, while dropping
/// every timer would silently stop the machine's backups.
pub(super) fn resolve_backup_tasks(
    cfg: &CfgdConfig,
    config_path: &Path,
    profile_override: Option<&str>,
    printer: &Printer,
    scope: crate::Scope,
) -> Vec<BackupTask> {
    let profiles_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profiles");
    let profile_name = profile_override
        .or(cfg.spec.profile.as_deref())
        .unwrap_or("default");

    let local = match config::resolve_profile(profile_name, &profiles_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                profile = %profile_name,
                "backup timers: profile resolution failed — no scheduled backups installed"
            );
            return Vec::new();
        }
    };

    let specs = match super::compose_daemon_desired_state(cfg, &local, printer, scope) {
        Ok((resolved, _)) => resolved.merged.backups,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "backup timers: source composition failed — falling back to locally-declared backups"
            );
            local.merged.backups.clone()
        }
    };

    build_backup_tasks(&specs, profile_name, Instant::now())
}

/// Run one scheduled backup. Blocking; the loop dispatches it through
/// `spawn_blocking_with_test_home`.
///
/// Failures never propagate: `run_backup` records an operational failure in the
/// returned row, and the `Err` arm (a state-store write) is a storage problem
/// for one unit, not a reason to take the daemon's timer branch down.
pub(super) fn run_scheduled_backup(
    spec: &BackupSpec,
    config_dir: &Path,
    profile_name: &str,
    state_dir: &Path,
    printer: &Printer,
) {
    let store = match StateStore::open_in_dir(state_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                backup = %spec.name,
                error = %e,
                "scheduled backup: state store error — run skipped"
            );
            return;
        }
    };

    let unit = BackupUnit::new(spec, config_dir, profile_name, state_dir)
        .with_context(ReconcileContext::Reconcile);
    let subject = format!("backup '{}'", spec.name);
    match run_backup(&unit, &store, printer) {
        Ok(record) => {
            // The same three-way split `cfgd backup run` reports: a clean run is
            // Ok, a good snapshot with a failed postBackup hook is Warn, and no
            // artifact at all is Fail.
            let role = if record.is_clean() {
                Role::Ok
            } else if record.status == BackupRunStatus::Success {
                Role::Warn
            } else {
                Role::Fail
            };
            match &record.error {
                Some(e) => {
                    printer
                        .status(role, subject)
                        .detail(collapse_to_subject_line(e));
                    tracing::warn!(backup = %record.name, error = %e, "scheduled backup completed with errors");
                }
                None => {
                    printer.status_simple(role, subject);
                    tracing::info!(backup = %record.name, "scheduled backup completed");
                }
            }
        }
        Err(e) => {
            printer
                .status(Role::Fail, subject)
                .detail(collapse_to_subject_line(&e));
            tracing::error!(
                backup = %spec.name,
                error = %e,
                "scheduled backup could not be recorded"
            );
        }
    }
}
