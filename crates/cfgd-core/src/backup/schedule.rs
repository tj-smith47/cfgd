//! When a `spec.backups[]` unit is next due.
//!
//! The grammar of `spec.backups[].schedule` and the rule for seeding it from a
//! unit's last recorded run live here rather than in the daemon, because two
//! surfaces need the same answer on different clocks: the daemon arms a
//! monotonic timer from it, and `cfgd backup list` renders the wall-clock time
//! an operator reads. One seeding rule, two clocks.

use std::time::Duration;

use chrono::{DateTime, Local};

/// Floor for an interval schedule. `parse_duration_str` accepts `0`, which
/// would turn the daemon loop's timer branch into a spin.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// How a scheduled backup decides when to fire next.
pub(crate) enum BackupSchedule {
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
    pub(crate) fn parse(schedule: &str) -> Option<Self> {
        if let Ok(d) = crate::parse_duration_str(schedule) {
            return Some(Self::Interval(d.max(MIN_INTERVAL)));
        }
        schedule
            .parse::<croner::Cron>()
            .ok()
            .map(|c| Self::Cron(Box::new(c)))
    }

    /// How long from `from_wall` until the FIRST fire, given the unit's last
    /// recorded run.
    ///
    /// An interval schedule is a cadence, not an uptime offset: `1d` means one
    /// snapshot a day, and arming it a full day out on every start means a
    /// laptop rebooted daily never reaches its own deadline. Seeding from the
    /// last recorded finish makes the cadence survive restarts — an overdue
    /// unit is due immediately (`ZERO`), an up-to-date one waits out the
    /// remainder.
    ///
    /// Cron is unaffected: its occurrences are absolute wall-clock times, so
    /// the next one is already the correct answer after any downtime. A cron
    /// unit that slept through its window waits for the next window, which is
    /// what the same line in a crontab does.
    pub(crate) fn first_delay(
        &self,
        from_wall: DateTime<Local>,
        last_finished: Option<&str>,
    ) -> Option<Duration> {
        let Self::Interval(period) = self else {
            return self.delay_after(from_wall);
        };
        let elapsed = last_finished
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            // A negative elapsed means the recorded finish is in the future
            // (the clock was stepped back, or the state dir came from another
            // machine); `to_std` rejects it and the full period is used.
            .and_then(|finished| {
                (from_wall.with_timezone(&chrono::Utc) - finished.with_timezone(&chrono::Utc))
                    .to_std()
                    .ok()
            });
        Some(match elapsed {
            Some(e) if e < *period => *period - e,
            Some(_) => Duration::ZERO,
            None => *period,
        })
    }

    /// How long from `from_wall` until this schedule's next occurrence,
    /// ignoring any recorded history.
    pub(crate) fn delay_after(&self, from_wall: DateTime<Local>) -> Option<Duration> {
        match self {
            Self::Interval(period) => Some(*period),
            Self::Cron(cron) => {
                let next_wall = cron.find_next_occurrence(&from_wall, false).ok()?;
                (next_wall - from_wall).to_std().ok()
            }
        }
    }
}

/// The wall-clock time a `spec.backups[]` unit is next due, as an ISO 8601 UTC
/// timestamp — the same shape a `backup_runs` row's `finished_at` carries, so
/// "last run" and "next run" read on one scale.
///
/// `last_finished` is the unit's most recent recorded `finished_at`, which
/// anchors an interval schedule exactly as it anchors the daemon's timer.
/// `None` when `schedule` is neither an interval nor a cron expression, or when
/// a cron expression has no upcoming occurrence at all.
pub fn next_run_at(schedule: &str, last_finished: Option<&str>) -> Option<String> {
    let parsed = BackupSchedule::parse(schedule)?;
    let now = Local::now();
    let delay = parsed.first_delay(now, last_finished)?;
    let next = now.checked_add_signed(chrono::TimeDelta::from_std(delay).ok()?)?;
    Some(crate::unix_secs_to_iso8601(
        u64::try_from(next.timestamp()).ok()?,
    ))
}

#[cfg(test)]
mod tests;
