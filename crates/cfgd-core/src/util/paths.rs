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

/// `tokio::task::spawn_blocking` that carries the test-home thread-local onto
/// the blocking-pool worker.
///
/// The override lives in a thread-local, so a plain `spawn_blocking` closure
/// that resolves `~` / `$HOME` (via `home_dir_var`, `default_state_dir`, …)
/// silently falls back to the ambient `$HOME` — a real-filesystem touch under
/// parallel tests. This wrapper captures the caller's override and re-installs
/// it as the worker's first statement, so blocking dispatch sites cannot
/// forget the guard. In production the override is always `None`; the wrapper
/// costs one thread-local read and is otherwise transparent.
pub fn spawn_blocking_with_test_home<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let test_home = test_home_override();
    tokio::task::spawn_blocking(move || {
        let _guard = test_home.as_deref().map(with_test_home_guard);
        f()
    })
}

/// Read the current test HOME override (if any). Only used internally by
/// `home_dir_var` / `default_config_dir`, and by `tests` to assert that the
/// guard was installed/cleared as expected.
pub(crate) fn test_home_override() -> Option<std::path::PathBuf> {
    TEST_HOME_OVERRIDE.with(|o| o.borrow().clone())
}

/// Deployment scope selecting the family of base directories cfgd writes under.
///
/// [`Scope::User`] resolves the per-user XDG / platform-native locations (the
/// historical default). [`Scope::System`] resolves the machine-wide FHS
/// (`/etc`, `/var/lib`, `/var/cache`, `/run`) locations on Linux, `/Library`
/// locations on macOS, and `%ProgramData%` locations on Windows — cfgd runs as
/// root in this scope. The scope-aware `default_*_for(scope)` resolvers select
/// the family; the zero-argument `default_*` overloads are the `User` default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Per-user directories (XDG / `~/...` / `%APPDATA%`). The default.
    User,
    /// Machine-wide directories (FHS / `/Library` / `%ProgramData%`). cfgd runs
    /// as root.
    System,
}

impl Scope {
    /// Map a system-scope bool to a scope: `true` → [`Scope::System`], `false` →
    /// [`Scope::User`].
    pub fn from_system_flag(system: bool) -> Self {
        if system { Scope::System } else { Scope::User }
    }

    /// Whether this is the machine-wide [`Scope::System`].
    pub fn is_system(&self) -> bool {
        matches!(self, Scope::System)
    }
}

