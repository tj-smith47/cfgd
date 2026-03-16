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

/// The canonical API version string used in all cfgd YAML documents (local and CRD).
pub const API_VERSION: &str = "cfgd.io/v1alpha1";

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
    let existing: std::collections::HashSet<String> = target.iter().cloned().collect();
    for item in source {
        if !existing.contains(item) {
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

/// Merge env vars by name: later entries override earlier ones with the same name.
/// Used by config layer merging, composition, and reconciler module merge.
pub fn merge_env(base: &mut Vec<config::EnvVar>, updates: &[config::EnvVar]) {
    for ev in updates {
        if let Some(pos) = base.iter().position(|e| e.name == ev.name) {
            base[pos] = ev.clone();
        } else {
            base.push(ev.clone());
        }
    }
}

/// Parse a `KEY=VALUE` string into an `EnvVar`.
pub fn parse_env_var(input: &str) -> std::result::Result<config::EnvVar, String> {
    let (key, value) = input
        .split_once('=')
        .ok_or_else(|| format!("invalid env var '{}' — expected KEY=VALUE", input))?;
    Ok(config::EnvVar {
        name: key.to_string(),
        value: value.to_string(),
    })
}

// ---------------------------------------------------------------------------
// File safety primitives — atomic writes, state capture, path validation
// ---------------------------------------------------------------------------

/// Maximum file size (10 MB) for backup content capture.
/// Files larger than this are tracked but their content is not stored in backups.
const MAX_BACKUP_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Captured state of a file for backup purposes.
#[derive(Debug, Clone)]
pub struct FileState {
    pub content: Vec<u8>,
    pub content_hash: String,
    pub permissions: Option<u32>,
    pub is_symlink: bool,
    pub symlink_target: Option<std::path::PathBuf>,
    /// True if the file exceeded MAX_BACKUP_FILE_SIZE and content was not captured.
    pub oversized: bool,
}

/// Atomically write content to a file using temp-file-then-rename.
///
/// The temp file is created in the same directory as `target` to guarantee a
/// same-filesystem rename (atomic on POSIX). Preserves the permissions of an
/// existing target file if one exists. Creates parent directories as needed.
///
/// Returns the SHA256 hex digest of the written content.
use sha2::Digest as _;

pub fn atomic_write(
    target: &std::path::Path,
    content: &[u8],
) -> std::result::Result<String, std::io::Error> {
    use std::io::Write;

    let parent = target.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;

    // Preserve permissions of existing file if present
    if let Ok(meta) = std::fs::metadata(target) {
        let _ = tmp.as_file().set_permissions(meta.permissions());
    }

    let hash = format!("{:x}", sha2::Sha256::digest(content));

    // persist() does atomic rename on Unix
    tmp.persist(target).map_err(|e| e.error)?;

    Ok(hash)
}

/// Atomically write string content to a file.
pub fn atomic_write_str(
    target: &std::path::Path,
    content: &str,
) -> std::result::Result<String, std::io::Error> {
    atomic_write(target, content.as_bytes())
}

/// Capture a file's content and metadata for backup.
///
/// Uses `symlink_metadata()` — never follows symlinks. For symlinks, captures
/// the link target path but not the content. For regular files >10 MB, sets
/// `oversized: true` and does not capture content.
///
/// Returns `None` if the file does not exist.
pub fn capture_file_state(
    path: &std::path::Path,
) -> std::result::Result<Option<FileState>, std::io::Error> {
    let symlink_meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    if symlink_meta.file_type().is_symlink() {
        let symlink_target = std::fs::read_link(path)?;
        return Ok(Some(FileState {
            content: Vec::new(),
            content_hash: String::new(),
            permissions: None,
            is_symlink: true,
            symlink_target: Some(symlink_target),
            oversized: false,
        }));
    }

    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(symlink_meta.permissions().mode() & 0o777)
    };
    #[cfg(not(unix))]
    let permissions = None;

    if symlink_meta.len() > MAX_BACKUP_FILE_SIZE {
        return Ok(Some(FileState {
            content: Vec::new(),
            content_hash: String::new(),
            permissions,
            is_symlink: false,
            symlink_target: None,
            oversized: true,
        }));
    }

    let content = std::fs::read(path)?;
    let hash = format!("{:x}", sha2::Sha256::digest(&content));

    Ok(Some(FileState {
        content,
        content_hash: hash,
        permissions,
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    }))
}

/// Validate that a resolved path does not escape a root directory.
///
/// Canonicalizes both paths and checks containment. Returns the canonicalized
/// path on success.
pub fn validate_path_within(
    path: &std::path::Path,
    root: &std::path::Path,
) -> std::result::Result<std::path::PathBuf, std::io::Error> {
    let canonical_root = root.canonicalize()?;
    let canonical_path = path.canonicalize()?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "path {} escapes root {}",
                canonical_path.display(),
                canonical_root.display()
            ),
        ));
    }
    Ok(canonical_path)
}

/// Validate that a path contains no `..` components (pre-canonicalization check).
///
/// This catches traversal attempts even when intermediate directories don't
/// exist yet, which `canonicalize()` cannot handle.
pub fn validate_no_traversal(path: &std::path::Path) -> std::result::Result<(), String> {
    for component in path.components() {
        if let std::path::Component::ParentDir = component {
            return Err(format!("path contains '..': {}", path.display()));
        }
    }
    Ok(())
}

