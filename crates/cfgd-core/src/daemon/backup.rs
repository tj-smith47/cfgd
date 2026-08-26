// Scheduled `spec.backups[]` timers for the daemon loop.
//
// This module owns SCHEDULING only. A fire dispatches the same
// `crate::backup::run_backup` a `cfgd backup run` does, against the same
// `BackupUnit`, so a scheduled run and a CLI-triggered run write identical
// `backup_runs` rows, honour the same retention, and run the same hooks.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

use crate::backup::BackupUnit;
use crate::backup::schedule::BackupSchedule;
use crate::config::{self, BackupSpec, CfgdConfig};
use crate::errors::Result;
use crate::output::{Printer, Role};
use crate::reconciler::ReconcileContext;
use crate::state::StateStore;

/// How many missed fires `BackupTask::advance` walks before jumping straight to
/// the next fire after now. A per-second schedule and a daemon that was blocked
/// for a day would otherwise be walked one occurrence at a time.
const MAX_MISSED_CATCHUP: u32 = 1000;

/// Deadline used when a schedule cannot produce a next occurrence at all. Long
/// enough not to spin, short enough that a schedule which only fails for a
/// transient reason recovers on its own.
const SCHEDULE_STALL_RETRY: Duration = Duration::from_secs(3600);

/// The `Trigger` row a scheduled fire's header carries — what woke this run,
/// in the same slot the reconcile tick names its drift trigger in.
const SCHEDULE_TRIGGER: &str = "schedule";