/// First `:`-separated entry of a systemd directory env var (e.g.
/// `STATE_DIRECTORY`) as a `PathBuf`, or `None` when unset or empty.
///
/// systemd sets `CONFIGURATION_DIRECTORY` / `STATE_DIRECTORY` /
/// `CACHE_DIRECTORY` / `RUNTIME_DIRECTORY` for a service process whose unit
/// declares the matching `*Directory=` setting; the value is a colon-separated
/// list and the first entry is the primary directory. These vars are only ever
/// present in a systemd-managed process, so honoring them whenever set (in any
/// scope) routes cfgd to exactly the directory systemd provisioned.
pub(crate) fn systemd_dir(env_var: &str) -> Option<std::path::PathBuf> {
    let raw = std::env::var_os(env_var)?;
    if raw.is_empty() {
        return None;
    }
    // The value is a `:`-separated list; the primary directory is the first
    // non-empty entry. `OsStr` has no `split`, so route through the lossy form —
    // systemd directory paths are ordinary filesystem paths.
    let value = raw.to_string_lossy();
    value
        .split(':')
        .find(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

/// Default config directory.
///
/// Resolution order:
/// 1. `$XDG_CONFIG_HOME/cfgd` when `XDG_CONFIG_HOME` is set to a **non-empty,
///    absolute** path. The XDG Base Directory spec mandates that empty or
///    relative values be treated as unset (joining a relative value would yield
///    a CWD-dependent config path). Honored on every platform, so an explicit
///    `XDG_CONFIG_HOME` relocates the config dir on macOS and Windows too.
/// 2. the platform-native config location:
///    - Linux: `~/.config/cfgd`
///    - macOS: `~/Library/Application Support/cfgd` for a fresh install — the
///      same native root the state ([`crate::state::default_state_dir`]) and
///      runtime ([`default_runtime_dir`]) directories use, so all per-user cfgd
///      data shares one location. An existing `~/.config/cfgd` (from a build
///      that used it) is always preferred and read in place so an upgrade never
///      strands config; the CLI offers a one-time prompt to move it or pin
///      `XDG_CONFIG_HOME` (see `resolve_macos_config_dir` and
///      [`macos_legacy_config_migration`]).
///    - Windows: `%APPDATA%\cfgd`
///
/// The home directory is resolved from `HOME`/`USERPROFILE` only, never the
/// passwd database: when home cannot be resolved the path stays a literal
/// `~/.config/cfgd` so the caller surfaces a clean error and writes nothing,
/// instead of silently resolving to the account's real home.
pub fn default_config_dir() -> std::path::PathBuf {
    default_config_dir_for(Scope::User)
}

/// Scope-aware config directory.
///
/// Precedence (highest first): systemd's `$CONFIGURATION_DIRECTORY` (set only in
/// a systemd-managed process), then the scope default. [`Scope::User`] is the
/// frozen XDG / platform-native resolution documented on [`default_config_dir`];
/// [`Scope::System`] is the absolute machine-wide config root (Linux `/etc/cfgd`,
/// macOS `/Library/Application Support/cfgd`, Windows `%ProgramData%\cfgd`) and
/// consults neither the test-home override nor XDG. Config has no
/// `CFGD_CONFIG_DIR` short-circuit here — the CLI threads that as an explicit
/// override. Pure path logic — never touches the filesystem.
pub fn default_config_dir_for(scope: Scope) -> std::path::PathBuf {
    if let Some(dir) = systemd_dir("CONFIGURATION_DIRECTORY") {
        return dir;
    }
    if scope.is_system() {
        return system_config_dir();
    }
    // Thread-local test override always wins for the user scope. Lets tests
    // redirect config lookup to a tempdir without mutating global env state.
    if let Some(home) = test_home_override() {
        return home.join(".config").join("cfgd");
    }
    if let Some(dir) = xdg_config_home_cfgd() {
        return dir;
    }
    #[cfg(target_os = "macos")]
    {
        match home_dir_var() {
            Some(home) => resolve_macos_config_dir(std::path::Path::new(&home), |p| p.is_dir()),
            // HOME unresolved: keep the literal `~/.config/cfgd` so the caller
            // fails cleanly instead of writing to a passwd-derived home.
            None => expand_tilde(std::path::Path::new("~/.config/cfgd")),
        }
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

/// The machine-wide config root: Linux `/etc/cfgd`, macOS
/// `/Library/Application Support/cfgd`, Windows `%ProgramData%\cfgd`. Absolute on
/// every platform — never consults a home directory or the test-home override.
fn system_config_dir() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        std::path::PathBuf::from("/etc/cfgd")
    }
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from("/Library/Application Support/cfgd")
    }
    #[cfg(windows)]
    {
        program_data_dir().join("cfgd")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        std::path::PathBuf::from("/etc/cfgd")
    }
}

/// Windows `%ProgramData%` (via `ProgramData` env), with the `C:\ProgramData`
/// fallback the rest of cfgd uses. Only compiled on Windows — the only platform
/// whose system roots nest under `%ProgramData%`.
#[cfg(windows)]
pub(crate) fn program_data_dir() -> std::path::PathBuf {
    std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"))
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

/// Resolve the macOS config directory.
///
/// The native macOS location for a fresh install is
/// `~/Library/Application Support/cfgd` — the same root as state and runtime, so
/// all per-user cfgd data shares one place. An existing `~/.config/cfgd` is
/// always preferred (read in place) so an upgrade from a build that used
/// `~/.config` never strands a user's config; the CLI separately offers a
/// one-time prompt to move it or pin `XDG_CONFIG_HOME` (see
/// [`macos_legacy_config_migration`]). Discovery itself is read-only — it never
/// moves files.
///
/// `exists` is injected (rather than calling [`std::path::Path::is_dir`]
/// directly) so the resolution order is unit-testable on every platform, not
/// only macOS.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn resolve_macos_config_dir(
    home: &std::path::Path,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    let dotconfig = home.join(".config").join("cfgd");
    if exists(&dotconfig) {
        return dotconfig;
    }
    home.join("Library")
        .join("Application Support")
        .join("cfgd")
}

/// When a legacy `~/.config/cfgd` config dir exists on macOS while the native
/// `~/Library/Application Support/cfgd` location does not, return the
/// `(legacy, native)` pair so the CLI can offer a one-time migration. Returns
/// `None` when there is nothing to migrate (no legacy dir, or the native
/// location already exists).
///
/// Compiled on every platform so the CLI migration entry point type-checks in
/// CI; callers gate the *behavior* to macOS (the native `~/Library` layout is
/// meaningless elsewhere).
pub fn macos_legacy_config_migration(
    home: &std::path::Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let legacy = home.join(".config").join("cfgd");
    let native = home
        .join("Library")
        .join("Application Support")
        .join("cfgd");
    if legacy.is_dir() && !native.exists() {
        Some((legacy, native))
    } else {
        None
    }
}

/// Move a directory tree from `src` to `dst`, creating `dst`'s parent first.
///
/// Refuses when `dst` already exists (so a racing creation during a caller's
/// interactive pause can't be clobbered). Tries an atomic `rename` (the
/// same-filesystem fast path) and falls back to a symlink-preserving recursive
/// copy + remove when the paths live on different filesystems (`rename` then
/// fails with `EXDEV`). On a partial copy the destination is rolled back so a
/// failed move never strands two divergent copies.
pub fn move_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if dst.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", dst.posix()),
        ));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        // EXDEV (errno 18 on Linux/macOS): cross-filesystem rename is rejected;
        // fall back to copy-then-remove. Other errors (permissions, missing
        // source) surface unchanged.
        Err(e) if e.raw_os_error() == Some(18) => {
            if let Err(copy_err) = copy_tree_preserving_symlinks(src, dst) {
                let _ = std::fs::remove_dir_all(dst);
                return Err(copy_err);
            }
            if let Err(rm_err) = std::fs::remove_dir_all(src) {
                let _ = std::fs::remove_dir_all(dst);
                return Err(rm_err);
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Recreate `link` — a symlink inside a tree being copied — at `at`.
///
/// The ONE recreate both preserving copies take, and the ONE wording of a host
/// that will not make links: the refusal names the link inside the SOURCE tree,
/// the entry an operator can see and act on, rather than the counterpart under a
/// copy cfgd named. Degrading to a skip would drop the link silently, which is
/// exactly the neither-generation loss a preserving copy exists to prevent;
/// [`crate::create_symlink`] words the privilege fix already.
fn recreate_link(link: &std::path::Path, at: &std::path::Path) -> std::io::Result<()> {
    let dest = std::fs::read_link(link)?;
    super::fs_perms::create_symlink(&dest, at)
        .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", link.posix())))
}

/// Recursively copy a directory tree, recreating symlinks as symlinks (unlike
/// [`copy_dir_recursive`], which skips them). Used only by [`move_dir`]'s
/// cross-filesystem fallback, where the whole tree is owned by the mover and
/// dropping symlinked entries would silently lose data.
fn copy_tree_preserving_symlinks(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if file_type.is_symlink() {
            recreate_link(&entry.path(), &to)?;
        } else if file_type.is_dir() {
            copy_tree_preserving_symlinks(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Move a single file from `src` to `dst`, creating `dst`'s parent first.
///
/// The single-file sibling of [`move_dir`]. Refuses when `dst` already exists
/// (so a prior migration is never clobbered). Tries an atomic `rename` (the
/// same-filesystem fast path) and falls back to copy + remove when the paths
/// live on different filesystems (`rename` then fails with `EXDEV`). On a failed
/// copy or a failed source-removal the destination is rolled back so a degraded
/// move never strands two divergent copies.
pub fn move_file(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if dst.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", dst.posix()),
        ));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        // EXDEV (errno 18 on Linux/macOS): cross-filesystem rename is rejected;
        // fall back to copy-then-remove. Other errors surface unchanged.
        Err(e) if e.raw_os_error() == Some(18) => {
            if let Err(copy_err) = std::fs::copy(src, dst) {
                let _ = std::fs::remove_file(dst);
                return Err(copy_err);
            }
            if let Err(rm_err) = std::fs::remove_file(src) {
                let _ = std::fs::remove_file(dst);
                return Err(rm_err);
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// The pre-split default data directory (`<data_local>/cfgd`) — the single
/// location that held both the state DB and the `sources/` cache before they
/// moved to independent state and cache roots.
///
/// Reproduced here (rather than inlined at the migration call site) so the
/// startup migration and its tests share one definition. This is the legacy
/// *default* only: it never honors `CFGD_STATE_DIR`/`CFGD_CACHE_DIR` (those are
/// overrides, not the legacy default). Pure path logic — touches no filesystem.
///
/// Honors the [`TestHomeGuard`] thread-local override (test builds resolve a
/// Linux-shaped `~/.local/share/cfgd` under the override home) so tests never
/// read the real data dir. Returns `None` when no home directory is resolvable.
pub fn legacy_data_dir() -> Option<std::path::PathBuf> {
    if let Some(home) = test_home_override() {
        return Some(home.join(".local").join("share").join("cfgd"));
    }
    Some(directories::BaseDirs::new()?.data_local_dir().join("cfgd"))
}

/// Per-user runtime directory for short-lived sockets and pid files.
///
/// Resolution order:
/// - Linux: `$XDG_RUNTIME_DIR/cfgd` if set, else `$HOME/.cache/cfgd/runtime`.
///   The base `$XDG_RUNTIME_DIR` is owner-private by spec; the cache fallback
///   nests under a `runtime/` subdir of the cache root so the socket/lock never
///   land directly in the cache root (where module/source caches live).
/// - macOS: `$HOME/Library/Application Support/cfgd/runtime`. There is no
///   per-user `tmpfs` on macOS, and `$TMPDIR` is per-user but still
///   world-traversable when the umask leaks; Application Support is the
///   conventional per-user location for app state. The `runtime/` subdir
///   de-collides the socket/lock from the config root, which on macOS shares
///   the same Application Support parent.
/// - Windows: `%LOCALAPPDATA%\cfgd` via `directories::BaseDirs`. (Daemons on
///   Windows use named pipes, which are kernel objects — this path is
///   provided for parity and is unused by the daemon socket flow.)
///
/// `CFGD_RUNTIME_DIR` short-circuits all resolution when set, so the env form
/// works at every call site (including non-CLI ones and the daemon).
///
/// Honors the [`TestHomeGuard`] thread-local override on every platform so
/// tests can redirect the runtime dir without mutating process-global env
/// state. Returns `None` only when no home directory can be resolved at all.
pub fn default_runtime_dir() -> Option<std::path::PathBuf> {
    default_runtime_dir_for(Scope::User)
}

/// Scope-aware runtime directory.
///
/// Precedence (highest first): `CFGD_RUNTIME_DIR` (verbatim), systemd's
/// `$RUNTIME_DIRECTORY`, then the scope default. [`Scope::User`] is the frozen
/// resolution documented on [`default_runtime_dir`]. [`Scope::System`] is the
/// absolute machine-wide runtime root (Linux `/run/cfgd`, macOS
/// `/Library/Application Support/cfgd/runtime`, Windows `%ProgramData%\cfgd\runtime`)
/// and is therefore always `Some` — it needs no home directory. Pure path logic.
pub fn default_runtime_dir_for(scope: Scope) -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("CFGD_RUNTIME_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }
    if let Some(dir) = systemd_dir("RUNTIME_DIRECTORY") {
        return Some(dir);
    }
    if scope.is_system() {
        return Some(system_runtime_dir());
    }
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
        Some(
            std::path::PathBuf::from(home)
                .join(".cache")
                .join("cfgd")
                .join("runtime"),
        )
    }
    #[cfg(target_os = "macos")]
    {
        let home = home_dir_var()?;
        Some(
            std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("cfgd")
                .join("runtime"),
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
        Some(
            std::path::PathBuf::from(home)
                .join(".cache")
                .join("cfgd")
                .join("runtime"),
        )
    }
}

/// The single per-user cache root for cfgd. Both the source cache
/// ([`ResolvedDirs::sources_dir`]) and the module cache
/// ([`ResolvedDirs::module_cache_dir`]) nest under this one root.
///
/// Resolution: the platform-native cache location with a `cfgd` segment:
/// - Linux: `$XDG_CACHE_HOME/cfgd` (default `~/.cache/cfgd`)
/// - macOS: `~/Library/Caches/cfgd`
/// - Windows: `%LOCALAPPDATA%\cfgd`
///
/// `CFGD_CACHE_DIR` short-circuits all resolution when set, so the env form
/// works at every call site (including non-CLI ones and the daemon).
///
/// Honors the [`TestHomeGuard`] thread-local override (test builds resolve a
/// Linux-shaped `~/.cache/cfgd` under the override home) so tests never write
/// to the real cache. Errors only when no home directory can be resolved.
pub fn default_cache_dir() -> crate::errors::Result<std::path::PathBuf> {
    default_cache_dir_for(Scope::User)
}

/// Scope-aware cache directory.
///
/// Precedence (highest first): `CFGD_CACHE_DIR` (verbatim), systemd's
/// `$CACHE_DIRECTORY`, then the scope default. [`Scope::User`] is the frozen
/// resolution documented on [`default_cache_dir`]. [`Scope::System`] is the
/// absolute machine-wide cache root (Linux `/var/cache/cfgd`, macOS
/// `/Library/Caches/cfgd`, Windows `%ProgramData%\cfgd\cache`) and consults no
/// home directory. Pure path logic — never touches the filesystem.
pub fn default_cache_dir_for(scope: Scope) -> crate::errors::Result<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("CFGD_CACHE_DIR") {
        return Ok(std::path::PathBuf::from(dir));
    }
    if let Some(dir) = systemd_dir("CACHE_DIRECTORY") {
        return Ok(dir);
    }
    if scope.is_system() {
        return Ok(system_cache_dir());
    }
    if let Some(home) = test_home_override() {
        return Ok(home.join(".cache").join("cfgd"));
    }
    let base =
        directories::BaseDirs::new().ok_or(crate::errors::StateError::DirectoryNotWritable {
            path: std::path::PathBuf::from("~/.cache/cfgd"),
        })?;
    Ok(base.cache_dir().join("cfgd"))
}

/// The machine-wide runtime root: Linux `/run/cfgd`, macOS
/// `/Library/Application Support/cfgd/runtime`, Windows `%ProgramData%\cfgd\runtime`.
/// Absolute on every platform — never consults a home directory.
fn system_runtime_dir() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        std::path::PathBuf::from("/run/cfgd")
    }
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from("/Library/Application Support/cfgd/runtime")
    }
    #[cfg(windows)]
    {
        program_data_dir().join("cfgd").join("runtime")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        std::path::PathBuf::from("/run/cfgd")
    }
}

/// The machine-wide cache root: Linux `/var/cache/cfgd`, macOS
/// `/Library/Caches/cfgd`, Windows `%ProgramData%\cfgd\cache`. Absolute on every
/// platform — never consults a home directory.
fn system_cache_dir() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        std::path::PathBuf::from("/var/cache/cfgd")
    }
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from("/Library/Caches/cfgd")
    }
    #[cfg(windows)]
    {
        program_data_dir().join("cfgd").join("cache")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        std::path::PathBuf::from("/var/cache/cfgd")
    }
}

