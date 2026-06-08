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

/// Default config directory.
///
/// Resolution order:
/// 1. `$XDG_CONFIG_HOME/cfgd` when `XDG_CONFIG_HOME` is set to a **non-empty,
///    absolute** path. The XDG Base Directory spec mandates that empty or
///    relative values be treated as unset (joining a relative value would yield
///    a CWD-dependent config path). Honored on every platform, so an explicit
///    `XDG_CONFIG_HOME` relocates the config dir on macOS and Windows too.
/// 2. the platform-native config base joined with `cfgd`:
///    - Linux: `~/.config/cfgd`
///    - macOS: `~/Library/Application Support/cfgd` — the same native root the
///      state ([`crate::state::default_state_dir`]) and runtime
///      ([`default_runtime_dir`]) directories use, so all per-user cfgd data
///      shares one location instead of splitting config under `~/.config`.
///    - Windows: `%APPDATA%\cfgd`
///
/// The home directory is resolved from `HOME`/`USERPROFILE` only, never the
/// passwd database: when home cannot be resolved the path stays a literal
/// `~/.config/cfgd` so the caller surfaces a clean error and writes nothing,
/// instead of silently resolving to the account's real home.
pub fn default_config_dir() -> std::path::PathBuf {
    // Thread-local test override always wins. Lets tests redirect config
    // lookup to a tempdir without mutating global env state.
    if let Some(home) = test_home_override() {
        return home.join(".config").join("cfgd");
    }
    if let Some(dir) = xdg_config_home_cfgd() {
        return dir;
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir_var() {
            return std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("cfgd");
        }
        expand_tilde(std::path::Path::new("~/.config/cfgd"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // `expand_tilde` resolves from HOME only and leaves `~` literal when
        // HOME is unset — the clean-failure guarantee above.
        expand_tilde(std::path::Path::new("~/.config/cfgd"))
    }
    #[cfg(windows)]
    {
        directories::BaseDirs::new()
            .map(|b| b.config_dir().join("cfgd"))
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData\cfgd"))
    }
}

/// `$XDG_CONFIG_HOME/cfgd` when the variable is a non-empty, absolute path;
/// `None` otherwise (the spec treats empty/relative values as unset). Honored on
/// every platform so an explicit `XDG_CONFIG_HOME` works on macOS and Windows
/// too, not only Linux.
fn xdg_config_home_cfgd() -> Option<std::path::PathBuf> {
    let raw = std::env::var_os("XDG_CONFIG_HOME")?;
    let path = std::path::PathBuf::from(raw);
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return None;
    }
    Some(path.join("cfgd"))
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

/// Expand `~`/`~/` segments in a colon-separated environment value to the user's
/// home directory.
///
/// Declared `spec.env` values are written into managed shell files inside double
/// quotes (and injected directly into child process environments), where the
/// shell performs no tilde expansion — a literal `~/.local/bin` would stay broken.
/// This expands a leading `~`, and any `~` following a `:` (PATH-style, matching
/// the unquoted shell-assignment semantics a user expects), while leaving every
/// other segment byte-for-byte unchanged. `$VAR` references are NOT touched: in a
/// double-quoted bash/zsh value the shell still expands those at source time, and
/// pre-expanding `…:$PATH` would freeze a stale PATH into the file.
pub fn expand_env_value_tilde(value: &str) -> String {
    value
        .split(':')
        .map(|seg| {
            expand_tilde(std::path::Path::new(seg))
                .display()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(":")
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
                canonical_path.posix(),
                canonical_root.posix()
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
            return Err(format!("path contains '..': {}", path.posix()));
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

/// The cfgd crate version baked into version-bearing CLI output
/// (`plugin version`, `upgrade --check`, ...). cfgd and cfgd-core are
/// version-synced in lockstep by anodizer (`version_sync` in `.anodizer.yaml`),
/// so cfgd-core's own `CARGO_PKG_VERSION` equals the cfgd binary's at every
/// release — making it the stable source for version normalization.
pub const CURRENT_CFGD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Replace the exact current cfgd version literal with a stable `<VERSION>`
/// placeholder so version-bearing snapshot goldens survive release bumps
/// without per-bump edits.
///
/// Only the *exact* current version is substituted — a genuinely wrong
/// version (e.g. a stale `0.3.0` leaking into output) still fails to match
/// `<VERSION>` and surfaces as a snapshot mismatch, so real version bugs are
/// not masked. Fixture version strings (package versions, pinned tags, mocked
/// release numbers) are left untouched because they never equal the running
/// crate version.
pub fn normalize_cfgd_version(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(CURRENT_CFGD_VERSION) {
        std::borrow::Cow::Owned(s.replace(CURRENT_CFGD_VERSION, "<VERSION>"))
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
/// placeholder so a single golden file works on both.
///
/// Also collapses libgit2's `<prose>; class=Os (N)` form to
/// `<os error>; class=Os (N)` — Linux libgit2 emits
/// `... No such file or directory; class=Os (2)`, Windows libgit2 emits
/// `... The system cannot find the file specified. — ; class=Os (2)`.
/// Different prose, same logical error; fold to the common prefix shape
/// so the golden is OS-independent.
///
/// Use after path normalization in [`normalize_for_snapshot`]-style
/// pipelines for tests that touch the filesystem or git.
pub fn posixify_os_error_text(s: &str) -> std::borrow::Cow<'_, str> {
    const STD_MARKER: &str = "(os error ";
    const GIT_MARKER: &str = "; class=Os (";
    if !s.contains(STD_MARKER) && !s.contains(GIT_MARKER) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        // Pick whichever marker appears next in `rest` — process each in turn.
        let std_idx = rest.find(STD_MARKER);
        let git_idx = rest.find(GIT_MARKER);
        let (idx, marker, is_git) = match (std_idx, git_idx) {
            (None, None) => {
                out.push_str(rest);
                break;
            }
            (Some(i), None) => (i, STD_MARKER, false),
            (None, Some(i)) => (i, GIT_MARKER, true),
            (Some(s_i), Some(g_i)) => {
                if s_i <= g_i {
                    (s_i, STD_MARKER, false)
                } else {
                    (g_i, GIT_MARKER, true)
                }
            }
        };
        let after_open = &rest[idx + marker.len()..];
        let digits_end = after_open
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after_open.len());
        let is_well_formed = digits_end > 0 && after_open.as_bytes().get(digits_end) == Some(&b')');
        if !is_well_formed {
            // Not a real marker — emit one byte and continue scanning.
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
        if is_git {
            // Preserve the `; class=Os (N)` tail so consumers that grep
            // for the libgit2 marker still see it.
            out.push_str(GIT_MARKER);
            out.push_str(&after_open[..digits_end + 1]);
        }
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

/// Display-only extension for human-facing path output. On Windows, folds
/// `\` → `/` so a status subject or error message shows POSIX-form paths
/// consistently across runners. On Unix, passes through unchanged — a
/// legitimate `\` in a Unix filename survives byte-for-byte.
///
/// `display_posix()` is the eager form (returns `String`).
/// `posix()` is the lazy form — returns `impl Display` so it composes with
/// `format!`/`write!`/`println!` without an intermediate allocation.
///
/// Use in:
/// - Printer status subjects (`status[_simple]`, kv values, error messages)
/// - `tracing::info!`/`warn!`/`error!` event fields where the path is the
///   human-visible value
///
/// Do NOT use in:
/// - JSON / YAML / SQLite / OCI / gateway boundaries — use
///   [`to_posix_string`] instead (always folds, not Windows-only)
/// - Debug-only `tracing::debug!`/`trace!` event fields — keep native so
///   debug tooling sees what's on disk
pub trait PathDisplayExt {
    /// Eager: returns a `String` with `\` folded to `/` on Windows, native on Unix.
    fn display_posix(&self) -> String;
    /// Lazy: returns a `Display` adapter suitable for `format!` / `write!`.
    fn posix(&self) -> PathPosix<'_>;
}

impl<P: AsRef<std::path::Path>> PathDisplayExt for P {
    fn display_posix(&self) -> String {
        #[cfg(windows)]
        {
            to_posix_string(self.as_ref())
        }
        #[cfg(not(windows))]
        {
            self.as_ref().display().to_string()
        }
    }

    fn posix(&self) -> PathPosix<'_> {
        PathPosix(self.as_ref())
    }
}

/// `Display` adapter returned by [`PathDisplayExt::posix`]. On Windows,
/// renders the path with `\` → `/` substitution; on Unix it's
/// indistinguishable from `Path::display()`.
pub struct PathPosix<'a>(&'a std::path::Path);

impl std::fmt::Display for PathPosix<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[cfg(windows)]
        {
            let s = self.0.to_string_lossy();
            for ch in s.chars() {
                let mapped = if ch == '\\' { '/' } else { ch };
                std::fmt::Write::write_char(f, mapped)?;
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            std::fmt::Display::fmt(&self.0.display(), f)
        }
    }
}
