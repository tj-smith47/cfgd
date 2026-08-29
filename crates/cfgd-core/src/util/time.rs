/// Returns the current UTC time as an ISO 8601 / RFC 3339 string.
pub fn utc_now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    unix_secs_to_iso8601(secs)
}

/// Strip filename-unsafe characters (`:`, `-`, `T`, `Z`) from an ISO 8601
/// timestamp so it can be used as a path segment. Helper extracted from three
/// inline replace calls in oci/build, cli/module/keys, and gateway/api/drift.
pub fn iso8601_to_filename_safe(ts: &str) -> String {
    ts.replace([':', '-', 'T', 'Z'], "")
}

/// Convenience: current UTC time as a filename-safe string.
pub fn utc_now_filename_safe() -> String {
    iso8601_to_filename_safe(&utc_now_iso8601())
}

/// Returns the current time as seconds since the Unix epoch.
pub fn unix_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Converts a Unix timestamp (seconds since epoch) to an ISO 8601 UTC string.
pub fn unix_secs_to_iso8601(secs: u64) -> String {
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// `strftime`-style UTC timestamp format for `spec.backups[]` snapshot
/// filenames (rendered via the `{timestamp}` `namePattern` variable). Fixed
/// here so the backup engine and the schema/docs agree on one format without
/// re-deriving it.
pub const BACKUP_TIMESTAMP_FORMAT: &str = "%Y%m%dT%H%M%SZ";

/// Render a Unix timestamp in [`BACKUP_TIMESTAMP_FORMAT`]
/// (`20260512T143025Z`) — the `{timestamp}` `namePattern` variable.
///
/// Derived from [`unix_secs_to_iso8601`] rather than a second date algorithm:
/// the two formats differ only in the `:`/`-` separators, so folding them out
/// keeps one calendar implementation in the crate.
pub fn unix_secs_to_backup_stamp(secs: u64) -> String {
    unix_secs_to_iso8601(secs).replace([':', '-'], "")
}

/// Convenience: the current UTC time in [`BACKUP_TIMESTAMP_FORMAT`].
pub fn utc_now_backup_stamp() -> String {
    unix_secs_to_backup_stamp(unix_secs_now())
}

/// Parse a duration string like "30s", "5m", "1h", or a plain number (as seconds).
///
/// Returns an error description on invalid input.
pub fn parse_duration_str(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    const SUFFIXES: &[(char, u64)] = &[('s', 1), ('m', 60), ('h', 3600), ('d', 86400)];
    for &(suffix, multiplier) in SUFFIXES {
        if let Some(n) = s.strip_suffix(suffix) {
            return n
                .trim()
                .parse::<u64>()
                .map(|v| std::time::Duration::from_secs(v * multiplier))
                .map_err(|_| format!("invalid timeout: {}", s));
        }
    }
    s.parse::<u64>()
        .map(std::time::Duration::from_secs)
        .map_err(|_| format!("invalid timeout '{}': use 30s, 5m, or 1h", s))
}

/// Render the age of an ISO 8601 timestamp relative to `now` as a short
/// "Xs ago" / "Xm ago" / "Xh ago" / "Xd ago" string, or `None` when `ts` or `now` fails
/// to parse, or `ts` names an instant after `now` (clock skew) — the caller
/// then omits the age line rather than showing a negative duration. `now` is
/// a parameter (never read from the wall clock here) so a caller can pin it
/// for a golden or unit test instead of the render depending on when the test
/// happened to run.
///
/// The sub-minute bucket is `"{secs}s ago"`, not `"just now"`: a daemon
/// advertising `sync every 5s` and stamping its own log to the second had a
/// dashboard that could not resolve anything it did, because every field
/// floored to one phrase. `just now` survives under five seconds, where the
/// number would be noise and the reader means "this is happening".
///
/// Both timestamps are strings — the callers of this
/// function (the `cfgd` binary crate's display paths) never hold a `chrono`
/// type of their own, only the ISO 8601 strings `cfgd-core` already hands
/// back from `utc_now_iso8601()` and the state store.
pub fn humanize_age_since(ts: &str, now: &str) -> Option<String> {
    let secs = age_since_secs(ts, now)?;
    if secs < 0 {
        return None;
    }
    Some(if secs < 5 {
        "just now".to_string()
    } else {
        format!("{} ago", span_magnitude(secs))
    })
}

/// The ONE bucketing of a span into `Ns` / `Nm` / `Nh` / `Nd`, read by the
/// backward twin, the forward twin and the magnitude cell, so one instant
/// cannot be `59s ago` in one column and `1m` in another.
fn span_magnitude(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// The forward counterpart of [`humanize_age_since`]: how long until `ts`,
/// as `"in 40s"` / `"in 5m"` / `"in 3h"` / `"in 2d"`, or `"due now"` inside the
/// last five seconds — the same buckets its backward twin reads, so one instant
/// cannot be `59s ago` in one column and `due now` in another.
///
/// `None` when `ts` or `now` fails to parse, or when `ts` is already PAST —
/// "in -2h" is not a thing a schedule column may say, and a caller holding an
/// overdue instant reaches for `humanize_age_since` to word it as an age.
/// Shares `age_since_secs` with its backward twin, so the two cannot disagree
/// about where an hour ends.
pub fn humanize_until(ts: &str, now: &str) -> Option<String> {
    let secs = -age_since_secs(ts, now)?;
    if secs < 0 {
        return None;
    }
    Some(if secs < 5 {
        "due now".to_string()
    } else {
        format!("in {}", span_magnitude(secs))
    })
}

/// A human listing column reporting a RECORDED PAST instant: its age, or
/// `never` when there is no record.
///
/// An ISO 8601 instant answers "when exactly", which is a question a machine
/// consumer asks — it stays in the `-o json` payload. A person scanning a
/// listing is asking "how stale is this". A stamp too malformed or too far in
/// the future to subtract falls back to ITSELF rather than to `never`: it IS a
/// record, just not one whose age can be stated.
pub fn humanize_age_cell(ts: Option<&str>, now: &str) -> String {
    match ts {
        Some(ts) => humanize_age_since(ts, now).unwrap_or_else(|| ts.to_string()),
        None => "never".to_string(),
    }
}

/// The MAGNITUDE half of [`humanize_age_cell`], for a slot whose KEY or HEADER
/// is already the dimension (`Age`): `3m`, not `3m ago`. `Age  3m ago` spent
/// the value restating the pastness its key asserts, and lost the property
/// that makes `kubectl get`'s `AGE` scannable — every cell a bare magnitude of
/// one shape. A key naming the EVENT (`Last Applied`, `Created`, `Last Sync`)
/// keeps [`humanize_age_cell`]: there the value says how far back the moment
/// is. Absence still reads `never`; an unsubtractable stamp still falls back
/// to itself; the sub-five-second bucket reads `0s`–`4s` rather than `just
/// now`, a magnitude having no sentence to be.
pub fn humanize_age_magnitude_cell(ts: Option<&str>, now: &str) -> String {
    match ts {
        Some(ts) => age_since_secs(ts, now)
            .filter(|secs| *secs >= 0)
            .map(span_magnitude)
            .unwrap_or_else(|| ts.to_string()),
        None => "never".to_string(),
    }
}

/// The forward twin of [`humanize_age_cell`], for a column reporting a FUTURE
/// instant: how long until it, or `-` when nothing is scheduled.
///
/// `-` rather than `never`: a unit with no next occurrence has not failed to do
/// anything, so the cell says "not known", the one thing `-` means everywhere
/// else in a cfgd table. An instant already past falls back to itself, the same
/// way an unsubtractable stamp does.
pub fn humanize_until_cell(ts: Option<&str>, now: &str) -> String {
    match ts {
        Some(ts) => humanize_until(ts, now).unwrap_or_else(|| ts.to_string()),
        None => crate::ABSENT.to_string(),
    }
}

/// A MEASURED duration a human reads, as `"44s"` / `"1m 44s"` / `"2h 3m"` /
/// `"3d 2h"` — the duration counterpart of [`humanize_age_cell`].
///
/// `cfgd daemon status` printed `Uptime 259200s` one row above
/// `Last Reconcile 3d ago`, so one kv block answered the same kind of question
/// in two vocabularies. Every measured duration a person reads goes through
/// here; a DECLARED interval (`Reconcile Interval 30s`) is the operator's own
/// literal and stays verbatim.
///
/// Two units at most, largest first, and the second is dropped when it is
/// zero — `2h` rather than `2h 0m`. Zero itself renders `0s`, which is what a
/// daemon that started this instant has been up.
pub fn humanize_duration_secs(secs: u64) -> String {
    const DAY: u64 = 86_400;
    const HOUR: u64 = 3_600;
    const MINUTE: u64 = 60;
    let (major_div, major_unit, minor_div, minor_unit) = if secs >= DAY {
        (DAY, 'd', HOUR, 'h')
    } else if secs >= HOUR {
        (HOUR, 'h', MINUTE, 'm')
    } else if secs >= MINUTE {
        (MINUTE, 'm', 1, 's')
    } else {
        return format!("{secs}s");
    };
    let major = secs / major_div;
    match (secs % major_div) / minor_div {
        0 => format!("{major}{major_unit}"),
        minor => format!("{major}{major_unit} {minor}{minor_unit}"),
    }
}

/// The signed seconds between `ts` and `now` (positive when `ts` is in the
/// past), or `None` when either fails to parse as RFC 3339. The shared
/// primitive behind `humanize_age_since`'s rendering, `humanize_until`'s and
/// `is_stale_since`'s threshold check, so the three can never disagree about
/// what "the age of `ts`" means.
fn age_since_secs(ts: &str, now: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(ts)
        .ok()?
        .with_timezone(&chrono::Utc);
    let now = chrono::DateTime::parse_from_rfc3339(now)
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(now.signed_duration_since(then).num_seconds())
}

/// Whether `ts` is more than `threshold_secs` older than `now`. An
/// unparseable `ts` (or `now`) reads as stale rather than fresh — a caller
/// deciding whether to show a "this is old, go check" hint must not suppress
/// it just because the timestamp it read was malformed.
pub fn is_stale_since(ts: &str, now: &str, threshold_secs: i64) -> bool {
    age_since_secs(ts, now).is_none_or(|secs| secs > threshold_secs)
}

#[cfg(test)]
mod tests {
    /// The magnitude cell and the relation cell read one bucketing: every
    /// magnitude is the relation minus its ` ago`, absence and an
    /// unsubtractable stamp read the same on both, and the magnitude never
    /// carries a relation word.
    #[test]
    fn the_magnitude_cell_is_the_relation_cell_minus_its_relation_word() {
        let now = "2026-05-14T12:00:00Z";
        for ts in [
            "2026-05-14T11:59:58Z",
            "2026-05-14T11:59:20Z",
            "2026-05-14T11:57:00Z",
            "2026-05-14T09:00:00Z",
            "2026-05-11T12:00:00Z",
        ] {
            let relation = super::humanize_age_cell(Some(ts), now);
            let magnitude = super::humanize_age_magnitude_cell(Some(ts), now);
            assert!(!magnitude.ends_with(" ago"), "{magnitude}");
            if let Some(span) = relation.strip_suffix(" ago") {
                assert_eq!(magnitude, span);
            } else {
                assert_eq!(relation, "just now");
                assert_eq!(magnitude, "2s");
            }
        }
        assert_eq!(super::humanize_age_magnitude_cell(None, now), "never");
        assert_eq!(
            super::humanize_age_magnitude_cell(Some("not a stamp"), now),
            "not a stamp"
        );
        assert_eq!(
            super::humanize_age_magnitude_cell(Some("2026-05-14T13:00:00Z"), now),
            "2026-05-14T13:00:00Z",
            "an instant after `now` is unsubtractable and falls back to itself"
        );
    }

    use super::*;

    #[test]
    fn iso8601_to_filename_safe_strips_separators() {
        assert_eq!(
            iso8601_to_filename_safe("2026-05-12T14:30:25Z"),
            "20260512143025"
        );
    }

    #[test]
    fn iso8601_to_filename_safe_preserves_fractional_seconds() {
        // Only `:`, `-`, `T`, `Z` are stripped — `.` and digits survive.
        assert_eq!(
            iso8601_to_filename_safe("2026-05-12T14:30:25.123Z"),
            "20260512143025.123"
        );
    }

    #[test]
    fn unix_secs_to_backup_stamp_matches_the_pinned_format() {
        // 2026-05-12T14:30:25Z
        assert_eq!(unix_secs_to_backup_stamp(1_778_596_225), "20260512T143025Z");
    }

    #[test]
    fn utc_now_backup_stamp_is_sortable_and_separator_free() {
        let s = utc_now_backup_stamp();
        assert_eq!(s.len(), 16, "expected YYYYMMDDTHHMMSSZ shape: {s:?}");
        assert!(!s.contains([':', '-']), "stamp kept a separator: {s:?}");
        assert!(s.ends_with('Z'), "stamp is not UTC-marked: {s:?}");
    }

    #[test]
    fn humanize_age_since_buckets_by_magnitude() {
        let now = "2026-05-12T14:30:25Z";
        assert_eq!(
            humanize_age_since("2026-05-12T14:30:23Z", now),
            Some("just now".to_string())
        );
        assert_eq!(
            humanize_age_since("2026-05-12T14:30:00Z", now),
            Some("25s ago".to_string())
        );
        assert_eq!(
            humanize_age_since("2026-05-12T14:25:25Z", now),
            Some("5m ago".to_string())
        );
        assert_eq!(
            humanize_age_since("2026-05-12T11:30:25Z", now),
            Some("3h ago".to_string())
        );
        assert_eq!(
            humanize_age_since("2026-05-10T14:30:25Z", now),
            Some("2d ago".to_string())
        );
    }

    #[test]
    fn humanize_age_since_rejects_unparseable_and_future_timestamps() {
        let now = "2026-05-12T14:30:25Z";
        assert_eq!(humanize_age_since("not-a-timestamp", now), None);
        assert_eq!(humanize_age_since("2026-05-12T14:31:00Z", now), None);
    }

    #[test]
    fn humanize_until_buckets_forward_and_refuses_the_past() {
        let now = "2026-05-12T14:30:25Z";
        assert_eq!(
            humanize_until("2026-05-12T14:30:27Z", now),
            Some("due now".to_string())
        );
        assert_eq!(
            humanize_until("2026-05-12T14:31:00Z", now),
            Some("in 35s".to_string())
        );
        assert_eq!(
            humanize_until("2026-05-12T14:35:25Z", now),
            Some("in 5m".to_string())
        );
        assert_eq!(
            humanize_until("2026-05-12T17:30:25Z", now),
            Some("in 3h".to_string())
        );
        assert_eq!(
            humanize_until("2026-05-14T14:30:25Z", now),
            Some("in 2d".to_string())
        );
        assert_eq!(humanize_until("2026-05-12T14:25:25Z", now), None);
        assert_eq!(humanize_until("not-a-timestamp", now), None);
    }

    /// The two directions share one primitive, so an instant one of them buckets
    /// at `Nh` is the same instant the other buckets at `Nh` — a forward twin
    /// with its own arithmetic drifted at exactly the boundaries nobody tests.
    #[test]
    fn the_two_directions_agree_on_every_bucket_boundary() {
        let now = "2026-05-12T14:30:25Z";
        for (secs, ago, until) in [
            (4_i64, "just now", "due now"),
            (5, "5s ago", "in 5s"),
            (59, "59s ago", "in 59s"),
            (60, "1m ago", "in 1m"),
            (3_599, "59m ago", "in 59m"),
            (3_600, "1h ago", "in 1h"),
            (86_399, "23h ago", "in 23h"),
            (86_400, "1d ago", "in 1d"),
        ] {
            let base = chrono::DateTime::parse_from_rfc3339(now).expect("now parses");
            let past = (base - chrono::Duration::seconds(secs)).to_rfc3339();
            let future = (base + chrono::Duration::seconds(secs)).to_rfc3339();
            assert_eq!(humanize_age_since(&past, now).as_deref(), Some(ago));
            assert_eq!(humanize_until(&future, now).as_deref(), Some(until));
        }
    }

    #[test]
    fn humanize_duration_secs_renders_at_most_two_units() {
        assert_eq!(humanize_duration_secs(0), "0s");
        assert_eq!(humanize_duration_secs(44), "44s");
        assert_eq!(humanize_duration_secs(104), "1m 44s");
        assert_eq!(humanize_duration_secs(120), "2m");
        assert_eq!(humanize_duration_secs(7_380), "2h 3m");
        assert_eq!(humanize_duration_secs(7_200), "2h");
        assert_eq!(humanize_duration_secs(266_400), "3d 2h");
        assert_eq!(humanize_duration_secs(259_200), "3d");
    }

    #[test]
    fn is_stale_since_compares_against_the_threshold() {
        let now = "2026-05-12T14:30:25Z";
        // 5 minutes old, 10 minute threshold: fresh.
        assert!(!is_stale_since("2026-05-12T14:25:25Z", now, 600));
        // 15 minutes old, 10 minute threshold: stale.
        assert!(is_stale_since("2026-05-12T14:15:25Z", now, 600));
        // Unparseable reads as stale rather than silently fresh.
        assert!(is_stale_since("not-a-timestamp", now, 600));
    }

    #[test]
    fn utc_now_filename_safe_has_no_unsafe_chars() {
        let s = utc_now_filename_safe();
        assert!(!s.is_empty());
        assert!(
            !s.contains([':', '-', 'T', 'Z']),
            "filename-safe stamp contained banned char: {s:?}"
        );
    }
}
