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
use crate::errors::Result;
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
/// transient reason recovers on its own.
const SCHEDULE_STALL_RETRY: Duration = Duration::from_secs(3600);

/// How long a degraded resolution waits before re-resolving.
///
/// Long enough that a persistently broken profile does not re-parse every tick;
/// short enough that a source cache caught mid-rewrite restores its timers
/// within minutes rather than at the next daemon restart.
const RESOLVE_RETRY: Duration = Duration::from_secs(300);

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

    /// The first fire when the daemon starts, given the unit's last recorded
    /// run.
    ///
    /// An interval schedule is a cadence, not an uptime offset: `1d` means one
    /// snapshot a day, and arming it at `now + 1d` on every start means a
    /// laptop rebooted daily never reaches its own deadline. Seeding from the
    /// last recorded finish makes the cadence survive restarts — an overdue
    /// unit fires promptly, an up-to-date one waits out the remainder.
    ///
    /// Cron is unaffected: its occurrences are absolute wall-clock times, so
    /// the next one is already the correct answer after any downtime. A cron
    /// unit that slept through its window waits for the next window, which is
    /// what the same line in a crontab does.
    fn first_fire(&self, now: Instant, last_finished: Option<&str>) -> Option<Instant> {
        let Self::Interval(period) = self else {
            return self
                .next_after(now, Local::now())
                .map(|(instant, _)| instant);
        };
        let elapsed = last_finished
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            // A negative elapsed means the recorded finish is in the future
            // (the clock was stepped back, or the state dir came from another
            // machine); `to_std` rejects it and the full period is used.
            .and_then(|finished| {
                (chrono::Utc::now() - finished.with_timezone(&chrono::Utc))
                    .to_std()
                    .ok()
            });
        match elapsed {
            Some(e) if e < *period => Some(now + (*period - e)),
            Some(_) => Some(now),
            None => Some(now + *period),
        }
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
    ///
    /// `last_finished` is the unit's most recent recorded `finished_at`, which
    /// anchors an interval schedule across restarts — see
    /// [`BackupSchedule::first_fire`].
    pub(super) fn new(
        spec: &BackupSpec,
        profile_name: &str,
        now: Instant,
        last_finished: Option<&str>,
    ) -> Option<Self> {
        let schedule_str = spec.schedule.clone()?;
        let Some(schedule) = BackupSchedule::parse(&schedule_str) else {
            tracing::warn!(
                backup = %spec.name,
                schedule = %schedule_str,
                "backup timer: schedule is neither an interval nor a cron expression — no timer installed"
            );
            return None;
        };
        let next_fire = schedule.first_fire(now, last_finished).unwrap_or_else(|| {
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

    /// Hold the next fire back until at least `deadline`, leaving a later one
    /// alone. Never brings a fire forward, so a schedule can only ever be
    /// delayed by this, not accelerated past its own cadence.
    fn defer_until(&mut self, deadline: Instant) {
        self.next_fire = self.next_fire.max(deadline);
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
        // Wall time of the fire being consumed, reconstructed by subtracting
        // how long ago the deadline passed. `checked_sub_signed` on a
        // `DateTime<Local>` is arithmetic on the absolute instant, so a UTC
        // offset change inside the elapsed window (DST) does not distort the
        // result — the reconstructed local time is exactly the one that was on
        // the clock then, offset included. The one input it cannot see through
        // is an operator STEPPING the wall clock while the fire was pending,
        // which desynchronises the two clocks by the step; the cron search then
        // enumerates from a wall time the monotonic deadline never corresponded
        // to. That is bounded to the fires inside one advance and self-heals on
        // the next one, which re-reads both clocks.
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
///
/// `last_finished` answers "when did this unit last finish?" for one name; the
/// caller supplies it from the state store (or a stub in tests) so this module
/// never has to own a store handle.
pub(super) fn build_backup_tasks(
    specs: &[BackupSpec],
    profile_name: &str,
    now: Instant,
    last_finished: &dyn Fn(&str) -> Option<String>,
) -> Vec<BackupTask> {
    specs
        .iter()
        .filter_map(|spec| {
            BackupTask::new(
                spec,
                profile_name,
                now,
                last_finished(&spec.name).as_deref(),
            )
        })
        .collect()
}

/// What a rebuild did to the timer set, for the reload report.
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

/// A freshly resolved timer set plus whether the resolution was complete.
pub(super) struct ResolvedBackupTasks {
    pub(super) tasks: Vec<BackupTask>,
    /// The local profile resolved, but source composition did not, so `tasks`
    /// is the LOCALLY-DECLARED set — which can differ from the composed one for
    /// any unit a source overrides.
    pub(super) degraded: bool,
}

/// The daemon's scheduled-backup timers, plus the re-resolve deadline that a
/// degraded resolution arms.
///
/// The retry is what keeps a degraded set from being permanent: a source cache
/// caught mid-rewrite, or a profile edit saved half-written, resolves cleanly a
/// few minutes later and the full timer set comes back without a restart or a
/// second SIGHUP.
pub(crate) struct BackupTimers {
    tasks: Vec<BackupTask>,
    retry_at: Option<Instant>,
}

impl BackupTimers {
    /// Adopt a startup resolution. Unlike a reload there is no prior set to
    /// preserve, so a degraded resolution IS installed — a machine whose source
    /// cache is unreadable at boot still takes its locally-declared backups
    /// rather than none at all.
    ///
    /// Its first fires are pushed past the retry deadline, though: until the
    /// retry either confirms or replaces these specs, a unit a source overrides
    /// may be carrying the local `destination`, and a run against the wrong
    /// destination also prunes — the retention pass drops every recorded run
    /// whose snapshot no longer sits under the spec's destination, taking the
    /// source-era history with it. Deferring the first fire by one retry window
    /// closes that window for a transient failure. It is deliberately a
    /// one-shot deferral: if the retry is still degraded the timers run anyway,
    /// because a permanently unreadable source must not mean a machine that
    /// never backs up.
    pub(super) fn new(resolved: ResolvedBackupTasks, now: Instant) -> Self {
        let ResolvedBackupTasks {
            mut tasks,
            degraded,
        } = resolved;
        let retry_at = degraded.then(|| now + RESOLVE_RETRY);
        if let Some(deadline) = retry_at {
            for task in tasks.iter_mut() {
                task.defer_until(deadline);
            }
        }
        Self { tasks, retry_at }
    }

    /// An empty set with no retry armed — the shape used when the daemon could
    /// not resolve a profile at all.
    pub(super) fn empty() -> Self {
        Self {
            tasks: Vec::new(),
            retry_at: None,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Individual timers, for asserting which units survived a reload and what
    /// deadline each carries. The loop itself never needs them — it works
    /// through [`Self::next_deadline`] and [`Self::take_due`].
    #[cfg(test)]
    pub(super) fn tasks(&self) -> &[BackupTask] {
        &self.tasks
    }

    pub(super) fn is_degraded(&self) -> bool {
        self.retry_at.is_some()
    }

    /// The soonest thing the loop must wake for: a fire, or the re-resolve.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.tasks
            .iter()
            .map(BackupTask::next_fire)
            .chain(self.retry_at)
            .min()
    }

    pub(super) fn retry_due(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|at| at <= now)
    }

    /// Schedule another re-resolve. Used when the resolution could not even be
    /// attempted (config load / profile failure), where there is no
    /// `ResolvedBackupTasks` to hand to [`Self::apply_resolved`].
    pub(super) fn arm_retry(&mut self, now: Instant) {
        self.retry_at = Some(now + RESOLVE_RETRY);
    }

    /// Consume every fire that is due, returning the units to run.
    pub(super) fn take_due(&mut self, now: Instant) -> Vec<(String, BackupSpec)> {
        let mut due = Vec::new();
        for task in self.tasks.iter_mut() {
            if !task.is_due(now) {
                continue;
            }
            let missed = task.advance(now);
            if missed > 0 {
                tracing::warn!(
                    backup = %task.spec.name,
                    missed_fires = missed,
                    "backup: schedule elapsed while the daemon was busy — skipped the missed fire(s)"
                );
            }
            due.push((task.profile_name.clone(), task.spec.clone()));
        }
        due
    }

    /// Adopt a re-resolution (SIGHUP or retry).
    ///
    /// A DEGRADED resolution is refused: swapping the local-only set in would
    /// silently retire every source-delivered timer ("N removed") and could
    /// substitute a different spec for a unit a source overrides, and the
    /// running set is strictly better evidence of the machine's intent than a
    /// set built from half the inputs. The retry is re-armed instead, so a
    /// transient failure still converges without operator action.
    pub(super) fn apply_resolved(
        &mut self,
        resolved: ResolvedBackupTasks,
        now: Instant,
    ) -> Option<BackupReloadSummary> {
        if resolved.degraded {
            self.retry_at = Some(now + RESOLVE_RETRY);
            return None;
        }
        self.retry_at = None;
        Some(reload_backup_tasks(&mut self.tasks, resolved.tasks))
    }
}

/// Resolve the machine's scheduled backups into a fresh timer set.
///
/// Composition is cache-only (the sync task owns fetch cadence), so a
/// source-delivered backup gets a timer like a locally-declared one.
///
/// `Err` means the local profile itself did not resolve — there is no
/// trustworthy set at all, and the caller decides whether that means "start
/// with none" (boot) or "keep what is running" (reload). A compose failure is
/// softer: it returns the locally-declared set marked `degraded`, because a
/// backup only ever writes into its own destination, so running the local set
/// cannot damage source-delivered state the way the pruning reconcile could.
/// The `degraded` flag is what stops that softness from becoming permanent —
/// see [`BackupTimers`].
pub(super) fn resolve_backup_tasks(
    cfg: &CfgdConfig,
    config_path: &Path,
    profile_override: Option<&str>,
    printer: &Printer,
    scope: crate::Scope,
    state_dir: Option<&Path>,
    now: Instant,
) -> Result<ResolvedBackupTasks> {
    let profiles_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profiles");
    let profile_name = profile_override
        .or(cfg.spec.profile.as_deref())
        .unwrap_or("default");

    let local = config::resolve_profile(profile_name, &profiles_dir)?;

    let (specs, degraded) = match super::compose_daemon_desired_state(cfg, &local, printer, scope) {
        Ok((resolved, _)) => (resolved.merged.backups, false),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "backup timers: source composition failed — falling back to locally-declared backups"
            );
            (local.merged.backups.clone(), true)
        }
    };

    // A state-store failure only costs the restart-seeding of interval
    // schedules (they fall back to a full period from now), so it is a warn,
    // not a resolution failure.
    let store = state_dir.and_then(|dir| match StateStore::open_in_dir(dir) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "backup timers: state store unavailable — interval schedules restart from now"
            );
            None
        }
    });
    let last_finished = |name: &str| {
        store
            .as_ref()
            .and_then(|s| s.latest_backup_run(name).ok().flatten())
            .map(|record| record.finished_at)
    };

    Ok(ResolvedBackupTasks {
        tasks: build_backup_tasks(&specs, profile_name, now, &last_finished),
        degraded,
    })
}