/// Resolve the config directory, applying an explicit override when present.
///
/// `over` is the highest-precedence source (a `--config-dir` flag or its env
/// var, resolved by the CLI layer). When `None`, falls through to
/// [`default_config_dir_for`] for the given [`Scope`] (systemd + scope default).
/// Pure path logic — never touches the filesystem or prints.
pub fn resolve_config_dir(over: Option<&std::path::Path>, scope: Scope) -> std::path::PathBuf {
    match over {
        Some(p) => p.to_path_buf(),
        None => default_config_dir_for(scope),
    }
}

/// Resolve the state directory, applying an explicit override when present.
///
/// When `None`, falls through to [`crate::state::default_state_dir_for`] for the
/// given [`Scope`] (which honors the `CFGD_STATE_DIR` short-circuit, systemd's
/// `$STATE_DIRECTORY`, and the scope default). Surfaces the same
/// [`crate::errors::StateError`] when no home can be resolved.
pub fn resolve_state_dir(
    over: Option<&std::path::Path>,
    scope: Scope,
) -> crate::errors::Result<std::path::PathBuf> {
    match over {
        Some(p) => Ok(p.to_path_buf()),
        None => crate::state::default_state_dir_for(scope),
    }
}

/// Resolve the cache directory, applying an explicit override when present.
///
/// When `None`, falls through to [`default_cache_dir_for`] for the given
/// [`Scope`] (the single cache root shared by the source and module caches).
pub fn resolve_cache_dir(
    over: Option<&std::path::Path>,
    scope: Scope,
) -> crate::errors::Result<std::path::PathBuf> {
    match over {
        Some(p) => Ok(p.to_path_buf()),
        None => default_cache_dir_for(scope),
    }
}

