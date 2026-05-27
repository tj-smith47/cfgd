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

/// Always-fold POSIX form of a path. Use anywhere a path crosses into JSON,
/// YAML, SQLite, gateway API, OCI annotations, `file://` URLs, or snapshot
/// goldens. Backslash is treated as a separator; legitimate backslash-in-
/// filename on POSIX is sacrificed for cross-OS state portability (see the
/// path-handling consolidation spec for the fold-policy rationale).
pub fn to_posix_string(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

/// Fold `\` → `/` in free-form text that may contain native-separator paths.
/// `Cow` so the unix path stays borrowed; only Windows captures pay for the
/// allocation.
pub fn posixify_text(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\\') {
        std::borrow::Cow::Owned(s.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Build a `file://` URL that round-trips through `url::Url::parse` on both
/// unix (`file:///home/foo`) and Windows (`file:///C:/Users/foo`). Replaces
/// every hand-rolled `format!("file://{}", path.display())` callsite that
/// silently emits backslashes and a missing third slash on Windows.
pub fn to_file_url(path: impl AsRef<std::path::Path>) -> String {
    let s = to_posix_string(path);
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// CRLF → LF, for paired use with [`posixify_text`] in snapshot normalization.
/// `Cow` so unix captures stay borrowed.
pub fn normalize_line_endings(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains("\r\n") {
        std::borrow::Cow::Owned(s.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Composite normalizer for snapshot tests: CRLF→LF, fold `\`→`/`, then
/// substitute each `(path, placeholder)` pair. Substitutions are applied
/// longest-first to handle nested temp paths correctly (e.g. when
/// `<BARE>/inner` and `<BARE_ROOT>` both match, longest wins). Each path is
/// posixified before substitution so the captured text and the substitution
/// keys share the same separator convention.
pub fn normalize_for_snapshot(captured: &str, paths: &[(&std::path::Path, &str)]) -> String {
    let lf = normalize_line_endings(captured);
    let posix = posixify_text(&lf);
    let os = posixify_os_error_text(&posix);
    let mut subs: Vec<(String, &str)> = paths
        .iter()
        .map(|(p, label)| (to_posix_string(p), *label))
        .collect();
    subs.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    let mut out = os.into_owned();
    for (p, label) in subs {
        if p.is_empty() {
            continue;
        }
        out = out.replace(&p, label);
    }
    out
}

/// Collapse OS-specific `std::io::Error` text in captured snapshot output.
/// Linux emits `... File exists (os error 17)` for `ErrorKind::AlreadyExists`;
/// Windows emits `... Cannot create a file when that file already exists.
/// (os error 183)` for the same kind. Both fold to a stable `<os error>`
/// placeholder so a single golden file works on both. Use after path
/// normalization in [`normalize_for_snapshot`]-style pipelines for tests
/// that touch the filesystem.
pub fn posixify_os_error_text(s: &str) -> std::borrow::Cow<'_, str> {
    const MARKER: &str = "(os error ";
    if !s.contains(MARKER) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(idx) = rest.find(MARKER) else {
            out.push_str(rest);
            break;
        };
        let after_open = &rest[idx + MARKER.len()..];
        let digits_end = after_open
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after_open.len());
        let is_well_formed = digits_end > 0 && after_open.as_bytes().get(digits_end) == Some(&b')');
        if !is_well_formed {
            // Not a real OS-error marker — emit one byte and continue scanning.
            let safe_end = idx + 1;
            out.push_str(&rest[..safe_end]);
            rest = &rest[safe_end..];
            continue;
        }
        // Walk back from `idx` to the last "<sep>: " — that's the boundary
        // between the error prefix (e.g. "io error on <PATH>: ") and the
        // OS-native prose we collapse.
        let prefix = &rest[..idx];
        let cut = prefix.rfind(": ").map(|p| p + 2).unwrap_or(idx);
        out.push_str(&prefix[..cut]);
        out.push_str("<os error>");
        rest = &after_open[digits_end + 1..];
    }
    std::borrow::Cow::Owned(out)
}

/// User-input path tolerance: accept `C:\foo`, `C:/foo`, `~/foo`, `./foo`.
/// Folds `\` → `/` and expands a leading `~` via [`expand_tilde`]. Use when
/// loading config fields where a Linux author may write `/` and a Windows
/// author may write `\` for the same logical location.
pub fn from_user_input(s: &str) -> std::path::PathBuf {
    let folded = if s.contains('\\') {
        s.replace('\\', "/")
    } else {
        s.to_string()
    };
    expand_tilde(std::path::Path::new(&folded))
}