/// How long a degraded resolution waits before re-resolving.
///
/// Long enough that a persistently broken profile does not re-parse every tick;
/// short enough that a source cache caught mid-rewrite restores its timers
/// within minutes rather than at the next daemon restart.
const RESOLVE_RETRY: Duration = Duration::from_secs(300);

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
    /// [`BackupSchedule::first_delay`].
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
                "daemon: backup schedule is neither an interval nor a cron expression — no timer installed"
            );
            return None;
        };
        let next_fire = schedule
            .first_delay(Local::now(), last_finished)
            .map(|delay| now + delay)
            .unwrap_or_else(|| {
                tracing::warn!(
                    backup = %spec.name,
                    schedule = %schedule_str,
                    "daemon: backup schedule has no upcoming occurrence — retrying later"
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
        let delay = self.schedule.delay_after(from_wall)?;
        let wall = from_wall.checked_add_signed(chrono::TimeDelta::from_std(delay).ok()?)?;
        Some((from + delay, wall))
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
            let Some((next, next_wall)) = self.next_after(cursor, cursor_wall) else {
                tracing::warn!(
                    backup = %self.spec.name,
                    schedule = %self.schedule_str,
                    "daemon: backup schedule has no upcoming occurrence — retrying later"
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
    /// `Some` when the local profile resolved but source composition did not,
    /// so `tasks` is the LOCALLY-DECLARED set — which can differ from the
    /// composed one for any unit a source overrides. Carrying the reason rather
    /// than a bool keeps the bool-to-reason mapping from being re-made (and
    /// drifting) at each site that installs the set.
    pub(super) degraded: Option<DegradedReason>,
}

/// Why a timer set is degraded. The three have different blast radii and
/// different operator remedies, so the startup banner names which one it is
/// rather than reporting one degraded state that could mean any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DegradedReason {
    /// The top-level config file itself would not load, so the profile was
    /// never even reached.
    ConfigUnreadable,
    /// The profile itself would not resolve, so there are no timers at all.
    ProfileUnresolved,
    /// The profile resolved but source composition did not, so the set on hand
    /// is the locally-declared one and may be missing source-delivered units.
    SourcesUnavailable,
}

impl DegradedReason {
    /// Suffix appended to the banner's `backups=` count.
    pub(super) fn banner_note(self) -> &'static str {
        match self {
            Self::ConfigUnreadable => " (config unreadable)",
            Self::ProfileUnresolved => " (profile unresolved)",
            Self::SourcesUnavailable => " (source composition unavailable)",
        }
    }
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
    /// Set together with `degraded` — a retry exists exactly when the set is
    /// degraded, and never otherwise.
    retry_at: Option<Instant>,
    degraded: Option<DegradedReason>,
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
        let retry_at = degraded.map(|_| now + RESOLVE_RETRY);
        if let Some(deadline) = retry_at {
            for task in tasks.iter_mut() {
                task.defer_until(deadline);
            }
        }
        Self {
            tasks,
            retry_at,
            degraded,
        }
    }

    /// An empty set with no retry armed. Production never builds one — every
    /// startup path either resolves a set or arms a retry via
    /// [`Self::empty_with_retry`] — so it exists as the neutral fixture for
    /// loop tests that are not about backups at all.
    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self {
            tasks: Vec::new(),
            retry_at: None,
            degraded: None,
        }
    }

    /// An empty set that WILL re-resolve — the shape used when the daemon could
    /// not resolve a profile at all.
    ///
    /// Startup is the one moment with no prior set to fall back on, so the
    /// failure costs every timer. A profile saved mid-edit, or one a source the
    /// sync task is about to fetch still owes, must not leave the daemon
    /// permanently backup-less until someone notices and restarts it.
    pub(super) fn empty_with_retry(now: Instant) -> Self {
        Self {
            tasks: Vec::new(),
            retry_at: Some(now + RESOLVE_RETRY),
            degraded: Some(DegradedReason::ProfileUnresolved),
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

    /// Whether the set is degraded at all. Production reads
    /// [`Self::degraded_reason`] instead, because the banner names the cause;
    /// tests that only care that a retry was armed use this.
    #[cfg(test)]
    pub(super) fn is_degraded(&self) -> bool {
        self.degraded.is_some()
    }

    /// What is missing, when the set is degraded.
    pub(super) fn degraded_reason(&self) -> Option<DegradedReason> {
        self.degraded
    }

    /// Role and suffix for a line reporting a schedule change, given the state
    /// the set is in AFTER that change.
    ///
    /// A set that adopted a partial resolution has changed for the better and
    /// is still not an all-clear: the retry is armed, and once the one-shot
    /// first-fire deferral expires, a unit a source overrides runs against the
    /// LOCAL destination and its prune drops the source-era retention rows. A
    /// bare `✓ ... restored: 2 scheduled` would have the terminal reporting
    /// everything is fine while exactly that is queued up, so the same
    /// qualifier the startup banner carries rides these lines too — from one
    /// place, so the two cannot drift apart.
    pub(super) fn reload_line_qualifier(&self) -> (Role, &'static str) {
        match self.degraded {
            Some(reason) => (Role::Warn, reason.banner_note()),
            None => (Role::Ok, ""),
        }
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
    pub(super) fn arm_retry(&mut self, now: Instant, reason: DegradedReason) {
        self.retry_at = Some(now + RESOLVE_RETRY);
        self.degraded = Some(reason);
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
                    "daemon: backup schedule elapsed while the daemon was busy — skipped the missed {}",
                    crate::plural_noun(missed as usize, "fire")
                );
            }
            due.push((task.profile_name.clone(), task.spec.clone()));
        }
        due
    }

    /// Adopt a re-resolution (SIGHUP or retry).
    ///
    /// A DEGRADED resolution is refused while timers are running: swapping the
    /// local-only set in would silently retire every source-delivered timer
    /// ("N removed") and could substitute a different spec for a unit a source
    /// overrides, and the running set is strictly better evidence of the
    /// machine's intent than a set built from half the inputs. The retry is
    /// re-armed instead, so a transient failure still converges without
    /// operator action.
    ///
    /// With NO timers running there is nothing to protect, so a degraded
    /// resolution is adopted on exactly the terms [`Self::new`] adopts one at
    /// startup — first fires deferred one retry window, retry still armed.
    /// Refusing here instead would pin a daemon that booted with an
    /// unresolvable profile at zero timers for as long as composition stays
    /// down, which is the sticky-empty failure the retry exists to end.
    pub(super) fn apply_resolved(
        &mut self,
        resolved: ResolvedBackupTasks,
        now: Instant,
    ) -> Option<BackupReloadSummary> {
        if let Some(reason) = resolved.degraded {
            if self.tasks.is_empty() {
                *self = Self::new(resolved, now);
                return (!self.tasks.is_empty()).then_some(BackupReloadSummary {
                    added: self.tasks.len(),
                    removed: 0,
                    rescheduled: 0,
                });
            }
            self.arm_retry(now, reason);
            return None;
        }
        self.retry_at = None;
        self.degraded = None;
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
    let (profiles_dir, profile_name) = super::profile_context(config_path, cfg, profile_override);

    let local = config::resolve_profile(profile_name, &profiles_dir)?;

    let (specs, degraded) = match super::compose_daemon_desired_state(cfg, &local, printer, scope) {
        Ok(composed) => (composed.resolved.merged.backups, None),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "daemon: backup timers — source composition failed, falling back to locally-declared backups"
            );
            (
                local.merged.backups.clone(),
                Some(DegradedReason::SourcesUnavailable),
            )
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
                "daemon: backup timers — state store unavailable, interval schedules restart from now"
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

/// Run every due scheduled backup as ONE run: a `Backup` header, the `Backups`
/// pseudo-phase with a `backup:<name>` group per unit, and a rollup.
///
/// Blocking; the loop dispatches it through `spawn_blocking_with_test_home`.
/// The rendering itself belongs to [`crate::backup::run_backup_group`], which
/// `cfgd backup run` and apply's pending backups also call — a scheduled fire
/// that looked different from a hand-run would be the same unit described two
/// ways. What stays here is the daemon's own concerns: opening the state store
/// and the `tracing` lines a background run leaves behind for an operator who
/// was not watching the terminal.
///
/// Failures never propagate: an operational failure lands in the unit's row,
/// and a state-store failure is one unit's storage problem rather than a reason
/// to take the daemon's timer branch down.
///
/// `abort` is the daemon's shutdown flag. Without it a `preBackup` hook that
/// stops a database keeps running after SIGTERM until its own timeout, so a
/// `systemctl stop cfgd` (or a container's stop grace period) waits minutes on
/// a hook nobody is going to consume the result of.
pub(super) fn run_scheduled_backups(
    due: &[(String, BackupSpec)],
    config_path: &Path,
    config_dir: &Path,
    state_dir: &Path,
    printer: &Printer,
    abort: &crate::AbortFlag,
) {
    if due.is_empty() {
        return;
    }
    let store = match StateStore::open_in_dir(state_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                error = %e,
                "daemon: scheduled backup state store error — runs skipped"
            );
            return;
        }
    };

    let units: Vec<BackupUnit<'_>> = due
        .iter()
        .map(|(profile_name, spec)| {
            BackupUnit::new(spec, config_dir, profile_name, state_dir)
                .with_context(ReconcileContext::Reconcile)
                .with_abort(abort)
        })
        .collect();

    // A due set can span profiles — one timer set covers the whole config — and
    // the header has one profile row, so naming the first unit's profile would
    // attribute every other unit to a profile that never declared it. A
    // heterogeneous set omits the row instead.
    let single_profile = due
        .first()
        .map(|(profile, _)| profile.as_str())
        .filter(|first| due.iter().all(|(profile, _)| profile == first));

    let ctx = crate::reconciler::RunContext {
        title: crate::reconciler::RunTitle::Backup,
        config_path: Some(config_path),
        profile: single_profile,
        sources: &[],
        modules: &[],
        trigger: Some(SCHEDULE_TRIGGER),
        subject: None,
        unit_source: None,
    };
    let run = crate::reconciler::ApplyRun::backups(ctx, &units, &store);
    let reports = match run.execute_backups(printer) {
        Ok((_, reports)) => reports,
        Err(e) => {
            tracing::error!(error = %e, "daemon: scheduled backup run could not be rendered");
            return;
        }
    };

    // The reports the run just handed back, never a re-read of the store: a
    // `latest_backup_run` lookup returns whatever ran LAST, so a unit skipped
    // for a held lock but backed up an hour ago would be journalled as a run
    // that completed here and did not.
    debug_assert_eq!(
        due.len(),
        reports.len(),
        "one report per due unit, in unit order"
    );
    for ((_, spec), report) in due.iter().zip(&reports) {
        match (&report.skipped, &report.error, &report.record) {
            (Some(holder), _, _) => {
                tracing::info!(
                    "daemon: scheduled backup {} skipped — already running under {holder}",
                    spec.name
                );
            }
            (None, Some(e), _) => {
                tracing::warn!(backup = %spec.name, error = %e, "daemon: scheduled backup run could not be recorded");
            }
            (None, None, Some(record)) => match &record.error {
                Some(e) => {
                    tracing::warn!(backup = %record.name, error = %e, "daemon: scheduled backup completed with errors");
                }
                None => {
                    tracing::info!("daemon: scheduled backup {} completed", record.name);
                }
            },
            // Unreachable while `run_backup_group` fills exactly one of the
            // three: a report with none of them is a unit whose outcome was
            // lost, which is worth a line rather than silence.
            (None, None, None) => {
                tracing::warn!(backup = %spec.name, "daemon: scheduled backup produced no outcome");
            }
        }
    }
}