/// Resolve the runtime directory, applying an explicit override when present.
///
/// When `None`, falls through to [`default_runtime_dir_for`] for the given
/// [`Scope`]. `None` only in [`Scope::User`] when no home can be resolved (e.g.
/// `HOME` unset on Unix); [`Scope::System`] is always `Some`.
pub fn resolve_runtime_dir(
    over: Option<&std::path::Path>,
    scope: Scope,
) -> Option<std::path::PathBuf> {
    match over {
        Some(p) => Some(p.to_path_buf()),
        None => default_runtime_dir_for(scope),
    }
}

/// The four resolved base directories cfgd writes under, each XDG-correct with
/// a uniform `flag > env > XDG > platform-native` override precedence.
///
/// The cache root is unified: both the source cache and module cache nest under
/// [`ResolvedDirs::cache`] (via [`ResolvedDirs::sources_dir`] /
/// [`ResolvedDirs::module_cache_dir`]) rather than each resolving an
/// independent root. `runtime` is `Option` because a home directory is required
/// to resolve it and the daemon socket/lock path may be unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDirs {
    /// Config root (YAML config, profiles). XDG_CONFIG_HOME / `~/.config/cfgd`.
    pub config: std::path::PathBuf,
    /// State root (SQLite state DB, backups). XDG_STATE_HOME / `~/.local/state/cfgd`.
    pub state: std::path::PathBuf,
    /// Unified cache root (sources + modules). XDG_CACHE_HOME / `~/.cache/cfgd`.
    pub cache: std::path::PathBuf,
    /// Runtime root (socket, pid/lock files). `None` when no home is resolvable.
    pub runtime: Option<std::path::PathBuf>,
}

