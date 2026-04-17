pub mod compliance;
pub mod composition;
pub mod config;
pub mod daemon;
pub mod errors;
pub mod generate;
pub mod http;
pub mod modules;
pub mod oci;
pub mod output;
pub mod platform;
pub mod providers;
pub mod reconciler;
pub mod retry;
pub mod server_client;
pub mod sources;
pub mod state;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
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
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    unix_secs_to_iso8601(secs)
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

/// Prepare a `git` CLI command with SSH hang protection.
///
/// Sets `GIT_TERMINAL_PROMPT=0` to prevent interactive prompts and, for SSH URLs,
/// sets `GIT_SSH_COMMAND` with `BatchMode=yes` and configurable `StrictHostKeyChecking`
/// to prevent hangs in non-interactive contexts (piped install scripts, daemons).
///
/// The `ssh_policy` parameter controls the `StrictHostKeyChecking` value:
/// - `None` uses the default (`accept-new`)
/// - `Some(policy)` uses the specified policy
pub fn git_cmd_safe(
    url: Option<&str>,
    ssh_policy: Option<config::SshHostKeyPolicy>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if url.is_some_and(|u| u.starts_with("git@") || u.starts_with("ssh://")) {
        let policy = ssh_policy.unwrap_or_default();
        cmd.env(
            "GIT_SSH_COMMAND",
            format!(
                "ssh -o BatchMode=yes -o StrictHostKeyChecking={}",
                policy.as_ssh_option()
            ),
        );
    }
    cmd
}

/// Try a git CLI command via [`git_cmd_safe`], returning `true` on success.
/// On failure, logs the stderr via `tracing::debug` and returns `false`.
pub fn try_git_cmd(
    url: Option<&str>,
    args: &[&str],
    label: &str,
    ssh_policy: Option<config::SshHostKeyPolicy>,
) -> bool {
    let mut cmd = git_cmd_safe(url, ssh_policy);
    cmd.args(args);
    match command_output_with_timeout(&mut cmd, GIT_NETWORK_TIMEOUT) {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            tracing::debug!(
                "git {} CLI failed (exit {}): {}",
                label,
                output.status.code().unwrap_or(-1),
                stderr_lossy_trimmed(&output),
            );
            false
        }
        Err(e) => {
            tracing::debug!("git {} CLI unavailable: {e}", label);
            false
        }
    }
}

/// Default timeout for external commands (2 minutes).
pub const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Default timeout for git network operations (5 minutes).
pub const GIT_NETWORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Run a [`Command`] with a timeout, killing the process if it exceeds the limit.
/// Returns `Err` if spawn fails or the process is killed due to timeout.
pub fn command_output_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::sync::mpsc;

    let child = cmd.spawn()?;
    let id = child.id();
    let (tx, rx) = mpsc::channel();

    // Spawn a watchdog thread that kills the child after timeout
    std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            // Timeout expired — kill the process
            terminate_process(id);
        }
    });

    let result = child.wait_with_output();
    // Signal the watchdog to stop (if the process finished before timeout)
    let _ = tx.send(());
    result
}

thread_local! {
    /// Thread-local override for the resolved home directory.
    ///
    /// Tests that exercise code paths resolving `~` or `$HOME` must set this
    /// to a tempdir to prevent real-filesystem mutations (writes to
    /// `~/.cfgd.env`, injection into `~/.bashrc`, etc.). Production code
    /// never reads or writes this cell — it only affects `home_dir_var` and
    /// `default_config_dir` when a test scoped an override.
    ///
    /// Use `with_test_home(path, || ...)` to scope an override; the value is
    /// restored on return even if the closure panics (RAII via the guard).
    static TEST_HOME_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// RAII guard returned by [`with_test_home_guard`] — restores the prior
/// override on drop. Used by test harnesses (like `TestEnvBuilder`) that want
/// to install an override without wrapping the whole test in a closure.
#[must_use = "dropping the guard immediately restores the previous override"]
pub struct TestHomeGuard {
    prev: Option<std::path::PathBuf>,
}

impl Drop for TestHomeGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        TEST_HOME_OVERRIDE.with(|o| *o.borrow_mut() = prev);
    }
}

/// Install a HOME override for the current thread and return a guard that
/// restores the prior value on drop. Use in test builders that need the
/// override to outlive a single closure call.
pub fn with_test_home_guard(home: &std::path::Path) -> TestHomeGuard {
    let prev = TEST_HOME_OVERRIDE.with(|o| o.replace(Some(home.to_path_buf())));
    TestHomeGuard { prev }
}

