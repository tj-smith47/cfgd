use std::time::Duration;

use chrono::{Local, TimeZone};

use super::{BackupSchedule, next_run_at};

#[test]
fn parse_accepts_intervals_and_both_cron_arities() {
    assert!(matches!(
        BackupSchedule::parse("6h"),
        Some(BackupSchedule::Interval(d)) if d == Duration::from_secs(21_600)
    ));
    assert!(matches!(
        BackupSchedule::parse("0 3 * * *"),
        Some(BackupSchedule::Cron(_))
    ));
    assert!(matches!(
        BackupSchedule::parse("30 0 3 * * *"),
        Some(BackupSchedule::Cron(_))
    ));
    assert!(BackupSchedule::parse("every tuesday").is_none());
}

/// The machine's parser and the cluster's admission grammar answer the same
/// question about the same string.
///
/// `BackupSchedule::parse` returns the parsed value rather than a verdict, so
/// it cannot delegate to `validate_backup_schedule_grammar`; the two spell the
/// same two arms instead. A schedule admission accepts but the daemon cannot
/// parse is a unit that never runs, and one the daemon parses but admission
/// refuses is a policy the cluster rejects for no reason.
#[test]
fn every_schedule_the_grammar_admits_is_one_the_machine_parses() {
    for schedule in [
        "6h",
        "30s",
        "90",
        "0",
        "0 3 * * *",
        "30 0 3 * * *",
        "*/15 * * * *",
        "@daily",
        "",
        "  ",
        "nightly",
        "0 3 * *",
        "every day",
        "every tuesday",
    ] {
        assert_eq!(
            BackupSchedule::parse(schedule).is_some(),
            cfgd_schema::validate_backup_schedule_grammar(schedule).is_ok(),
            "the parser and the admission grammar disagree about {schedule:?}"
        );
    }
}

#[test]
fn parse_floors_a_zero_interval() {
    // `parse_duration_str` accepts `0`, which would spin the daemon's timer.
    assert!(matches!(
        BackupSchedule::parse("0"),
        Some(BackupSchedule::Interval(d)) if d == Duration::from_secs(1)
    ));
}

#[test]
fn an_interval_is_seeded_from_the_last_recorded_run() {
    let period = Duration::from_secs(3600);
    let schedule = BackupSchedule::Interval(period);
    let now = fixed_now();

    // Half a period elapsed → the remainder is what is left.
    let half_ago = (now - chrono::TimeDelta::minutes(30)).to_rfc3339();
    let delay = schedule
        .first_delay(now, Some(&half_ago))
        .expect("an interval always yields a delay");
    assert!(
        delay <= Duration::from_secs(1801) && delay >= Duration::from_secs(1799),
        "half a period elapsed must leave half a period: {delay:?}"
    );

    // Overdue → due now, not a full period out.
    let long_ago = (now - chrono::TimeDelta::hours(9)).to_rfc3339();
    assert_eq!(
        schedule.first_delay(now, Some(&long_ago)),
        Some(Duration::ZERO),
        "an overdue unit must be due immediately"
    );

    // No recorded run → a full period.
    assert_eq!(
        schedule.first_delay(now, None),
        Some(period),
        "a unit that never ran waits one full period"
    );

    // A finish in the future (clock stepped back, or a state dir from another
    // machine) must not produce a negative or absurd delay.
    let future = (now + chrono::TimeDelta::hours(4)).to_rfc3339();
    assert_eq!(
        schedule.first_delay(now, Some(&future)),
        Some(period),
        "a future finish falls back to a full period"
    );
}

#[test]
fn a_cron_schedule_ignores_the_recorded_run() {
    // Cron occurrences are absolute wall-clock times: a unit that slept through
    // its window waits for the next window, exactly like a crontab line.
    let schedule = BackupSchedule::parse("0 3 * * *").expect("cron parses");
    let now = fixed_now();
    let long_ago = (now - chrono::TimeDelta::days(9)).to_rfc3339();

    assert_eq!(
        schedule.first_delay(now, Some(&long_ago)),
        schedule.first_delay(now, None),
        "a recorded run must not move a cron occurrence"
    );
}

#[test]
fn next_run_at_renders_an_iso8601_utc_stamp() {
    let rendered = next_run_at("0 3 * * *", None).expect("a cron schedule has a next occurrence");
    assert_eq!(rendered.len(), 20, "ISO 8601 UTC shape: {rendered}");
    assert!(
        rendered.ends_with('Z') && rendered.contains('T'),
        "must match the shape a backup_runs finished_at carries: {rendered}"
    );
    assert!(
        rendered > crate::utc_now_iso8601(),
        "the next run is in the future: {rendered}"
    );
}

#[test]
fn next_run_at_is_none_for_an_unparseable_schedule() {
    assert_eq!(next_run_at("every tuesday", None), None);
}

/// A fixed local wall time, so the interval arithmetic under test is not racing
/// the clock.
fn fixed_now() -> chrono::DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 5, 12, 14, 30, 25)
        .single()
        .expect("a fixed, unambiguous local timestamp")
}