impl ResolvedDirs {
    /// Resolve all four roots, threading each override through its per-role
    /// resolver. `scope` selects the user vs. machine-wide default family for any
    /// root whose override is `None`. Pure path logic — creates no directories.
    pub fn resolve(
        config_over: Option<&std::path::Path>,
        state_over: Option<&std::path::Path>,
        cache_over: Option<&std::path::Path>,
        runtime_over: Option<&std::path::Path>,
        scope: Scope,
    ) -> crate::errors::Result<Self> {
        Ok(Self {
            config: resolve_config_dir(config_over, scope),
            state: resolve_state_dir(state_over, scope)?,
            cache: resolve_cache_dir(cache_over, scope)?,
            runtime: resolve_runtime_dir(runtime_over, scope),
        })
    }

    /// The source cache directory: `<cache>/sources`.
    pub fn sources_dir(&self) -> std::path::PathBuf {
        self.cache.join("sources")
    }

    /// The module cache directory: `<cache>/modules`.
    pub fn module_cache_dir(&self) -> std::path::PathBuf {
        self.cache.join("modules")
    }
}

/// Expand `~` and `~/...` paths to the user's home directory.
/// Fold every absolute path under the home directory in `text` to its `~/`
/// spelling — the DISPLAY inverse of [`expand_tilde`], for a subject a person
/// reads (`write ~/.cfgd.env`, `deploy ~/.config/nvim/init.lua`).
///
/// Display-only, exactly as `short_commit` is: a resource id, a stored
/// description and every `-o json` field keep the absolute path, so nothing
/// structured moves. One report used to spell one file two ways — the row
/// absolute, the `source ~/.cfgd.env` hint beside it folded — and the absolute
/// form is what pushed a two-target deploy row past the room its own elision
/// respects. Folded on the POSIX spelling, so a Windows home folds too.
pub fn fold_home_in_text(text: &str) -> String {
    let Some(home) = home_dir_var() else {
        return text.to_string();
    };
    let home = to_posix_string(&home);
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return text.to_string();
    }
    text.replace(&format!("{home}/"), "~/")
}

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
            // Only a tilde segment is rewritten; the expanded home is folded to
            // forward slashes so managed shell files never carry a host-native
            // `\` on Windows. Every other segment stays byte-for-byte (a literal
            // value like `C:\tools` is the user's, not ours to normalize).
            if seg == "~" || seg.starts_with("~/") || seg.starts_with("~\\") {
                to_posix_string(expand_tilde(std::path::Path::new(seg)))
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// The character separating entries inside a `PATH` value on this host.
///
/// `std` exposes no constant for it (`env::split_paths` hides the split), and a
/// generated env file's `PATH` line is folded in one place and rendered in
/// another — two spellings of this character would concatenate two lists into
/// one unusable entry.
pub const PATH_LIST_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

/// Whether a declared `PATH` segment references the `PATH` the shell already
/// has, in any dialect's spelling. A declaration is authored once and rendered
/// into every dialect, so `$PATH` in a value that ends up in a PowerShell file
/// has to become `$env:PATH` there rather than a literal nobody set.
pub fn is_inherited_path_ref(segment: &str) -> bool {
    matches!(
        segment.trim().to_ascii_uppercase().as_str(),
        "$PATH" | "${PATH}" | "$ENV:PATH" | "%PATH%"
    )
}

/// Fold one PATH entry to the single form two spellings of the same directory
/// compare equal in: `$HOME` / `${HOME}` / `~` resolved to `home`, separators
/// folded to `/`, and any trailing `/` dropped.
///
/// Comparison only — never render the result. A generated env file is written
/// by two producers (cfgd's own bootstrapped-manager line and whatever the
/// merged `PATH` env var declares), and without one key they publish the same
/// directory twice under two spellings.
pub fn normalize_path_entry(entry: &str, home: &std::path::Path) -> String {
    let home = to_posix_string(home);
    let home = home.trim_end_matches('/');
    let entry = posixify_text(entry.trim());
    // A prefix match must end at a segment boundary, or `$HOMEBREW_PREFIX`
    // resolves as `$HOME` followed by a `BREW_PREFIX` remainder.
    let expanded = ["${HOME}", "$HOME", "~"]
        .iter()
        .find_map(|token| {
            let rest = entry.strip_prefix(token)?;
            (rest.is_empty() || rest.starts_with('/')).then(|| format!("{home}{rest}"))
        })
        .unwrap_or_else(|| entry.into_owned());
    expanded.trim_end_matches('/').to_string()
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

/// Resolve `path` to an absolute path without requiring it to exist and
/// without resolving symlinks.
///
/// A CLI entry point (`--config`) is legitimately relative *and*
/// not-yet-created — a fresh machine before `cfgd init`, or a `--config-dir`
/// naming a directory with no file inside it yet — so this must not require
/// the path to exist. `Path::canonicalize` (`realpath(3)`) was tried here
/// first, but it also resolves symlinks: a symlinked `~/.config/cfgd/cfgd.yaml`
/// pointing into a dotfiles repo — the pattern `atomic_write_resolved` exists
/// to support — would relocate `config_dir` to the dotfiles directory, and
/// every derivation from it (`profiles_dir`, a script hook's `script_dir`)
/// would relocate with it, breaking a setup that worked before the path was
/// absolutized. `std::path::absolute` is purely lexical — CWD-join plus `.`
/// component removal, no filesystem access. `..` is left as a literal
/// component on non-Windows (collapsing it lexically would silently disagree
/// with a real symlink earlier in the path — the exact ambiguity this
/// function exists to avoid); Windows's `GetFullPathNameW` does normalize it.
/// Either way this satisfies "absolute regardless of invocation form"
/// without the relocation risk of following a real symlink.
///
/// On Windows this may still carry the `\\?\` verbatim marker; stripped here
/// so the returned value matches the plain form every other absolute path in
/// cfgd uses.
pub fn absolutize_path(path: &std::path::Path) -> std::path::PathBuf {
    match std::path::absolute(path) {
        Ok(absolute) => {
            std::path::PathBuf::from(strip_windows_verbatim(&absolute.to_string_lossy()))
        }
        Err(_) => path.to_path_buf(),
    }
}

/// Resolve a relative path against a base directory with traversal validation.
/// Absolute paths are returned as-is. Relative paths are validated with
/// `validate_no_traversal` and then joined to `base`.
///
/// The *input* is validated rather than the joined result: `base` is trusted
/// config-owned state, and joining first hides a `.`-only input behind the
/// base's own components.
pub fn resolve_relative_path(
    path: &std::path::Path,
    base: &std::path::Path,
) -> std::result::Result<std::path::PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        validate_no_traversal(path)?;
        Ok(base.join(path))
    }
}

