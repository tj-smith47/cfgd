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

#[cfg(test)]
mod tests {
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
    fn utc_now_filename_safe_has_no_unsafe_chars() {
        let s = utc_now_filename_safe();
        assert!(!s.is_empty());
        assert!(
            !s.contains([':', '-', 'T', 'Z']),
            "filename-safe stamp contained banned char: {s:?}"
        );
    }
}