/// Run one scheduled backup. Blocking; the loop dispatches it through
/// `spawn_blocking_with_test_home`.
///
/// Failures never propagate: `run_backup` records an operational failure in the
/// returned row, and the `Err` arm (a state-store write) is a storage problem
/// for one unit, not a reason to take the daemon's timer branch down.
///
/// `abort` is the daemon's shutdown flag. Without it a `preBackup` hook that
/// stops a database keeps running after SIGTERM until its own timeout, so a
/// `systemctl stop cfgd` (or a container's stop grace period) waits minutes on
/// a hook nobody is going to consume the result of.
pub(super) fn run_scheduled_backup(
    spec: &BackupSpec,
    config_dir: &Path,
    profile_name: &str,
    state_dir: &Path,
    printer: &Printer,
    abort: &crate::AbortFlag,
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
        .with_context(ReconcileContext::Reconcile)
        .with_abort(abort);
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
        // A hand-run (`cfgd backup run`) or an apply is inside this unit right
        // now. The engine allows one writer per unit, and the fire is dropped
        // rather than queued: by the time the other run finishes, the snapshot
        // it took is the one this fire would have duplicated.
        Err(crate::errors::CfgdError::Backup(crate::errors::BackupError::Busy {
            holder, ..
        })) => {
            printer
                .status(Role::Skipped, subject)
                .detail(format!("already running ({holder})"));
            tracing::info!(
                backup = %spec.name,
                holder = %holder,
                "scheduled backup skipped: the unit is already running elsewhere"
            );
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