/// The ONE resolution of a `spec.files[].source` into the file the planner
/// will actually read, and the reason the plan action and every surface that
/// DESCRIBES that action name the same bytes.
///
/// `resolve_relative_path` validates the input; a resolved path that EXISTS is
/// then checked for containment in `config_dir`, because a symlink inside the
/// config directory can point anywhere and only canonicalization sees it.
/// Refused resolutions answer `None` — a caller that must say why maps it onto
/// its own error type (the file manager renders `PathTraversal`).
///
/// It lives here rather than in the file manager because `cfgd decide` renders
/// what a withheld `files.*` decision would put on the machine, and that row
/// has to read the same file the action would write. Two copies of the rule
/// meant a source-delivered or symlinked path could be described by one and
/// refused by the other. **Every managed file's source is resolved against the
/// LOCAL config directory, whatever source delivered the entry** — nothing in
/// composition rebases a source-delivered `source:` onto its own checkout — so
/// there is exactly one rule here rather than a per-origin split.
pub fn resolve_managed_file_source(
    source: &str,
    config_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if source.is_empty() {
        return None;
    }
    let resolved = resolve_relative_path(&std::path::PathBuf::from(source), config_dir).ok()?;
    if resolved.exists() && validate_path_within(&resolved, config_dir).is_err() {
        return None;
    }
    Some(resolved)
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

/// Validate that a path neither traverses upward nor collapses onto the
/// directory it would be resolved against (pre-canonicalization check).
///
/// Catches traversal attempts even when intermediate directories don't exist
/// yet, which `canonicalize()` cannot handle. A leading `./` is ordinary user
/// syntax and stays legal, but a path built only from `.` segments names
/// whatever it is joined onto rather than anything inside it — the same class of
/// mistake as `..`, and the one that turns "write this file" into "operate on
/// the whole root".
pub fn validate_no_traversal(path: &std::path::Path) -> std::result::Result<(), String> {
    let mut named_something = false;
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err("path traversal — it contains '..'".to_string());
            }
            std::path::Component::Normal(_) => named_something = true,
            _ => {}
        }
    }
    if !named_something {
        return Err("it names no file or directory of its own".to_string());
    }
    Ok(())
}

/// [`validate_no_traversal`], except that a pure self-reference (`.`, `./`) is
/// accepted instead of rejected.
///
/// For call sites where "this directory itself" is a documented, legitimate
/// answer — a module file whose `source: .` deploys the module's own tree, a
/// git `subdir: "."` that is the repository root — while `..` traversal stays
/// forbidden.
pub fn validate_no_traversal_allowing_self(
    path: &std::path::Path,
) -> std::result::Result<(), String> {
    if is_self_reference(path) {
        return Ok(());
    }
    validate_no_traversal(path)
}

/// True when `path` is non-empty but built only from `.` segments (`.`, `./`,
/// `./.`), so joining it onto a directory names that directory itself.
///
/// [`validate_no_traversal`] rejects that shape, correctly, for a value that is
/// supposed to name something *inside* a root. A few call sites mean it
/// literally — a module file whose `source: .` deploys the module's own tree, a
/// git `subdir: "."` that is the repository root — and check this first.
pub fn is_self_reference(path: &std::path::Path) -> bool {
    let mut components = path.components().peekable();
    components.peek().is_some()
        && components.all(|component| matches!(component, std::path::Component::CurDir))
}

/// Validate that `raw` is a plain relative name: at least one segment, every
/// segment an ordinary name.
///
/// Stricter than [`validate_no_traversal`], and for a different job — this is
/// for strings that *name something being created* (a snapshot, a cache
/// directory for a source), where `.` is not path-writing convenience but a lie
/// about what is named. `daily/2026` is accepted; `.`, `daily/.`, `./daily`,
/// `/daily`, `daily/`, `daily//x`, `C:/daily` and `C:daily` are not.
///
/// Judged on the raw string rather than [`std::path::Path::components`], which
/// normalizes `.` away: `"daily/."` iterates as the single plain component
/// `daily` while the joined path still ends in `/.` and resolves to `daily`
/// itself — so a caller that then removes the "new" path removes the parent of
/// everything already inside it.
pub fn validate_plain_name(raw: &str) -> std::result::Result<(), String> {
    if raw.is_empty() {
        return Err("it is empty".to_string());
    }
    let rooted = |kind: &str| {
        Err(format!(
            "it starts from {kind}; a name is resolved inside the directory it belongs to, \
             and `Path::join` throws the parent away when the value is rooted"
        ))
    };
    for component in std::path::Path::new(raw).components() {
        match component {
            std::path::Component::Prefix(_) => return rooted("a drive or share"),
            std::path::Component::RootDir => return rooted("a filesystem root"),
            _ => {}
        }
    }
    for segment in raw.split(['/', '\\']) {
        if segment.is_empty() {
            return Err(
                "it has an empty path segment; every segment must name something".to_string(),
            );
        }
        if segment == "." || segment == ".." {
            return Err(format!(
                "the segment '{segment}' is a directory reference, not a name"
            ));
        }
        // Windows reads `C:name` as drive-relative and `name:stream` as an NTFS
        // alternate data stream, and unix parses neither as a prefix — so the
        // shape is refused on every host, keeping a name written on one OS valid
        // on the others rather than only where it happened to be created.
        if segment.contains(':') {
            return Err(format!(
                "the segment '{segment}' contains ':', a drive or data-stream separator on Windows"
            ));
        }
    }
    Ok(())
}