/// Scope a HOME override for the duration of `f`. The prior value (including
/// `None`) is restored when `f` returns, whether normally or via panic.
pub fn with_test_home<F, R>(home: &std::path::Path, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = with_test_home_guard(home);
    f()
}

/// Read the current test HOME override (if any). Only used internally by
/// `home_dir_var` / `default_config_dir`.
fn test_home_override() -> Option<std::path::PathBuf> {
    TEST_HOME_OVERRIDE.with(|o| o.borrow().clone())
}

/// Default config directory: `~/.config/cfgd` on Unix (respects XDG_CONFIG_HOME),
/// `AppData\Roaming\cfgd` on Windows.
pub fn default_config_dir() -> std::path::PathBuf {
    // Thread-local test override always wins. Lets tests redirect config
    // lookup to a tempdir without mutating global env state.
    if let Some(home) = test_home_override() {
        return home.join(".config").join("cfgd");
    }
    #[cfg(unix)]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return std::path::PathBuf::from(xdg).join("cfgd");
        }
        expand_tilde(std::path::Path::new("~/.config/cfgd"))
    }
    #[cfg(windows)]
    {
        directories::BaseDirs::new()
            .map(|b| b.config_dir().join("cfgd"))
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData\cfgd"))
    }
}

/// Expand `~` and `~/...` paths to the user's home directory.
pub fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    let path_str = path.display().to_string();
    let home = home_dir_var();
    if let Some(home) = home {
        if path_str == "~" {
            return std::path::PathBuf::from(home);
        }
        if path_str.starts_with("~/") || path_str.starts_with("~\\") {
            return std::path::PathBuf::from(path_str.replacen('~', &home, 1));
        }
    }
    path.to_path_buf()
}

/// Resolve the user's home directory, consulting the test override first.
/// Unix production path: checks HOME.
/// Windows production path: checks USERPROFILE first, then HOME (for WSL/Git Bash contexts).
fn home_dir_var() -> Option<String> {
    if let Some(home) = test_home_override() {
        return Some(home.to_string_lossy().into_owned());
    }
    #[cfg(unix)]
    {
        std::env::var("HOME").ok()
    }
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .ok()
    }
}

/// Get the system hostname as a String. Returns "unknown" on failure.
pub fn hostname_string() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Resolve a relative path against a base directory with traversal validation.
/// Absolute paths are returned as-is. Relative paths are joined to `base` and
/// validated with `validate_no_traversal`. Returns `Err` if the relative path
/// contains `..` components.
pub fn resolve_relative_path(
    path: &std::path::Path,
    base: &std::path::Path,
) -> std::result::Result<std::path::PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let joined = base.join(path);
        validate_no_traversal(&joined)?;
        Ok(joined)
    }
}

/// Create a symbolic link. On Unix, uses `std::os::unix::fs::symlink`.
/// On Windows, uses `symlink_file` or `symlink_dir` based on the source type.
/// If symlink creation fails on Windows due to insufficient privileges,
/// returns an error with guidance to enable Developer Mode or run as admin.
pub fn create_symlink(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        create_symlink_impl(source, target)
    }
    #[cfg(windows)]
    {
        create_symlink_impl(source, target).map_err(|e| {
            if e.raw_os_error() == Some(1314) {
                // ERROR_PRIVILEGE_NOT_HELD
                return std::io::Error::new(
                    e.kind(),
                    format!(
                        "symlink creation requires Developer Mode or admin privileges: {} -> {}\n\
                         Enable Developer Mode: Settings > Update & Security > For developers",
                        source.display(),
                        target.display()
                    ),
                );
            }
            e
        })
    }
}

#[cfg(unix)]
fn create_symlink_impl(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn create_symlink_impl(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, target)
    } else {
        std::os::windows::fs::symlink_file(source, target)
    }
}

