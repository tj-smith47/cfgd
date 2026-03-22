pub mod composition;
pub mod config;
pub mod daemon;
pub mod errors;
pub mod generate;
pub mod modules;
pub mod oci;
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
pub const CSI_DRIVER_NAME: &str = "csi.cfgd.io";
pub const MODULES_ANNOTATION: &str = "cfgd.io/modules";

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
    let mut existing: std::collections::HashSet<String> = target.iter().cloned().collect();
    for item in source {
        if existing.insert(item.clone()) {
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

/// Expand `~` and `~/...` paths to the user's home directory.
pub fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    let path_str = path.display().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if path_str == "~" {
            return std::path::PathBuf::from(home);
        }
        if path_str.starts_with("~/") {
            return std::path::PathBuf::from(path_str.replacen('~', &home, 1));
        }
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
            if key_path.exists()
                && let Ok(cred) = git2::Cred::ssh_key(username, None, &key_path, None)
            {
                return Ok(cred);
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

/// Check if a command is available on the system via PATH lookup.
pub fn command_available(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let path = dir.join(cmd);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    path.is_file()
                        && std::fs::metadata(&path)
                            .map(|m| m.permissions().mode() & 0o111 != 0)
                            .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    path.is_file()
                }
            })
        })
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

/// Merge shell aliases by name: later entries override earlier ones with the same name.
/// Same semantics as `merge_env`.
pub fn merge_aliases(base: &mut Vec<config::ShellAlias>, updates: &[config::ShellAlias]) {
    for alias in updates {
        if let Some(pos) = base.iter().position(|a| a.name == alias.name) {
            base[pos] = alias.clone();
        } else {
            base.push(alias.clone());
        }
    }
}

/// Split a list of values into adds and removes.
///
/// Values starting with `-` are treated as removals (the leading `-` is stripped).
/// All other values are adds. This powers the unified `--thing` CLI flags where
/// `--thing foo` adds and `--thing -foo` removes.
pub fn split_add_remove(values: &[String]) -> (Vec<String>, Vec<String>) {
    let mut adds = Vec::new();
    let mut removes = Vec::new();
    for v in values {
        if let Some(stripped) = v.strip_prefix('-') {
            removes.push(stripped.to_string());
        } else {
            adds.push(v.clone());
        }
    }
    (adds, removes)
}