/// Total bytes of every regular file under `path`, or `path`'s own length when
/// it is a file.
///
/// Symlinks are skipped rather than followed, matching [`copy_dir_recursive`]:
/// a link back into an ancestor would otherwise recurse forever, and a link out
/// of the tree would bill bytes the tree does not own.
pub fn dir_size(path: &std::path::Path) -> std::result::Result<u64, std::io::Error> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            continue;
        }
        total = total.saturating_add(dir_size(&entry.path())?);
    }
    Ok(total)
}

/// Recursively copy a directory from source to target.
///
/// Skips symlinks to prevent symlink-following attacks and infinite loops. Use
/// [`copy_dir_recursive_preserving_symlinks`] where the copy must be able to
/// put the tree back exactly as it was.
/// `fs::copy` already carries file modes across; each directory's mode is
/// applied too (Unix), so a `0700` tree does not land as a `0755` copy that
/// exposes what the original protected. Windows has no mode bits, so the copy
/// inherits the destination's inherited ACL there.
///
/// Skipping symlinks on the READ side and creating the destination itself make
/// this correct ONLY where cfgd owns the destination. Writing into live user
/// data — a restore overlay — needs a walker that stats each DESTINATION entry
/// unfollowed, or a symlink already standing there redirects the write.
pub fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    copy_dir_tree(src, dst, SymlinkPolicy::Skip)
}

/// The same copy, with every symlink RECREATED at its counterpart name instead
/// of skipped — its target string written back verbatim, relative or absolute,
/// and never followed.
///
/// For a copy that is a REVERSIBILITY guarantee rather than a snapshot: the
/// sidecar [`crate::reconciler::backup_file`] leaves beside a directory it
/// displaces, and the staging `backup restore` and `backup rollback` publish
/// from. A link the operator made inside a directory source is part of what
/// that copy promises to put back, and one it drops is one the overlay can
/// never replace — an overlay adds and replaces but never deletes, so the
/// source lands as neither generation. Recreating a link is not following one:
/// nothing is read or written through it, so the traversal stays inside the
/// source exactly as the skip did.
///
/// A snapshot keeps the skip on purpose — see [`copy_dir_recursive`].
pub fn copy_dir_recursive_preserving_symlinks(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::result::Result<(), std::io::Error> {
    copy_dir_tree(src, dst, SymlinkPolicy::Recreate)
}

/// What a tree copy does with a symlink it meets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymlinkPolicy {
    Skip,
    Recreate,
}

fn copy_dir_tree(
    src: &std::path::Path,
    dst: &std::path::Path,
    links: SymlinkPolicy,
) -> std::result::Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    // Resolved once, up front: a lexically disjoint `dst` can still land inside
    // `src` through a symlinked ancestor, and the walk would then find its own
    // output and descend into it forever.
    let dst_root = dst.canonicalize().unwrap_or_else(|_| dst.to_path_buf());
    copy_dir_into(src, dst, &dst_root, links)
}

fn copy_dir_into(
    src: &std::path::Path,
    dst: &std::path::Path,
    dst_root: &std::path::Path,
    links: SymlinkPolicy,
) -> std::result::Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_symlink() {
            if links == SymlinkPolicy::Recreate {
                recreate_link(&entry.path(), &dst_path)?;
            }
            continue;
        }
        if file_type.is_dir() {
            let entry_path = entry.path();
            if entry_path
                .canonicalize()
                .is_ok_and(|real| real.starts_with(dst_root))
            {
                continue;
            }
            copy_dir_into(&entry_path, &dst_path, dst_root, links)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    carry_dir_mode(src, dst);
    Ok(())
}

/// Apply `src`'s directory mode to `dst`, best-effort. No-op on Windows.
///
/// A filesystem with no mode bits (vfat, CIFS, some FUSE mounts) rejects the
/// `chmod`; losing the mode there is not a reason to fail a copy that otherwise
/// succeeded, since the target could not have honoured the mode anyway.
///
/// Apply it *after* populating `dst`: a restrictive source mode (`0500`,
/// `0300`) set on the way in blocks writing the very children being copied.
#[cfg(unix)]
pub fn carry_dir_mode(src: &std::path::Path, dst: &std::path::Path) {
    let applied =
        std::fs::metadata(src).and_then(|meta| std::fs::set_permissions(dst, meta.permissions()));
    if let Err(e) = applied {
        tracing::warn!(
            src = %src.posix(),
            dst = %dst.posix(),
            error = %e,
            "could not carry the source directory mode across; the copy keeps the default mode",
        );
    }
}

#[cfg(not(unix))]
pub fn carry_dir_mode(_src: &std::path::Path, _dst: &std::path::Path) {}