/// Get Unix permission mode bits from file metadata. Returns None on Windows.
#[cfg(unix)]
pub fn file_permissions_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(windows)]
pub fn file_permissions_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Set Unix permission mode bits on a file. No-op on Windows (NTFS uses inherited ACLs).
#[cfg(unix)]
pub fn set_file_permissions(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub fn set_file_permissions(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    tracing::debug!("set_file_permissions is a no-op on Windows (NTFS uses inherited ACLs)");
    Ok(())
}

/// Check if a file is executable.
/// Unix: checks the executable bit in mode.
/// Windows: checks file extension against known executable types.
#[cfg(unix)]
pub fn is_executable(_path: &std::path::Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
pub fn is_executable(path: &std::path::Path, _metadata: &std::fs::Metadata) -> bool {
    const EXECUTABLE_EXTENSIONS: &[&str] = &["exe", "cmd", "bat", "ps1", "com"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXECUTABLE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Check if two paths refer to the same file (same inode on Unix, same file index on Windows).
#[cfg(unix)]
pub fn is_same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.ino() == mb.ino() && ma.dev() == mb.dev(),
        _ => false,
    }
}

#[cfg(windows)]
pub fn is_same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
    use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;

    fn file_info(path: &std::path::Path) -> Option<BY_HANDLE_FILE_INFORMATION> {
        let file = std::fs::File::open(path).ok()?;
        // SAFETY: `BY_HANDLE_FILE_INFORMATION` is a plain-old-data struct of
        // integer fields; the all-zero bit pattern is a valid initial value
        // that `GetFileInformationByHandle` will overwrite before we read it.
        let mut info = unsafe { std::mem::zeroed() };
        // SAFETY: `file.as_raw_handle()` returns a valid, open Win32 file
        // handle owned by `file`, which outlives the call. `&mut info`
        // points to sufficient, aligned, writable memory for the out
        // parameter. No aliasing: `info` is stack-local.
        let ret = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
        if ret != 0 { Some(info) } else { None }
    }

    match (file_info(a), file_info(b)) {
        (Some(ia), Some(ib)) => {
            ia.dwVolumeSerialNumber == ib.dwVolumeSerialNumber
                && ia.nFileIndexHigh == ib.nFileIndexHigh
                && ia.nFileIndexLow == ib.nFileIndexLow
        }
        _ => false,
    }
}

/// Send a termination signal to a process by PID.
/// Unix: sends SIGTERM. Windows: calls TerminateProcess.
#[cfg(unix)]
pub fn terminate_process(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

#[cfg(windows)]
pub fn terminate_process(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: `OpenProcess` is always sound to call with valid flags; it
    // returns NULL on failure (checked below) or a valid handle we own. We
    // call `TerminateProcess` and `CloseHandle` only with that owned
    // handle, and `CloseHandle` runs exactly once per successful open, so
    // there is no double-close or use-after-close.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// Check if the current process is running with elevated privileges.
/// Unix: checks euid == 0. Windows: checks IsUserAnAdmin().
#[cfg(unix)]
pub fn is_root() -> bool {
    use nix::unistd::geteuid;
    geteuid().is_root()
}

#[cfg(windows)]
pub fn is_root() -> bool {
    use windows_sys::Win32::UI::Shell::IsUserAnAdmin;
    // SAFETY: `IsUserAnAdmin` takes no parameters, has no preconditions,
    // and returns a BOOL. It is safe to call from any thread at any time.
    unsafe { IsUserAnAdmin() != 0 }
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
        let home = home_dir_var().unwrap_or_default();
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
/// Skips symlinks to prevent symlink-following attacks and infinite loops.
pub fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        // Skip symlinks — prevents following links outside the source tree
        if file_type.is_symlink() {
            continue;
        }
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

/// Check if a command is available on the system via PATH lookup.
/// On Windows, tries common executable extensions (.exe, .cmd, .bat, .ps1, .com)
/// since executables require an extension to be found.
pub fn command_available(cmd: &str) -> bool {
    let extensions: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat", ".ps1", ".com"]
    } else {
        &[""]
    };
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                extensions.iter().any(|ext| {
                    let name = format!("{}{}", cmd, ext);
                    let path = dir.join(&name);
                    path.is_file()
                        && std::fs::metadata(&path)
                            .map(|m| is_executable(&path, &m))
                            .unwrap_or(false)
                })
            })
        })
        .unwrap_or(false)
}

/// Build a `tracing_subscriber::EnvFilter` from `RUST_LOG` if set, falling
/// back to `default`. Consolidates the four identical
/// `EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(..))`
/// scaffolds in `cfgd/main.rs`, `cfgd/cli/plugin.rs`, `cfgd-operator/main.rs`,
/// and `cfgd-csi/main.rs`.
pub fn tracing_env_filter(default: &str) -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default))
}

/// Check that a CLI tool is available on PATH, returning a unified error
/// string otherwise. Before this helper, six `if !command_available("X")`
/// gates across `oci.rs` and `cli/module.rs` each produced a slightly
/// different "not found" message; strings had diverged in production. Pass
/// `install_hint` (a short imperative like "install it from https://...")
/// to make the hint specific; `None` falls back to a generic "install it
/// or add it to PATH".
pub fn require_tool(name: &str, install_hint: Option<&str>) -> std::result::Result<(), String> {
    if command_available(name) {
        return Ok(());
    }
    Err(match install_hint {
        Some(hint) => format!("{name} not found — {hint}"),
        None => format!("{name} not found — install it or add it to PATH"),
    })
}

