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
/// `home_dir_var` / `default_config_dir`, and by `tests` to assert that the
/// guard was installed/cleared as expected.
pub(crate) fn test_home_override() -> Option<std::path::PathBuf> {
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

/// Per-user runtime directory for short-lived sockets and pid files.
///
/// Resolution order:
/// - Linux: `$XDG_RUNTIME_DIR/cfgd` if set, else `$HOME/.cache/cfgd`. The base
///   `$XDG_RUNTIME_DIR` is owner-private by spec; the cache fallback is
///   under the user's home where Linux-default permissions already protect it.
/// - macOS: `$HOME/Library/Application Support/cfgd`. There is no
///   per-user `tmpfs` on macOS, and `$TMPDIR` is per-user but still
///   world-traversable when the umask leaks; Application Support is the
///   conventional per-user location for app state.
/// - Windows: `%LOCALAPPDATA%\cfgd` via `directories::BaseDirs`. (Daemons on
///   Windows use named pipes, which are kernel objects — this path is
///   provided for parity and is unused by the daemon socket flow.)
///
/// Honors the [`TestHomeGuard`] thread-local override on every platform so
/// tests can redirect the runtime dir without mutating process-global env
/// state. Returns `None` only when no home directory can be resolved at all.
pub fn default_runtime_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        // XDG_RUNTIME_DIR is a per-user tmpfs (typically 0700) on systemd
        // systems — prefer it. Test override of HOME does not shadow it
        // because tests that need a deterministic socket path point
        // XDG_RUNTIME_DIR at a tempdir directly.
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
            let xdg = std::path::PathBuf::from(xdg);
            if !xdg.as_os_str().is_empty() {
                return Some(xdg.join("cfgd"));
            }
        }
        let home = home_dir_var()?;
        Some(std::path::PathBuf::from(home).join(".cache").join("cfgd"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = home_dir_var()?;
        Some(
            std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("cfgd"),
        )
    }
    #[cfg(windows)]
    {
        if let Some(home) = test_home_override() {
            return Some(home.join("AppData").join("Local").join("cfgd"));
        }
        directories::BaseDirs::new().map(|b| b.data_local_dir().join("cfgd"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let home = home_dir_var()?;
        Some(std::path::PathBuf::from(home).join(".cache").join("cfgd"))
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
pub(crate) fn home_dir_var() -> Option<String> {
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