/// Always-fold POSIX form of a path. Use anywhere a path crosses into JSON,
/// YAML, SQLite, gateway API, OCI annotations, `file://` URLs, or snapshot
/// goldens. Backslash is treated as a separator; legitimate backslash-in-
/// filename on POSIX is sacrificed for cross-OS state portability (see the
/// path-handling consolidation spec for the fold-policy rationale).
pub fn to_posix_string(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

/// Lossless POSIX form for a persisted string that is ALSO reopened as a
/// filesystem path — `file_backups.file_path` and
/// `module_file_manifest.file_path`, whose rows a rollback feeds straight back
/// to `Path::new` to restore or delete a real file.
///
/// Folds on Windows only, where `\` cannot occur inside a filename and the
/// substitution is therefore reversible. On POSIX a backslash is an ordinary
/// filename character, so [`to_posix_string`]'s unconditional fold would turn
/// `/home/u/od\d.conf` into a key naming a different file — and a restore
/// keyed on it would write backup content over, or delete, that other file.
///
/// Prefer [`to_posix_string`] for every key that is only ever compared, never
/// reopened; the sacrifice it documents is safe only there.
pub fn to_posix_fs_key(path: impl AsRef<std::path::Path>) -> String {
    #[cfg(windows)]
    {
        to_posix_string(path)
    }
    #[cfg(not(windows))]
    {
        path.as_ref().to_string_lossy().into_owned()
    }
}

/// Strip a leading Windows extended-length (`\\?\`) verbatim prefix from a
/// path string, returning the rest unchanged.
///
/// `std::fs::canonicalize` on Windows returns verbatim paths
/// (`\\?\C:\Users\...`); the prefix is correct for the Win32 API but leaks an
/// implementation detail into anything a user reads or that another
/// non-verbatim path (e.g. one derived from `std::env::current_dir`) must
/// compare against. Fold it away wherever a canonicalized path becomes
/// user-visible or comparable. No-op on non-verbatim and on POSIX inputs.
pub fn strip_windows_verbatim(s: &str) -> &str {
    s.strip_prefix(r"\\?\").unwrap_or(s)
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

/// Replace the exact running cfgd version literal with a stable `<VERSION>`
/// placeholder so version-bearing snapshot goldens survive release bumps
/// without per-bump edits.
///
/// `cfgd_version` is the *binary crate's* `env!("CARGO_PKG_VERSION")`, passed
/// by the caller. cfgd-core's own crate version is NOT a valid substitute:
/// crates version independently (per-crate tags/bumps), so the two drift the
/// moment one releases without the other — see the same rule on
/// `config::modeline`.
///
/// Only the *exact* running version is substituted — a genuinely wrong
/// version (e.g. a stale `0.3.0` leaking into output) still fails to match
/// `<VERSION>` and surfaces as a snapshot mismatch, so real version bugs are
/// not masked. Fixture version strings (package versions, pinned tags, mocked
/// release numbers) are left untouched because they never equal the running
/// crate version.
pub fn normalize_cfgd_version<'a>(s: &'a str, cfgd_version: &str) -> std::borrow::Cow<'a, str> {
    if !cfgd_version.is_empty() && s.contains(cfgd_version) {
        std::borrow::Cow::Owned(s.replace(cfgd_version, "<VERSION>"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Replace every ` (N.Ns)` duration suffix with a stable ` (XXs)` placeholder,
/// so a golden that captures a run's elapsed time is host-stable.
///
/// The renderer formats durations as `{:.1}s`, so the span is always digits,
/// one dot, exactly one digit, `s`, `)`. Anything else in parentheses — a
/// package version, an exit code, a byte size — is left alone.
///
/// ` (<0.1s)` — the renderer's below-the-resolution floor — is the same slot
/// and normalizes to the same placeholder: whether a given action lands under
/// the floor is a property of the host, not of the render being pinned.
pub fn normalize_snapshot_durations(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        match duration_span(&chars[i..]) {
            Some((len, wall)) => {
                out.push_str(if wall { " (XXs wall)" } else { " (XXs)" });
                i += len;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Length in chars of a ` (N.Ns)` or ` (<0.1s)` suffix opening at `window`, if
/// there is one, and whether it is the wall-clock form (` (N.Ns wall)`) — a
/// golden keeps the word, so the vocabulary of a run's total stays pinned.
fn duration_span(window: &[char]) -> Option<(usize, bool)> {
    if window.len() < 7 || window[0] != ' ' || window[1] != '(' {
        return None;
    }
    // The floor spelling differs from a measurement only by its leading `<`,
    // and the digits behind it parse exactly as any other duration's do.
    let mut i = if window.get(2) == Some(&'<') { 3 } else { 2 };
    let digits_at = i;
    while window.get(i).is_some_and(char::is_ascii_digit) {
        i += 1;
    }
    if i == digits_at {
        return None;
    }
    if window.get(i) != Some(&'.') || !window.get(i + 1).is_some_and(char::is_ascii_digit) {
        return None;
    }
    if window.get(i + 2) != Some(&'s') {
        return None;
    }
    let wall = [' ', 'w', 'a', 'l', 'l']
        .iter()
        .enumerate()
        .all(|(k, c)| window.get(i + 3 + k) == Some(c));
    let close = if wall { i + 8 } else { i + 3 };
    if window.get(close) != Some(&')') {
        return None;
    }
    Some((close + 1, wall))
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
    // Each path in BOTH spellings a report can render it in: the absolute one,
    // and the `~/`-folded one every display slot goes through. A temp directory
    // lives under the home directory on Windows, so the fold rewrote the very
    // prefix the substitution was looking for and three goldens diverged there
    // alone.
    let mut subs: Vec<(String, &str)> = Vec::with_capacity(paths.len() * 2);
    for (path, label) in paths {
        let posix_path = to_posix_string(path);
        let folded = fold_home_in_text(&posix_path);
        if folded != posix_path {
            subs.push((folded, *label));
        }
        subs.push((posix_path, *label));
    }
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

#[cfg(test)]
mod tests;