/// Merge env vars by name: later entries override earlier ones with the same name.
/// Used by config layer merging, composition, and reconciler module merge.
pub fn merge_env(base: &mut Vec<config::EnvVar>, updates: &[config::EnvVar]) {
    let mut index: std::collections::HashMap<String, usize> = base
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name.clone(), i))
        .collect();
    for ev in updates {
        if let Some(&pos) = index.get(&ev.name) {
            base[pos] = ev.clone();
        } else {
            index.insert(ev.name.clone(), base.len());
            base.push(ev.clone());
        }
    }
}

/// Parse a `KEY=VALUE` string into an `EnvVar`.
pub fn parse_env_var(input: &str) -> std::result::Result<config::EnvVar, String> {
    let (key, value) = input
        .split_once('=')
        .ok_or_else(|| format!("invalid env var '{}' — expected KEY=VALUE", input))?;
    validate_env_var_name(key)?;
    Ok(config::EnvVar {
        name: key.to_string(),
        value: value.to_string(),
    })
}

/// Validate that an environment variable name is safe for shell interpolation.
/// Accepts names matching `[A-Za-z_][A-Za-z0-9_]*`.
pub fn validate_env_var_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("environment variable name must not be empty".to_string());
    }
    let first = name.as_bytes()[0];
    if !first.is_ascii_alphabetic() && first != b'_' {
        return Err(format!(
            "invalid env var name '{}' — must start with a letter or underscore",
            name
        ));
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(format!(
            "invalid env var name '{}' — must contain only letters, digits, and underscores",
            name
        ));
    }
    Ok(())
}

/// Validate that a shell alias name is safe for shell interpolation.
/// Accepts names matching `[A-Za-z0-9_.-]+`.
pub fn validate_alias_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("alias name must not be empty".to_string());
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(format!(
            "invalid alias name '{}' — must contain only letters, digits, underscores, hyphens, and dots",
            name
        ));
    }
    Ok(())
}