/// Parse a `name=command` string into a `ShellAlias`.
pub fn parse_alias(input: &str) -> std::result::Result<config::ShellAlias, String> {
    let (name, command) = input
        .split_once('=')
        .ok_or_else(|| format!("invalid alias '{}' — expected name=command", input))?;
    Ok(config::ShellAlias {
        name: name.to_string(),
        command: command.to_string(),
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

/// Compute SHA256 hash of data and return as lowercase hex string.
use sha2::Digest as _;

pub fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(data))
}

/// Atomically write content to a file using temp-file-then-rename.
///
/// The temp file is created in the same directory as `target` to guarantee a
/// same-filesystem rename (atomic on POSIX). Preserves the permissions of an
/// existing target file if one exists. Creates parent directories as needed.
///
/// Returns the SHA256 hex digest of the written content.
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

    let hash = sha256_hex(content);

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
    let hash = sha256_hex(&content);

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
/// Sanitize a string for use as a Kubernetes object name (RFC 1123 DNS label).
/// Lowercases, replaces underscores with hyphens, filters non-alphanumeric chars,
/// and trims leading/trailing hyphens.
pub fn sanitize_k8s_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace('_', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

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
pub fn acquire_apply_lock(state_dir: &std::path::Path) -> errors::Result<ApplyLockGuard> {
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

// ---------------------------------------------------------------------------
// Reconcile patch resolution
// ---------------------------------------------------------------------------

/// Fully resolved reconcile settings for a single entity (no Options).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EffectiveReconcile {
    pub interval: String,
    pub auto_apply: bool,
    pub drift_policy: config::DriftPolicy,
}

/// Resolve effective reconcile settings for a module given the profile
/// inheritance chain and any patches in the global reconcile config.
///
/// Precedence (most specific wins):
///   Named Module patch > Kind-wide Module patch > Named Profile patch >
///   Kind-wide Profile patch > Global reconcile settings
///
/// `profile_chain` is ancestors-first, leaf-last (e.g., `["base", "work"]`).
/// Within each level, patches apply in list order (last wins for duplicates).
pub fn resolve_effective_reconcile(
    module_name: &str,
    profile_chain: &[&str],
    reconcile: &config::ReconcileConfig,
) -> EffectiveReconcile {
    let mut effective = EffectiveReconcile {
        interval: reconcile.interval.clone(),
        auto_apply: reconcile.auto_apply,
        drift_policy: reconcile.drift_policy.clone(),
    };

    // 1. Kind-wide Profile patch (no name = applies to all profiles)
    if let Some(patch) = reconcile
        .patches
        .iter()
        .rev()
        .find(|p| p.kind == config::ReconcilePatchKind::Profile && p.name.is_none())
    {
        overlay_reconcile_patch(&mut effective, patch);
    }

    // 2. Named Profile patches in inheritance order (leaf last = leaf wins)
    for profile_name in profile_chain {
        if let Some(patch) = reconcile.patches.iter().rev().find(|p| {
            p.kind == config::ReconcilePatchKind::Profile && p.name.as_deref() == Some(profile_name)
        }) {
            overlay_reconcile_patch(&mut effective, patch);
        }
    }

    // 3. Kind-wide Module patch (no name = applies to all modules)
    if let Some(patch) = reconcile
        .patches
        .iter()
        .rev()
        .find(|p| p.kind == config::ReconcilePatchKind::Module && p.name.is_none())
    {
        overlay_reconcile_patch(&mut effective, patch);
    }

    // 4. Named Module patch (highest priority) — last matching entry wins
    if let Some(patch) = reconcile.patches.iter().rev().find(|p| {
        p.kind == config::ReconcilePatchKind::Module && p.name.as_deref() == Some(module_name)
    }) {
        overlay_reconcile_patch(&mut effective, patch);
    }

    effective
}

/// Overlay a patch's `Some` fields onto an effective reconcile struct.
fn overlay_reconcile_patch(base: &mut EffectiveReconcile, patch: &config::ReconcilePatch) {
    if let Some(ref interval) = patch.interval {
        base.interval = interval.clone();
    }
    if let Some(auto_apply) = patch.auto_apply {
        base.auto_apply = auto_apply;
    }
    if let Some(ref dp) = patch.drift_policy {
        base.drift_policy = dp.clone();
    }
}

// ---------------------------------------------------------------------------
// Duration parsing
// ---------------------------------------------------------------------------

/// Parse a duration string like "30s", "5m", "1h", or a plain number (as seconds).
///
/// Returns an error description on invalid input.
pub fn parse_duration_str(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('s') {
        n.trim()
            .parse::<u64>()
            .map(std::time::Duration::from_secs)
            .map_err(|_| format!("invalid timeout: {}", s))
    } else if let Some(n) = s.strip_suffix('m') {
        n.trim()
            .parse::<u64>()
            .map(|m| std::time::Duration::from_secs(m * 60))
            .map_err(|_| format!("invalid timeout: {}", s))
    } else if let Some(n) = s.strip_suffix('h') {
        n.trim()
            .parse::<u64>()
            .map(|h| std::time::Duration::from_secs(h * 3600))
            .map_err(|_| format!("invalid timeout: {}", s))
    } else {
        s.parse::<u64>()
            .map(std::time::Duration::from_secs)
            .map_err(|_| format!("invalid timeout '{}': use 30s, 5m, or 1h", s))
    }
}

/// Default timeout for profile-level scripts (5 minutes).
pub const PROFILE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_str_seconds() {
        let d = parse_duration_str("30s").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_str_minutes() {
        let d = parse_duration_str("5m").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_str_hours() {
        let d = parse_duration_str("1h").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_str_plain_seconds() {
        let d = parse_duration_str("60").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(60));
    }

    #[test]
    fn parse_duration_str_whitespace() {
        let d = parse_duration_str(" 10 s ").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(10));
    }

    #[test]
    fn parse_duration_str_invalid() {
        assert!(parse_duration_str("abc").is_err());
        assert!(parse_duration_str("").is_err());
        assert!(parse_duration_str("xs").is_err());
    }

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
            matches!(
                err,
                errors::CfgdError::State(errors::StateError::ApplyLockHeld { .. })
            ),
            "expected ApplyLockHeld, got: {}",
            err
        );
    }

    #[test]
    fn merge_aliases_override_by_name() {
        let mut base = vec![
            config::ShellAlias {
                name: "vim".into(),
                command: "vi".into(),
            },
            config::ShellAlias {
                name: "ll".into(),
                command: "ls -l".into(),
            },
        ];
        let updates = vec![config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        }];
        merge_aliases(&mut base, &updates);
        assert_eq!(base.len(), 2);
        assert_eq!(base[0].command, "nvim");
        assert_eq!(base[1].command, "ls -l");
    }

    #[test]
    fn merge_aliases_appends_new() {
        let mut base = vec![config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        }];
        let updates = vec![config::ShellAlias {
            name: "ll".into(),
            command: "ls -la".into(),
        }];
        merge_aliases(&mut base, &updates);
        assert_eq!(base.len(), 2);
    }

    #[test]
    fn split_add_remove_basic() {
        let vals: Vec<String> = vec!["foo".into(), "-bar".into(), "baz".into(), "-qux".into()];
        let (adds, removes) = split_add_remove(&vals);
        assert_eq!(adds, vec!["foo", "baz"]);
        assert_eq!(removes, vec!["bar", "qux"]);
    }

    #[test]
    fn split_add_remove_empty() {
        let (adds, removes) = split_add_remove(&[]);
        assert!(adds.is_empty());
        assert!(removes.is_empty());
    }

    #[test]
    fn split_add_remove_all_adds() {
        let vals: Vec<String> = vec!["a".into(), "b".into()];
        let (adds, removes) = split_add_remove(&vals);
        assert_eq!(adds, vec!["a", "b"]);
        assert!(removes.is_empty());
    }

    #[test]
    fn split_add_remove_all_removes() {
        let vals: Vec<String> = vec!["-x".into(), "-y".into()];
        let (adds, removes) = split_add_remove(&vals);
        assert!(adds.is_empty());
        assert_eq!(removes, vec!["x", "y"]);
    }

    #[test]
    fn parse_alias_valid() {
        let alias = parse_alias("vim=nvim").unwrap();
        assert_eq!(alias.name, "vim");
        assert_eq!(alias.command, "nvim");
    }

    #[test]
    fn parse_alias_with_args() {
        let alias = parse_alias("ll=ls -la --color").unwrap();
        assert_eq!(alias.name, "ll");
        assert_eq!(alias.command, "ls -la --color");
    }

    #[test]
    fn parse_alias_invalid() {
        assert!(parse_alias("no-equals-sign").is_err());
    }

    #[test]
    fn deep_merge_yaml_maps() {
        let mut base = serde_yaml::from_str::<serde_yaml::Value>("a: 1\nb: 2").unwrap();
        let overlay = serde_yaml::from_str::<serde_yaml::Value>("b: 3\nc: 4").unwrap();
        deep_merge_yaml(&mut base, &overlay);
        assert_eq!(base["a"], serde_yaml::Value::from(1));
        assert_eq!(base["b"], serde_yaml::Value::from(3));
        assert_eq!(base["c"], serde_yaml::Value::from(4));
    }

    #[test]
    fn deep_merge_yaml_nested() {
        let mut base = serde_yaml::from_str::<serde_yaml::Value>("top:\n  a: 1\n  b: 2").unwrap();
        let overlay = serde_yaml::from_str::<serde_yaml::Value>("top:\n  b: 9\n  c: 3").unwrap();
        deep_merge_yaml(&mut base, &overlay);
        assert_eq!(base["top"]["a"], serde_yaml::Value::from(1));
        assert_eq!(base["top"]["b"], serde_yaml::Value::from(9));
        assert_eq!(base["top"]["c"], serde_yaml::Value::from(3));
    }

    #[test]
    fn deep_merge_yaml_overlay_replaces_scalar() {
        let mut base = serde_yaml::from_str::<serde_yaml::Value>("x: old").unwrap();
        let overlay = serde_yaml::from_str::<serde_yaml::Value>("x: new").unwrap();
        deep_merge_yaml(&mut base, &overlay);
        assert_eq!(base["x"], serde_yaml::Value::from("new"));
    }

    #[test]
    fn union_extend_deduplicates() {
        let mut target = vec!["a".to_string(), "b".to_string()];
        union_extend(&mut target, &["b".to_string(), "c".to_string()]);
        assert_eq!(target, vec!["a", "b", "c"]);
    }

    #[test]
    fn union_extend_empty_source() {
        let mut target = vec!["a".to_string()];
        union_extend(&mut target, &[]);
        assert_eq!(target, vec!["a"]);
    }

    #[test]
    fn merge_env_overrides_by_name() {
        let mut base = vec![
            config::EnvVar {
                name: "FOO".into(),
                value: "old".into(),
            },
            config::EnvVar {
                name: "BAR".into(),
                value: "keep".into(),
            },
        ];
        let updates = vec![config::EnvVar {
            name: "FOO".into(),
            value: "new".into(),
        }];
        merge_env(&mut base, &updates);
        assert_eq!(base.len(), 2);
        assert_eq!(base.iter().find(|e| e.name == "FOO").unwrap().value, "new");
        assert_eq!(base.iter().find(|e| e.name == "BAR").unwrap().value, "keep");
    }

    #[test]
    fn merge_env_adds_new() {
        let mut base = vec![];
        let updates = vec![config::EnvVar {
            name: "NEW".into(),
            value: "val".into(),
        }];
        merge_env(&mut base, &updates);
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].name, "NEW");
    }

    #[test]
    fn merge_aliases_overrides_by_name() {
        let mut base = vec![config::ShellAlias {
            name: "ll".into(),
            command: "ls -l".into(),
        }];
        let updates = vec![config::ShellAlias {
            name: "ll".into(),
            command: "ls -la".into(),
        }];
        merge_aliases(&mut base, &updates);
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].command, "ls -la");
    }

    #[test]
    fn shell_escape_value_safe_string() {
        assert_eq!(shell_escape_value("hello"), "\"hello\"");
    }

    #[test]
    fn shell_escape_value_metacharacters() {
        let escaped = shell_escape_value("it's a $test");
        // Should single-quote when metacharacters present
        assert!(escaped.starts_with('\''));
    }

    #[test]
    fn xml_escape_all_entities() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }

    #[test]
    fn xml_escape_no_special_chars() {
        assert_eq!(xml_escape("hello world"), "hello world");
    }

    #[test]
    fn unix_secs_to_iso8601_epoch() {
        let result = unix_secs_to_iso8601(0);
        assert_eq!(result, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn unix_secs_to_iso8601_known_date() {
        let result = unix_secs_to_iso8601(1700000000);
        assert!(result.starts_with("2023-11-14"));
    }

    #[test]
    fn copy_dir_recursive_copies_tree() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dst_path = dst.path().join("copy");
        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("a.txt"), "hello").unwrap();
        std::fs::write(src.path().join("sub/b.txt"), "world").unwrap();
        copy_dir_recursive(src.path(), &dst_path).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst_path.join("a.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dst_path.join("sub/b.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn expand_tilde_with_home() {
        let result = expand_tilde(std::path::Path::new("~/test"));
        assert!(!result.to_string_lossy().contains('~'));
        assert!(result.to_string_lossy().ends_with("/test"));
    }

    #[test]
    fn expand_tilde_absolute_unchanged() {
        let result = expand_tilde(std::path::Path::new("/absolute/path"));
        assert_eq!(result, std::path::PathBuf::from("/absolute/path"));
    }

    #[test]
    fn acquire_apply_lock_and_release() {
        let dir = tempfile::tempdir().unwrap();
        let guard = acquire_apply_lock(dir.path()).unwrap();
        assert!(dir.path().join("apply.lock").exists());
        drop(guard);
    }

    // --- Reconcile patch resolution tests ---

    fn test_reconcile_config(patches: Vec<config::ReconcilePatch>) -> config::ReconcileConfig {
        config::ReconcileConfig {
            interval: "5m".into(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches,
        }
    }

    #[test]
    fn resolve_reconcile_global_only() {
        let cfg = test_reconcile_config(vec![]);
        let eff = resolve_effective_reconcile("some-module", &["default"], &cfg);
        assert_eq!(eff.interval, "5m");
        assert!(!eff.auto_apply);
        assert_eq!(eff.drift_policy, config::DriftPolicy::NotifyOnly);
    }

    #[test]
    fn resolve_reconcile_module_patch() {
        let cfg = test_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("certs".into()),
            interval: Some("1m".into()),
            auto_apply: None,
            drift_policy: Some(config::DriftPolicy::Auto),
        }]);
        let eff = resolve_effective_reconcile("certs", &["default"], &cfg);
        assert_eq!(eff.interval, "1m");
        assert!(!eff.auto_apply); // inherited from global
        assert_eq!(eff.drift_policy, config::DriftPolicy::Auto);
    }

    #[test]
    fn resolve_reconcile_profile_patch() {
        let cfg = test_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Profile,
            name: Some("work".into()),
            interval: None,
            auto_apply: Some(true),
            drift_policy: None,
        }]);
        let eff = resolve_effective_reconcile("any-mod", &["base", "work"], &cfg);
        assert_eq!(eff.interval, "5m"); // global
        assert!(eff.auto_apply); // from profile patch
    }

    #[test]
    fn resolve_reconcile_module_beats_profile() {
        let cfg = test_reconcile_config(vec![
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Profile,
                name: Some("work".into()),
                interval: None,
                auto_apply: Some(false),
                drift_policy: None,
            },
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("certs".into()),
                interval: None,
                auto_apply: Some(true),
                drift_policy: None,
            },
        ]);
        let eff = resolve_effective_reconcile("certs", &["work"], &cfg);
        assert!(eff.auto_apply); // module wins over profile
    }

    #[test]
    fn resolve_reconcile_leaf_profile_wins() {
        let cfg = test_reconcile_config(vec![
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Profile,
                name: Some("base".into()),
                interval: Some("10m".into()),
                auto_apply: None,
                drift_policy: None,
            },
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Profile,
                name: Some("work".into()),
                interval: Some("2m".into()),
                auto_apply: None,
                drift_policy: None,
            },
        ]);
        // work is the leaf (last in chain) → wins
        let eff = resolve_effective_reconcile("any", &["base", "work"], &cfg);
        assert_eq!(eff.interval, "2m");
    }

    #[test]
    fn resolve_reconcile_fields_merge_independently() {
        let cfg = test_reconcile_config(vec![
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Profile,
                name: Some("work".into()),
                interval: Some("10m".into()),
                auto_apply: None,
                drift_policy: None,
            },
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("certs".into()),
                interval: None,
                auto_apply: None,
                drift_policy: Some(config::DriftPolicy::Auto),
            },
        ]);
        let eff = resolve_effective_reconcile("certs", &["work"], &cfg);
        assert_eq!(eff.interval, "10m"); // from profile patch
        assert_eq!(eff.drift_policy, config::DriftPolicy::Auto); // from module patch
        assert!(!eff.auto_apply); // from global
    }

    #[test]
    fn resolve_reconcile_missing_module_ignored() {
        let cfg = test_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("nonexistent".into()),
            interval: Some("1s".into()),
            auto_apply: None,
            drift_policy: None,
        }]);
        // Asking for a different module — patch doesn't apply
        let eff = resolve_effective_reconcile("other", &["default"], &cfg);
        assert_eq!(eff.interval, "5m");
    }

    #[test]
    fn resolve_reconcile_duplicate_module_last_wins() {
        let cfg = test_reconcile_config(vec![
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("certs".into()),
                interval: Some("10m".into()),
                auto_apply: None,
                drift_policy: None,
            },
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("certs".into()),
                interval: Some("1m".into()),
                auto_apply: None,
                drift_policy: None,
            },
        ]);
        let eff = resolve_effective_reconcile("certs", &["default"], &cfg);
        assert_eq!(eff.interval, "1m"); // last entry wins
    }

    #[test]
    fn resolve_reconcile_kind_wide_module_patch() {
        let cfg = test_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: None, // applies to all modules
            interval: Some("30s".into()),
            auto_apply: None,
            drift_policy: None,
        }]);
        let eff = resolve_effective_reconcile("any-module", &["default"], &cfg);
        assert_eq!(eff.interval, "30s");
    }

    #[test]
    fn resolve_reconcile_named_beats_kind_wide() {
        let cfg = test_reconcile_config(vec![
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: None, // all modules
                interval: Some("30s".into()),
                auto_apply: None,
                drift_policy: None,
            },
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("certs".into()), // specific
                interval: Some("5s".into()),
                auto_apply: None,
                drift_policy: None,
            },
        ]);
        // Named patch wins over kind-wide
        let eff = resolve_effective_reconcile("certs", &["default"], &cfg);
        assert_eq!(eff.interval, "5s");
        // Other modules get kind-wide
        let eff2 = resolve_effective_reconcile("other", &["default"], &cfg);
        assert_eq!(eff2.interval, "30s");
    }
}