/// Escape a value for use in shell `export` statements.
///
/// Uses single quotes for values containing shell metacharacters (`$`, backtick,
/// `\`, `"`). Single quotes within the value are escaped via `'\''`.
pub fn shell_escape_value(value: &str) -> String {
    if value.contains('$')
        || value.contains('`')
        || value.contains('\\')
        || value.contains('"')
        || value.contains('\'')
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        format!("\"{}\"", value)
    }
}

/// Escape a string for safe inclusion in XML/plist content.
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Acquire an exclusive apply lock via `flock()`.
///
/// The lock file is created at `state_dir/apply.lock`. Uses non-blocking
/// `LOCK_EX | LOCK_NB` — returns `StateError::ApplyLockHeld` if another
/// process holds the lock. The lock is released automatically when the guard
/// is dropped.
#[cfg(unix)]
pub fn acquire_apply_lock(
    state_dir: &std::path::Path,
) -> errors::Result<ApplyLockGuard> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    std::fs::create_dir_all(state_dir)?;
    let lock_path = state_dir.join("apply.lock");

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;

    let fd = file.as_raw_fd();
    let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            let holder = std::fs::read_to_string(&lock_path).unwrap_or_default();
            return Err(errors::StateError::ApplyLockHeld {
                holder: format!("pid {}", holder.trim()),
            }
            .into());
        }
        return Err(err.into());
    }

    // Write our PID to the lock file
    let mut f = file;
    f.set_len(0)?;
    write!(f, "{}", std::process::id())?;
    f.sync_all()?;

    Ok(ApplyLockGuard {
        _file: f,
        path: lock_path,
    })
}

/// RAII guard that releases the apply lock when dropped.
#[cfg(unix)]
#[derive(Debug)]
pub struct ApplyLockGuard {
    _file: std::fs::File,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for ApplyLockGuard {
    fn drop(&mut self) {
        // flock is released automatically when the fd is closed.
        // Clear the PID from the lock file so stale reads aren't confusing.
        let _ = std::fs::write(&self.path, "");
    }
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

    #[test]
    fn atomic_write_creates_file_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("test.txt");
        let hash = atomic_write(&target, b"hello world").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello world");
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c/test.txt");
        atomic_write(&target, b"nested").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "nested");
    }

    #[test]
    fn atomic_write_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("perms.txt");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        atomic_write(&target, b"new").unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn atomic_write_str_works() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("str.txt");
        let hash = atomic_write_str(&target, "string content").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "string content");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn capture_file_state_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.txt");
        std::fs::write(&target, "contents").unwrap();

        let state = capture_file_state(&target).unwrap().unwrap();
        assert_eq!(state.content, b"contents");
        assert!(!state.content_hash.is_empty());
        assert!(!state.is_symlink);
        assert!(state.symlink_target.is_none());
        assert!(!state.oversized);
    }

    #[test]
    fn capture_file_state_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&real, "target").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let state = capture_file_state(&link).unwrap().unwrap();
        assert!(state.is_symlink);
        assert_eq!(state.symlink_target.unwrap(), real);
        assert!(state.content.is_empty());
    }

    #[test]
    fn capture_file_state_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does_not_exist.txt");
        let state = capture_file_state(&missing).unwrap();
        assert!(state.is_none());
    }

    #[test]
    fn validate_path_within_accepts_child() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("sub/file.txt");
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(&child, "").unwrap();
        assert!(validate_path_within(&child, dir.path()).is_ok());
    }

    #[test]
    fn validate_path_within_rejects_escape() {
        let dir = tempfile::tempdir().unwrap();
        // /tmp itself exists and is outside our tempdir
        let result = validate_path_within(std::path::Path::new("/tmp"), dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn validate_no_traversal_accepts_clean_path() {
        assert!(validate_no_traversal(std::path::Path::new("a/b/c")).is_ok());
        assert!(validate_no_traversal(std::path::Path::new("/absolute/path")).is_ok());
    }

    #[test]
    fn validate_no_traversal_rejects_dotdot() {
        assert!(validate_no_traversal(std::path::Path::new("a/../b")).is_err());
        assert!(validate_no_traversal(std::path::Path::new("../../etc")).is_err());
    }

    #[test]
    fn shell_escape_value_simple() {
        assert_eq!(shell_escape_value("hello"), "\"hello\"");
    }

    #[test]
    fn shell_escape_value_with_dollar() {
        assert_eq!(shell_escape_value("$HOME/bin"), "'$HOME/bin'");
    }

    #[test]
    fn shell_escape_value_with_single_quote() {
        assert_eq!(shell_escape_value("it's"), "'it'\\''s'");
    }

    #[test]
    fn xml_escape_special_chars() {
        assert_eq!(xml_escape("<tag>&\"'"), "&lt;tag&gt;&amp;&quot;&apos;");
    }

    #[test]
    fn xml_escape_passthrough() {
        assert_eq!(xml_escape("normal text"), "normal text");
    }

    #[test]
    fn acquire_apply_lock_works() {
        let dir = tempfile::tempdir().unwrap();
        let guard = acquire_apply_lock(dir.path()).unwrap();
        // Lock file should contain our PID
        let content = std::fs::read_to_string(dir.path().join("apply.lock")).unwrap();
        assert_eq!(content, format!("{}", std::process::id()));
        drop(guard);
    }

    #[test]
    fn acquire_apply_lock_detects_contention() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = acquire_apply_lock(dir.path()).unwrap();
        // Second acquire should fail with ApplyLockHeld
        let result = acquire_apply_lock(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, errors::CfgdError::State(errors::StateError::ApplyLockHeld { .. })),
            "expected ApplyLockHeld, got: {}",
            err
        );
    }
}