/// Merge shell aliases by name: later entries override earlier ones with the same name.
/// Same semantics as `merge_env`.
pub fn merge_aliases(base: &mut Vec<config::ShellAlias>, updates: &[config::ShellAlias]) {
    let mut index: std::collections::HashMap<String, usize> = base
        .iter()
        .enumerate()
        .map(|(i, a)| (a.name.clone(), i))
        .collect();
    for alias in updates {
        if let Some(&pos) = index.get(&alias.name) {
            base[pos] = alias.clone();
        } else {
            index.insert(alias.name.clone(), base.len());
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
    validate_alias_name(name)?;
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

/// Compute an OCI-style `sha256:<hex>` digest string from data.
pub fn sha256_digest(data: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(data))
}

/// Strip the `sha256:` prefix from a digest string, returning the hex body.
/// Returns the original string unchanged if no prefix is present.
pub fn strip_sha256_prefix(s: &str) -> &str {
    s.strip_prefix("sha256:").unwrap_or(s)
}

/// Named exponential-histogram bucket presets for latency metrics. Kept in
/// cfgd-core so the SLO-adjacent choice is auditable in one place rather
/// than divergent inline calls in cfgd-operator and cfgd-csi. Consumers
/// feed the triple into `prometheus_client::metrics::histogram::exponential_buckets(start, factor, length)`.
pub const DURATION_BUCKETS_SHORT: (f64, f64, u16) = (0.001, 2.0, 16);
pub const DURATION_BUCKETS_LONG: (f64, f64, u16) = (0.1, 2.0, 10);

/// Extract stdout from a `Command` output as a trimmed, lossy UTF-8 string.
pub fn stdout_lossy_trimmed(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Extract stderr from a `Command` output as a trimmed, lossy UTF-8 string.
pub fn stderr_lossy_trimmed(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
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

    let permissions = file_permissions_mode(&symlink_meta);

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

/// Like `capture_file_state`, but follows symlinks to capture the resolved
/// content. For symlinks, `is_symlink` and `symlink_target` are recorded AND
/// the actual file content behind the symlink is read. This is used for
/// post-apply snapshots where we need to know both the link target and the
/// content that was accessible through the symlink at the time of capture.
///
/// Returns `None` if the file does not exist (or the symlink is dangling).
pub fn capture_file_resolved_state(
    path: &std::path::Path,
) -> std::result::Result<Option<FileState>, std::io::Error> {
    let symlink_meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    let is_symlink = symlink_meta.file_type().is_symlink();
    let symlink_target = if is_symlink {
        std::fs::read_link(path).ok()
    } else {
        None
    };

    // Read the actual content (following symlinks)
    let real_meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Dangling symlink
            return Ok(None);
        }
        Err(e) => return Err(e),
    };

    let permissions = file_permissions_mode(&real_meta);

    if real_meta.len() > MAX_BACKUP_FILE_SIZE {
        return Ok(Some(FileState {
            content: Vec::new(),
            content_hash: String::new(),
            permissions,
            is_symlink,
            symlink_target,
            oversized: true,
        }));
    }

    let content = std::fs::read(path)?;
    let hash = sha256_hex(&content);

    Ok(Some(FileState {
        content,
        content_hash: hash,
        permissions,
        is_symlink,
        symlink_target,
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
/// Single-pass scan: returns double-quoted string when no metacharacters are present
/// (zero intermediate allocations in the common case).
pub fn shell_escape_value(value: &str) -> String {
    if !value
        .bytes()
        .any(|b| matches!(b, b'$' | b'`' | b'\\' | b'"' | b'\''))
    {
        return format!("\"{}\"", value);
    }
    // Single-quote strategy: only `'` needs escaping inside single quotes
    if !value.contains('\'') {
        return format!("'{}'", value);
    }
    // Value contains both metacharacters and single quotes — break-out escaping
    let mut out = String::with_capacity(value.len() + 8);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Escape a value for use inside bash/zsh double quotes (single pass).
/// Escapes `\`, `"`, `` ` ``, and `!` — the four characters with special
/// meaning inside double-quoted strings.
pub fn escape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '\\' | '"' | '`' | '!' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Escape a string for safe inclusion in XML/plist content (single pass).
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Acquire an exclusive apply lock via `flock()`.
///
/// The lock file is created at `state_dir/apply.lock`. Uses non-blocking
/// `LOCK_EX | LOCK_NB` — returns `StateError::ApplyLockHeld` if another
/// process holds the lock. The lock is released automatically when the guard
/// is dropped.
/// Platform-specific lock file type.
/// Unix: `nix::fcntl::Flock` (safe RAII flock, unlocks on drop).
/// Windows: plain `File` (LockFileEx releases on handle close).
#[cfg(unix)]
type LockFile = nix::fcntl::Flock<std::fs::File>;
#[cfg(windows)]
type LockFile = std::fs::File;

/// RAII guard that releases the apply lock when dropped.
#[derive(Debug)]
pub struct ApplyLockGuard {
    _file: LockFile,
    _path: std::path::PathBuf,
}

impl Drop for ApplyLockGuard {
    fn drop(&mut self) {
        // Clear the PID so stale reads aren't confusing.
        // Lock is released when LockFile is dropped.
        if let Err(e) = self._file.set_len(0) {
            tracing::debug!(path = ?self._path, error = %e, "failed to clear apply-lock PID on drop");
        }
    }
}

#[cfg(unix)]
pub fn acquire_apply_lock(state_dir: &std::path::Path) -> errors::Result<ApplyLockGuard> {
    use std::io::Write;

    std::fs::create_dir_all(state_dir)?;
    let lock_path = state_dir.join("apply.lock");

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;

    let mut locked = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_file, errno)| {
            if errno == nix::errno::Errno::EWOULDBLOCK {
                let holder = std::fs::read_to_string(&lock_path).unwrap_or_default();
                errors::CfgdError::from(errors::StateError::ApplyLockHeld {
                    holder: format!("pid {}", holder.trim()),
                })
            } else {
                errors::CfgdError::from(std::io::Error::from(errno))
            }
        })?;

    // Write our PID to the lock file
    locked.set_len(0)?;
    write!(locked, "{}", std::process::id())?;
    locked.sync_all()?;

    Ok(ApplyLockGuard {
        _file: locked,
        _path: lock_path,
    })
}

/// Acquire an exclusive apply lock via `LockFileEx`.
///
/// The lock file is created at `state_dir/apply.lock`. Uses
/// `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY` — returns
/// `StateError::ApplyLockHeld` if another process holds the lock. The lock is
/// released automatically when the guard is dropped (file handle closed).
#[cfg(windows)]
pub fn acquire_apply_lock(state_dir: &std::path::Path) -> errors::Result<ApplyLockGuard> {
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };

    std::fs::create_dir_all(state_dir)?;
    let lock_path = state_dir.join("apply.lock");

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: `OVERLAPPED` is a plain-old-data struct of integers and a
    // handle field; the all-zero bit pattern is the documented "no event,
    // offset 0" initial value for synchronous-style LockFileEx calls.
    let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = unsafe { std::mem::zeroed() };
    // SAFETY: `handle` is a valid, open, owned Win32 file handle derived
    // from `file`, which outlives the call. `&mut overlapped` points to a
    // stack-local, aligned, writable OVERLAPPED struct. The lock byte
    // range (offset 0, length 1) is fixed and valid. Non-blocking lock
    // (LOCKFILE_FAIL_IMMEDIATELY) avoids indefinite wait.
    let ret = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ret == 0 {
        let err = std::io::Error::last_os_error();
        // ERROR_LOCK_VIOLATION (33) = lock held by another process
        if err.raw_os_error() == Some(33) {
            let holder = std::fs::read_to_string(&lock_path).unwrap_or_default();
            return Err(errors::StateError::ApplyLockHeld {
                holder: format!("pid {}", holder.trim()),
            }
            .into());
        }
        return Err(err.into());
    }

    let mut f = file;
    f.set_len(0)?;
    write!(f, "{}", std::process::id())?;
    f.sync_all()?;

    Ok(ApplyLockGuard {
        _file: f,
        _path: lock_path,
    })
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

/// Default timeout for profile-level scripts (5 minutes).
pub const PROFILE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Check if a file is encrypted with the given backend.
///
/// - `sops`: parses YAML/JSON and checks for a top-level `sops` key with `mac` and `lastmodified`.
/// - `age`: checks if the file starts with the `age-encryption.org` header (reads as bytes to handle binary).
/// - Unknown backend: returns `FileError::UnknownEncryptionBackend`.
pub fn is_file_encrypted(
    path: &std::path::Path,
    backend: &str,
) -> std::result::Result<bool, errors::FileError> {
    use errors::FileError;
    match backend {
        "sops" => {
            let content = std::fs::read_to_string(path).map_err(|e| FileError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            // Try YAML first.  SOPS injects a top-level `sops` map with `mac` + `lastmodified`.
            let value: Option<serde_yaml::Value> = serde_yaml::from_str(&content).ok();
            if let Some(serde_yaml::Value::Mapping(map)) = value
                && let Some(serde_yaml::Value::Mapping(sops)) =
                    map.get(serde_yaml::Value::String("sops".to_string()))
                && sops.contains_key(serde_yaml::Value::String("mac".to_string()))
                && sops.contains_key(serde_yaml::Value::String("lastmodified".to_string()))
            {
                return Ok(true);
            }
            // Try JSON (SOPS can encrypt JSON files too).
            let json_value: Option<serde_json::Value> = serde_json::from_str(&content).ok();
            if let Some(serde_json::Value::Object(map)) = json_value
                && let Some(serde_json::Value::Object(sops)) = map.get("sops")
                && sops.contains_key("mac")
                && sops.contains_key("lastmodified")
            {
                return Ok(true);
            }
            Ok(false)
        }
        "age" => {
            let content = std::fs::read(path).map_err(|e| FileError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Ok(content.starts_with(b"age-encryption.org"))
        }
        other => Err(FileError::UnknownEncryptionBackend {
            backend: other.to_string(),
        }),
    }
}

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
    fn parse_duration_str_days() {
        let d = parse_duration_str("30d").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(30 * 86400));
    }

    #[test]
    fn parse_duration_str_invalid() {
        assert!(
            parse_duration_str("abc")
                .unwrap_err()
                .contains("invalid timeout"),
            "bare letters should fail with a useful message"
        );
        assert!(
            parse_duration_str("")
                .unwrap_err()
                .contains("invalid timeout"),
            "empty string should fail"
        );
        assert!(
            parse_duration_str("xs")
                .unwrap_err()
                .contains("invalid timeout"),
            "non-numeric prefix should fail"
        );
    }

    #[test]
    fn parse_duration_str_zero() {
        assert_eq!(
            parse_duration_str("0s").unwrap(),
            std::time::Duration::from_secs(0)
        );
        assert_eq!(
            parse_duration_str("0").unwrap(),
            std::time::Duration::from_secs(0)
        );
    }

    #[test]
    fn parse_duration_str_negative() {
        assert!(
            parse_duration_str("-5s").is_err(),
            "negative durations should be rejected"
        );
    }

    #[test]
    fn parse_loose_version_full_semver() {
        assert_eq!(
            parse_loose_version("1.28.3"),
            Some(semver::Version::new(1, 28, 3))
        );
        assert_eq!(
            parse_loose_version("0.1.0"),
            Some(semver::Version::new(0, 1, 0))
        );
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
        assert!(
            parse_loose_version("-1").is_none(),
            "negative numbers are not valid versions"
        );
    }

    #[test]
    fn parse_loose_version_preserves_prerelease() {
        // semver::Version::parse handles pre-release tags
        let ver = parse_loose_version("1.2.3-beta.1").unwrap();
        assert_eq!(ver.major, 1);
        assert_eq!(ver.minor, 2);
        assert_eq!(ver.patch, 3);
        assert!(!ver.pre.is_empty(), "pre-release should be preserved");
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

    #[cfg(unix)]
    #[test]
    fn home_dir_var_uses_home_on_unix() {
        let result = home_dir_var();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), std::env::var("HOME").unwrap());
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

    #[cfg(unix)]
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
    #[cfg(unix)]
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
    fn create_symlink_creates_link() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        std::fs::write(&source, "hello").unwrap();
        let link = dir.path().join("link.txt");
        create_symlink(&source, &link).unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "hello");
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_mode_returns_mode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "test").unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        let mode = file_permissions_mode(&meta);
        assert!(mode.is_some());
        let bits = mode.unwrap();
        assert!(bits & 0o777 > 0, "mode bits should be non-zero");
        assert!(
            bits & 0o400 != 0,
            "owner read bit should be set on a newly created file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn set_file_permissions_changes_mode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "test").unwrap();
        set_file_permissions(&file, 0o755).unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        assert_eq!(file_permissions_mode(&meta), Some(0o755));
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_checks_mode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("script.sh");
        std::fs::write(&file, "#!/bin/sh").unwrap();

        set_file_permissions(&file, 0o644).unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        assert!(!is_executable(&file, &meta));

        set_file_permissions(&file, 0o755).unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        assert!(is_executable(&file, &meta));
    }

    #[cfg(unix)]
    #[test]
    fn is_same_inode_detects_hard_links() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("original.txt");
        std::fs::write(&file, "content").unwrap();
        let link = dir.path().join("hardlink.txt");
        std::fs::hard_link(&file, &link).unwrap();

        assert!(is_same_inode(&file, &link));
        assert!(!is_same_inode(&file, &dir.path().join("nonexistent")));
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
        // Use two independent tempdirs so the target exists on every platform
        // (/tmp is absent on Windows) and lives outside our designated root.
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let result = validate_path_within(outside.path(), root.path());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            err.to_string().contains("escapes root"),
            "expected 'escapes root' message, got: {err}"
        );
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
    #[cfg(unix)] // Windows LockFileEx prevents reading lock file content while held
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
    fn shell_escape_value_metacharacters() {
        // Contains both single-quote AND $, so must use break-out escaping
        assert_eq!(shell_escape_value("it's a $test"), "'it'\\''s a $test'");
    }

    #[test]
    fn shell_escape_value_backtick() {
        assert_eq!(shell_escape_value("`cmd`"), "'`cmd`'");
    }

    #[test]
    fn shell_escape_value_backslash() {
        assert_eq!(shell_escape_value("a\\b"), "'a\\b'");
    }

    #[test]
    fn shell_escape_value_empty() {
        assert_eq!(shell_escape_value(""), "\"\"");
    }

    #[test]
    fn xml_escape_all_entities() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
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
        let home = home_dir_var().expect("home directory must be available in test");
        let expected = std::path::PathBuf::from(home).join("test");
        assert_eq!(result, expected);
    }

    #[test]
    fn expand_tilde_absolute_unchanged() {
        let result = expand_tilde(std::path::Path::new("/absolute/path"));
        assert_eq!(result, std::path::PathBuf::from("/absolute/path"));
    }

    #[test]
    fn with_test_home_scopes_override_and_restores() {
        // Sanity: no override active at start (other tests' guards must have
        // been released before this one ran on the same thread).
        assert!(test_home_override().is_none());

        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().to_path_buf();

        let expanded = with_test_home(&fake_home, || {
            // While scoped, `~` resolves to the tempdir.
            expand_tilde(std::path::Path::new("~/sub/file"))
        });
        assert_eq!(expanded, fake_home.join("sub").join("file"));

        // Override cleared on closure return.
        assert!(test_home_override().is_none());
    }

    #[test]
    fn with_test_home_restores_on_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().to_path_buf();

        // catch_unwind to observe the panic without aborting the test.
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_test_home(&fake_home, || {
                assert_eq!(test_home_override().as_deref(), Some(fake_home.as_path()));
                panic!("simulated failure");
            });
        }))
        .is_err();
        assert!(panicked, "closure should have panicked");

        // Guard must have restored on unwind.
        assert!(test_home_override().is_none());
    }

    #[test]
    fn with_test_home_nests_correctly() {
        let outer_tmp = tempfile::tempdir().unwrap();
        let inner_tmp = tempfile::tempdir().unwrap();
        let outer = outer_tmp.path().to_path_buf();
        let inner = inner_tmp.path().to_path_buf();

        with_test_home(&outer, || {
            assert_eq!(test_home_override().as_deref(), Some(outer.as_path()));
            with_test_home(&inner, || {
                assert_eq!(test_home_override().as_deref(), Some(inner.as_path()));
            });
            // Inner guard dropped — outer restored.
            assert_eq!(test_home_override().as_deref(), Some(outer.as_path()));
        });
        assert!(test_home_override().is_none());
    }

    #[test]
    fn default_config_dir_follows_override() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().to_path_buf();
        let observed = with_test_home(&fake_home, default_config_dir);
        assert_eq!(observed, fake_home.join(".config").join("cfgd"));
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

    #[test]
    fn validate_env_var_name_accepts_valid() {
        assert!(validate_env_var_name("PATH").is_ok());
        assert!(validate_env_var_name("_PRIVATE").is_ok());
        assert!(validate_env_var_name("MY_VAR_123").is_ok());
        assert!(validate_env_var_name("a").is_ok());
    }

    #[test]
    fn validate_env_var_name_rejects_invalid() {
        let err = validate_env_var_name("").unwrap_err();
        assert!(err.contains("empty"), "empty should say empty: {err}");

        let err = validate_env_var_name("1STARTS_WITH_DIGIT").unwrap_err();
        assert!(
            err.contains("must start with"),
            "digit-prefix should explain: {err}"
        );

        // All of these should fail with the "invalid characters" message
        for bad in ["HAS SPACE", "HAS;SEMI", "HAS$DOLLAR", "HAS-DASH", "a=b"] {
            assert!(
                validate_env_var_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn validate_alias_name_accepts_valid() {
        assert!(validate_alias_name("ls").is_ok());
        assert!(validate_alias_name("my-alias").is_ok());
        assert!(validate_alias_name("my.alias").is_ok());
        assert!(validate_alias_name("my_alias_123").is_ok());
    }

    #[test]
    fn validate_alias_name_rejects_invalid() {
        let err = validate_alias_name("").unwrap_err();
        assert!(err.contains("empty"), "empty should say empty: {err}");

        for bad in ["has space", "has;semi", "has$dollar", "a=b", "has/slash"] {
            let err = validate_alias_name(bad).unwrap_err();
            assert!(
                err.contains("must contain only"),
                "{bad:?} rejection should explain allowed chars: {err}"
            );
        }
    }

    #[test]
    fn parse_env_var_validates_name() {
        let ev = parse_env_var("VALID=value").unwrap();
        assert_eq!(ev.name, "VALID");
        assert_eq!(ev.value, "value");

        assert!(
            parse_env_var("1BAD=value")
                .unwrap_err()
                .contains("must start with"),
            "digit-leading name should fail"
        );
        assert!(parse_env_var("BAD;NAME=value").is_err());
    }

    #[test]
    fn parse_env_var_value_with_equals() {
        // Values can contain '=' — only the first '=' splits key from value
        let ev = parse_env_var("PATH=/usr/bin:/bin").unwrap();
        assert_eq!(ev.name, "PATH");
        assert_eq!(ev.value, "/usr/bin:/bin");

        let ev2 = parse_env_var("FOO=a=b=c").unwrap();
        assert_eq!(ev2.name, "FOO");
        assert_eq!(ev2.value, "a=b=c");
    }

    #[test]
    fn parse_env_var_empty_value() {
        let ev = parse_env_var("EMPTY=").unwrap();
        assert_eq!(ev.name, "EMPTY");
        assert_eq!(ev.value, "");
    }

    #[test]
    fn parse_env_var_no_equals() {
        let err = parse_env_var("NOEQUALS").unwrap_err();
        assert!(
            err.contains("KEY=VALUE"),
            "should tell user the expected format, got: {err}"
        );
    }

    #[test]
    fn parse_alias_validates_name() {
        let a = parse_alias("valid=ls -la").unwrap();
        assert_eq!(a.name, "valid");
        assert_eq!(a.command, "ls -la");

        let a2 = parse_alias("my-alias=git status").unwrap();
        assert_eq!(a2.name, "my-alias");
        assert_eq!(a2.command, "git status");

        assert!(parse_alias("bad;name=cmd").is_err());
    }

    #[test]
    fn parse_alias_command_with_equals() {
        // Command can contain '=' — only the first splits name from command
        let a = parse_alias("env=FOO=bar baz").unwrap();
        assert_eq!(a.name, "env");
        assert_eq!(a.command, "FOO=bar baz");
    }
}
