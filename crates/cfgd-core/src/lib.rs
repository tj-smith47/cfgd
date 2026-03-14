pub mod composition;
pub mod config;
pub mod daemon;
pub mod errors;
pub mod modules;
pub mod output;
pub mod platform;
pub mod providers;
pub mod reconciler;
pub mod server_client;
pub mod sources;
pub mod state;
pub mod upgrade;

// ---------------------------------------------------------------------------
// Shared utilities — used by multiple modules within cfgd-core and downstream
// ---------------------------------------------------------------------------

/// Returns the current UTC time as an ISO 8601 / RFC 3339 string.
pub fn utc_now_iso8601() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
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

/// Deep merge two YAML values. Mappings are merged recursively; all other
/// types are replaced by the overlay value.
pub fn deep_merge_yaml(base: &mut serde_yaml::Value, overlay: &serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                if let Some(base_value) = base_map.get_mut(key) {
                    deep_merge_yaml(base_value, value);
                } else {
                    base_map.insert(key.clone(), value.clone());
                }
            }
        }
        (base, overlay) => {
            *base = overlay.clone();
        }
    }
}

/// Extend a `Vec<String>` with items from `source`, skipping duplicates.
pub fn union_extend(target: &mut Vec<String>, source: &[String]) {
    for item in source {
        if !target.contains(item) {
            target.push(item.clone());
        }
    }
}

/// Default config directory: `~/.config/cfgd/` (XDG_CONFIG_HOME/cfgd on Linux).
pub fn default_config_dir() -> std::path::PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.config_dir().join("cfgd"))
        .unwrap_or_else(|| expand_tilde(std::path::Path::new("~/.config/cfgd")))
}

/// Expand `~/...` paths to the user's home directory.
pub fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    let path_str = path.display().to_string();
    if path_str.starts_with("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return std::path::PathBuf::from(path_str.replacen('~', &home, 1));
    }
    path.to_path_buf()
}

/// Parse a potentially loose version string into a semver Version.
/// Handles "1.28" → "1.28.0" and "1" → "1.0.0".
pub fn parse_loose_version(s: &str) -> Option<semver::Version> {
    if let Ok(ver) = semver::Version::parse(s) {
        return Some(ver);
    }
    if s.matches('.').count() == 1
        && let Ok(ver) = semver::Version::parse(&format!("{s}.0"))
    {
        return Some(ver);
    }
    if !s.contains('.')
        && let Ok(ver) = semver::Version::parse(&format!("{s}.0.0"))
    {
        return Some(ver);
    }
    None
}

/// Check whether `version_str` satisfies `requirement_str` (semver range).
pub fn version_satisfies(version_str: &str, requirement_str: &str) -> bool {
    let req = match semver::VersionReq::parse(requirement_str) {
        Ok(r) => r,
        Err(_) => return false,
    };
    parse_loose_version(version_str)
        .map(|ver| req.matches(&ver))
        .unwrap_or(false)
}

/// Git credential callback for git2 — handles SSH and HTTPS authentication.
/// Used by sources/, modules/, and daemon/ for all git operations.
///
/// Tries in order:
/// 1. SSH agent (for SSH URLs)
/// 2. SSH key files: `~/.ssh/id_ed25519`, `~/.ssh/id_rsa` (for SSH URLs)
/// 3. Git credential helper / GIT_ASKPASS (for HTTPS URLs)
/// 4. Default system credentials
pub fn git_ssh_credentials(
    _url: &str,
    username_from_url: Option<&str>,
    allowed_types: git2::CredentialType,
) -> std::result::Result<git2::Cred, git2::Error> {
    let username = username_from_url.unwrap_or("git");

    if allowed_types.contains(git2::CredentialType::SSH_KEY) {
        if let Ok(cred) = git2::Cred::ssh_key_from_agent(username) {
            return Ok(cred);
        }
        let home = std::env::var("HOME").unwrap_or_default();
        for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
            let key_path = std::path::Path::new(&home).join(".ssh").join(key_name);
            if key_path.exists() {
                return git2::Cred::ssh_key(username, None, &key_path, None);
            }
        }
    }

    if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        return git2::Cred::credential_helper(
            &git2::Config::open_default()
                .map_err(|e| git2::Error::from_str(&format!("cannot open git config: {e}")))?,
            _url,
            username_from_url,
        );
    }

    if allowed_types.contains(git2::CredentialType::DEFAULT) {
        return git2::Cred::default();
    }

    Err(git2::Error::from_str("no suitable credentials found"))
}

/// Recursively copy a directory from source to target.
pub fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

/// Check if a command is available on the system by running `<cmd> --version`.
pub fn command_available(cmd: &str) -> bool {
    std::process::Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_loose_version_full_semver() {
        assert!(parse_loose_version("1.28.3").is_some());
        assert!(parse_loose_version("0.1.0").is_some());
    }

    #[test]
    fn parse_loose_version_two_part() {
        let ver = parse_loose_version("1.28").unwrap();
        assert_eq!(ver, semver::Version::new(1, 28, 0));
    }

    #[test]
    fn parse_loose_version_single_part() {
        let ver = parse_loose_version("1").unwrap();
        assert_eq!(ver, semver::Version::new(1, 0, 0));
    }

    #[test]
    fn parse_loose_version_rejects_garbage() {
        assert!(parse_loose_version("abc").is_none());
        assert!(parse_loose_version("").is_none());
        assert!(parse_loose_version("1.2.3.4").is_none());
    }

    #[test]
    fn version_satisfies_basic() {
        assert!(version_satisfies("1.28.3", ">=1.28"));
        assert!(!version_satisfies("1.27.0", ">=1.28"));
        assert!(version_satisfies("2.40.1", "~2.40"));
        assert!(!version_satisfies("2.39.0", "~2.40"));
    }

    #[test]
    fn version_satisfies_loose() {
        assert!(version_satisfies("1.28", ">=1.28"));
        assert!(version_satisfies("2", ">=1.28"));
        assert!(!version_satisfies("1", ">=1.28"));
    }

    #[test]
    fn version_satisfies_invalid_requirement() {
        assert!(!version_satisfies("1.0.0", "not valid"));
    }

    #[test]
    fn version_satisfies_invalid_version() {
        assert!(!version_satisfies("abc", ">=1.0"));
    }
}
