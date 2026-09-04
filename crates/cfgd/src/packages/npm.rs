//! npm-based package manager (global packages).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Role;
use cfgd_core::providers::{BootstrapPlan, PackageContext, PackageManager, PackageStateStore};

use super::shared::{
    MediatedArms, bootstrap_via_brew_then_system, brew_then_system_arms, detect_brew_system_method,
    pkg_run, planned_method_failed, planned_method_unavailable, report_abandoned_step,
    run_pkg_cmd_live, run_pkg_query, tool_cmd_with_resolver,
};

pub struct NpmManager;

/// npm's own bootstrap arm, reached when no brew/system mediator is present.
/// The ONE spelling: the planner resolves the method against it and the
/// cascade declines toward it, so the two must name the same string or a
/// planned `nvm` is a method nothing can run.
const NPM_FALLBACK_METHOD: &str = "nvm";

/// What a mediator installs to deliver npm. Read by `bootstrap` and by
/// `mediated_packages`, so a batched provision asks apt for exactly the names
/// the solo bootstrap does.
const NPM_MEDIATED: MediatedArms = brew_then_system_arms("node", &["nodejs", "npm"]);

/// Where a global npm operation should point, resolved once per operation so
/// install/uninstall/update/list all agree — see [`resolve_npm_prefix`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NpmPrefixDecision {
    /// `None` means "let npm resolve on its own" — either the environment
    /// already pins a prefix (`npm_config_prefix`/`NPM_CONFIG_PREFIX`) or
    /// cfgd is running elevated. `Some(dir)` is the prefix cfgd itself
    /// determined, used for `path_dirs()` and for the idempotency guarantee
    /// across every global operation.
    pub(super) prefix: Option<PathBuf>,
    /// True only when npm's own configured prefix failed the write-probe and
    /// `prefix` is the `$HOME/.npm-global` fallback. This is the ONLY case
    /// that gets a `--prefix` flag on argv and the one-time install() notice
    /// — a writable configured prefix needs no argv change at all.
    pub(super) is_fallback: bool,
}

impl NpmPrefixDecision {
    /// The prefix to apply on argv/notice — `Some` only when the resolver
    /// fell back, since a writable configured prefix needs no argv change
    /// and no notice at all. The single condition `apply_prefix_flag` and
    /// `install()`'s notice branch both key off, so the two can never fire
    /// out of step with each other.
    pub(super) fn fallback_prefix(&self) -> Option<&Path> {
        if self.is_fallback {
            self.prefix.as_deref()
        } else {
            None
        }
    }
}

/// The `manager` key this file's prefix decisions are persisted and read
/// back under in `package_manager_prefixes`. Every other built-in
/// `PackageManager` resolves its install location live on every call and has
/// no row in this table at all — npm is the only manager that persists a
/// prefix decision, because it is the only one whose global-install location
/// depends on a write-probe rather than being a fixed, well-known system path.
const NPM_PREFIX_STATE_MANAGER: &str = "npm";

/// Resolve the global-install prefix for the current process (real
/// `is_root()`), reusing a previously persisted decision when one exists.
///
/// The environment-pinned and elevated branches are checked BEFORE consulting
/// the persisted row, and unconditionally short-circuit it: an env-pinned
/// prefix or an elevated process is a live, unambiguous signal about where
/// npm's global installs must go on THIS invocation, so a stale row recorded
/// under different conditions (e.g. unelevated, no env pin) must never
/// override it — persistence exists purely to stabilize the *derived* branch
/// below, not to outrank a case that needs no derivation at all.
///
/// A derived prefix, once decided, must stay stable across runs: `install()`
/// may have already put packages under it, and re-deriving from live inputs
/// (a write-probe, npm's own project-local `.npmrc`) on a later run can
/// legitimately land on a different directory, making those packages
/// invisible to `installed_packages()`. A persisted hit also means npm is
/// never even spawned to ask for its configured prefix, so a query path
/// (`installed_packages`, `path_dirs`) that runs on every reconcile tick
/// doesn't pay that cost repeatedly either. See [`persisted_npm_prefix_decision`]
/// for the revalidation that keeps a persisted row from outliving its own
/// correctness.
///
/// `state` is the reconciler's already-open store, reached through the
/// [`cfgd_core::providers::PackageStateStore`] capability that
/// [`cfgd_core::providers::PackageContext`] carries, so a `--state-dir`-scoped run's
/// persisted decision honors that override instead of silently reading and
/// writing the default state location out from under it. See
/// [`resolve_npm_prefix_for`] for the underlying decision logic; split out so
/// tests can drive the elevated/unelevated branches directly without needing
/// real root privileges.
pub(super) fn resolve_npm_prefix(state: &dyn PackageStateStore) -> Result<NpmPrefixDecision> {
    if npm_env_prefix_set() || effective_elevated() {
        return Ok(NpmPrefixDecision {
            prefix: None,
            is_fallback: false,
        });
    }
    if let Some(decision) = persisted_npm_prefix_decision(state) {
        return Ok(decision);
    }
    let decision = resolve_npm_prefix_for(false)?;
    persist_npm_prefix_decision(state, &decision);
    Ok(decision)
}

/// The persisted prefix decision for npm, if one has been recorded AND still
/// holds. A recorded prefix that fails the write-probe (or whose modules/bin
/// directories no longer resolve to any existing ancestor at all) is treated
/// as a miss rather than trusted blindly: the conditions that made it correct
/// (a permission grant, a filesystem mount) can change between runs, and
/// reusing a now-wrong prefix would silently install packages somewhere the
/// user can no longer see or use them. A miss here falls through to a fresh
/// [`resolve_npm_prefix_for`] call in the caller, which re-persists.
///
/// Absent or unreadable state is treated the same way — "nothing usable is
/// persisted" rather than an error — persistence is a stability optimization,
/// not something a caller should fail over.
///
/// A FALLBACK decision is additionally probed the other way: revalidation of
/// the fallback path alone can only ever notice the fallback getting worse,
/// so a machine whose configured prefix was later fixed (permissions granted
/// on `/usr/local`, or a second node arriving on `PATH` under a prefix its
/// owner can write) would stay on `~/.npm-global` forever. When the row says
/// `is_fallback` AND the fallback holds no packages, npm is re-asked for its
/// configured prefix and that prefix is write-probed; a writable answer
/// discards the row so the caller re-derives — promoting to the configured
/// prefix and re-persisting with `is_fallback: false`. Asking npm is a spawn,
/// but it is paid only while the machine is in the degraded state, on
/// operations that are about to spawn npm anyway. A probe that errors or
/// answers unwritable keeps the fallback: "could not check" must not discard
/// a decision that still works.
///
/// The emptiness condition is what keeps promotion from undoing convergence.
/// Moving the prefix does not move the packages under it, so promoting away
/// from a fallback that already holds installs makes every one of them
/// invisible to `installed_packages()`: the next plan re-queues packages that
/// are installed, the apply re-runs the install and every post-script the
/// module owns, and the machine reports drift it can never settle. An EMPTY
/// fallback orphans nothing, so healing stays available exactly where it is
/// free.
fn persisted_npm_prefix_decision(state: &dyn PackageStateStore) -> Option<NpmPrefixDecision> {
    persisted_npm_prefix_decision_with(state, npm_prefix_is_writable, npm_configured_prefix)
}

/// [`persisted_npm_prefix_decision`] with the write-probe and the
/// configured-prefix query injected, so tests can drive the promotion and
/// keep-fallback branches deterministically — the same seam shape
/// [`resolve_npm_prefix_with`] carries for the same reason.
fn persisted_npm_prefix_decision_with(
    state: &dyn PackageStateStore,
    is_writable: impl Fn(&Path) -> bool,
    configured_prefix: impl Fn() -> Result<Option<PathBuf>>,
) -> Option<NpmPrefixDecision> {
    let (prefix, is_fallback) = match state.resolved_prefix(NPM_PREFIX_STATE_MANAGER) {
        Ok(Some(record)) => record,
        Ok(None) => return None,
        Err(e) => {
            // tracing-ok: a state read that degrades to re-resolving; no row states it
            tracing::warn!(error = %e, "cannot read persisted npm prefix");
            return None;
        }
    };
    let prefix = PathBuf::from(prefix);
    if !is_writable(&prefix) {
        tracing::debug!(
            prefix = %prefix.display(), // native-ok: log line, not a persisted key
            "persisted npm prefix is no longer writable; discarding cached decision"
        );
        return None;
    }
    if is_fallback
        && !npm_prefix_holds_packages(&prefix)
        && let Ok(Some(configured)) = configured_prefix()
        && is_writable(&configured)
    {
        tracing::debug!(
            prefix = %configured.display(), // native-ok: log line, not a persisted key
            "npm's configured prefix is writable again; discarding fallback decision"
        );
        return None;
    }
    Some(NpmPrefixDecision {
        prefix: Some(prefix),
        is_fallback,
    })
}

/// Persist a freshly resolved prefix decision so later calls reuse it
/// instead of re-deriving. A `None` prefix (env-pinned or elevated) is
/// deliberately not persisted — that decision is re-derived cheaply from
/// process state every time and needs no stability guarantee. Failure to
/// persist is logged and otherwise ignored: the caller already has a valid
/// decision for this run, and the next run simply re-resolves.
fn persist_npm_prefix_decision(state: &dyn PackageStateStore, decision: &NpmPrefixDecision) {
    let Some(prefix) = decision.prefix.as_ref() else {
        return;
    };
    if let Err(e) = state.record_resolved_prefix(
        NPM_PREFIX_STATE_MANAGER,
        &cfgd_core::to_posix_fs_key(prefix),
        decision.is_fallback,
    ) {
        // tracing-ok: the write half of the same cache
        tracing::warn!(error = %e, "cannot persist resolved npm prefix");
    }
}

/// Real elevation, with a `#[cfg(test)]`-only override so tests can force
/// the unelevated branch on a root test runner (or vice versa) without
/// touching real privileges. Compiles down to exactly `cfgd_core::is_root()`
/// in a release build — the override machinery does not exist outside `cfg(test)`.
fn effective_elevated() -> bool {
    #[cfg(test)]
    {
        if let Some(elevated) = test_elevated_override() {
            return elevated;
        }
    }
    cfgd_core::is_root()
}

/// "No override installed", the value the elevation seam holds outside a
/// guard's lifetime.
#[cfg(test)]
const TEST_ELEVATED_UNSET: i8 = -1;

/// Test seam for [`effective_elevated`] — process-global rather than
/// thread-local, because an apply dispatches its package work onto worker
/// threads: a per-thread override is invisible exactly where the install runs,
/// so a test driving the real reconciler could never reach the unelevated
/// branch and would silently assert about the elevated one instead. Every
/// consumer is `#[serial]`, which is what makes one shared cell safe.
#[cfg(test)]
static TEST_ELEVATED_OVERRIDE: std::sync::atomic::AtomicI8 =
    std::sync::atomic::AtomicI8::new(TEST_ELEVATED_UNSET);

#[cfg(test)]
fn test_elevated_override() -> Option<bool> {
    match TEST_ELEVATED_OVERRIDE.load(std::sync::atomic::Ordering::SeqCst) {
        TEST_ELEVATED_UNSET => None,
        value => Some(value != 0),
    }
}

/// RAII guard restoring the previous elevation override on drop (including on panic).
/// Modelled on `with_test_home`/`TestHomeGuard` in `cfgd-core/src/util/paths.rs`.
///
/// `unix` as well as `test`: the only callers are the `npm_shim` tests, which
/// drive npm through a `/bin/sh` shim and are themselves `#[cfg(unix)]`. Gating
/// on `test` alone leaves this trio uncallable — and so dead under the `-D
/// warnings` the Windows test job builds with.
#[cfg(all(test, unix))]
#[must_use = "dropping the guard immediately restores the previous override"]
struct TestElevatedGuard {
    prev: i8,
}

#[cfg(all(test, unix))]
impl Drop for TestElevatedGuard {
    fn drop(&mut self) {
        TEST_ELEVATED_OVERRIDE.store(self.prev, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(all(test, unix))]
fn with_test_elevated_guard(elevated: bool) -> TestElevatedGuard {
    let prev = TEST_ELEVATED_OVERRIDE.swap(i8::from(elevated), std::sync::atomic::Ordering::SeqCst);
    TestElevatedGuard { prev }
}

#[cfg(all(test, unix))]
fn with_test_elevated<F, R>(elevated: bool, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = with_test_elevated_guard(elevated);
    f()
}

/// [`resolve_npm_prefix_with`] wired to the real write-probe
/// ([`npm_prefix_is_writable`]) — the production entry point for a given
/// elevation state. Split out so tests can drive `elevated` directly without
/// needing real root privileges.
pub(super) fn resolve_npm_prefix_for(elevated: bool) -> Result<NpmPrefixDecision> {
    resolve_npm_prefix_with(elevated, npm_prefix_is_writable)
}

/// Decide whether cfgd should point npm at a prefix of its own choosing.
///
/// 1. An environment-set prefix (`npm_config_prefix`/`NPM_CONFIG_PREFIX`)
///    means npm is already being pointed somewhere deliberately — never
///    override it.
/// 2. An elevated process can write npm's system prefix; overriding it would
///    install into cfgd's (root's) home instead, so leave it alone too.
/// 3. Otherwise, ask npm for its configured prefix and consult `is_writable`
///    for it. A writable answer is used as-is (no argv change — the working
///    case stays untouched). An unwritable or undeterminable answer falls
///    back to `$HOME/.npm-global`, created if absent.
///
/// `is_writable` is injected rather than this function calling
/// [`npm_prefix_is_writable`] itself, so a caller can force the fallback
/// branch deterministically: a root process bypasses Unix DAC checks
/// entirely, so the real probe can never observe an unwritable directory
/// there.
pub(super) fn resolve_npm_prefix_with(
    elevated: bool,
    is_writable: impl Fn(&Path) -> bool,
) -> Result<NpmPrefixDecision> {
    if npm_env_prefix_set() || elevated {
        return Ok(NpmPrefixDecision {
            prefix: None,
            is_fallback: false,
        });
    }

    match npm_configured_prefix()? {
        Some(prefix) if is_writable(&prefix) => Ok(NpmPrefixDecision {
            prefix: Some(prefix),
            is_fallback: false,
        }),
        _ => Ok(NpmPrefixDecision {
            prefix: Some(npm_fallback_prefix()),
            is_fallback: true,
        }),
    }
}

/// True when the user (or a parent process) already pinned npm's global
/// prefix via the environment — cfgd must not second-guess that choice.
pub(super) fn npm_env_prefix_set() -> bool {
    std::env::var_os("npm_config_prefix").is_some()
        || std::env::var_os("NPM_CONFIG_PREFIX").is_some()
}

/// Ask npm for its configured global prefix (`npm config get prefix`).
/// Returns `Ok(None)` when npm exits non-zero or answers with empty output —
/// both mean "couldn't determine a prefix", handled the same as a failed
/// write-probe by the caller. A hard spawn failure (ENOENT) propagates as
/// `PackageError::CommandFailed`, matching every other npm call in this file,
/// so it is never silently absorbed into the fallback path.
pub(super) fn npm_configured_prefix() -> Result<Option<PathBuf>> {
    let output = run_pkg_query("npm", npm_cmd().args(["config", "get", "prefix"]))?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = cfgd_core::stdout_lossy_trimmed(&output);
    if stdout.is_empty() || stdout == "undefined" {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(stdout)))
    }
}

/// `<prefix>/lib/node_modules` on Unix, `<prefix>/node_modules` on Windows —
/// the directory npm actually writes global packages into.
#[cfg(windows)]
pub(super) fn npm_global_modules_dir(prefix: &Path) -> PathBuf {
    prefix.join("node_modules")
}

#[cfg(not(windows))]
pub(super) fn npm_global_modules_dir(prefix: &Path) -> PathBuf {
    prefix.join("lib").join("node_modules")
}

/// `<prefix>/bin` on Unix, `<prefix>` itself on Windows — where npm puts the
/// shims/symlinks for globally installed executables.
#[cfg(windows)]
pub(super) fn npm_bin_dir(prefix: &Path) -> PathBuf {
    prefix.to_path_buf()
}

#[cfg(not(windows))]
pub(super) fn npm_bin_dir(prefix: &Path) -> PathBuf {
    prefix.join("bin")
}

/// Walk up from `path` until an existing ancestor is found. `npm`'s global
/// modules directory is usually not created yet (npm creates it lazily on
/// first install), so the write-probe target is whichever existing directory
/// is closest to it.
pub(super) fn deepest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.exists() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

/// Write-probe a single directory via the shared real-access probe, applied
/// to the deepest existing ancestor of `target` (npm's global modules
/// directory is usually not created yet — npm creates it lazily on first
/// install — so the write-probe target is whichever existing directory is
/// closest to it).
fn probe_npm_dir_writable(target: &Path) -> bool {
    deepest_existing_ancestor(target).is_some_and(|a| {
        matches!(
            cfgd_core::probe_dir_writable(&a),
            cfgd_core::DirWritable::Writable
        )
    })
}

/// Write-probe `prefix`: both the modules directory npm installs packages
/// into AND the bin directory it links executables into are probed. This
/// only actually catches a split-writability layout when both directories
/// (or distinct existing ancestors of each) already exist as separate mount
/// points or ACL boundaries — the common case where NEITHER directory has
/// been created yet has `deepest_existing_ancestor` walk both probes up to
/// the same existing parent, so the two calls answer identically and this
/// degenerates to a single effective probe. Still correct to keep both: it
/// costs nothing when they coincide, and catches the split case when it
/// doesn't.
pub(super) fn npm_prefix_is_writable(prefix: &Path) -> bool {
    probe_npm_dir_writable(&npm_global_modules_dir(prefix))
        && probe_npm_dir_writable(&npm_bin_dir(prefix))
}

/// Whether any globally installed package lives under `prefix` — the question
/// that decides whether abandoning it is free. Dot-prefixed entries
/// (`.package-lock.json`, `.bin`) are npm's own bookkeeping, not packages, and
/// a modules directory that cannot be read at all answers `false`: nothing is
/// known to be there.
pub(super) fn npm_prefix_holds_packages(prefix: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(npm_global_modules_dir(prefix)) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
}

/// The fallback prefix used when npm's own configured prefix isn't
/// user-writable: `$HOME/.npm-global`. Resolution only — does not create the
/// directory, so a read path (`installed_packages`, `path_dirs`) can name
/// this location without conjuring it into existence; see
/// [`ensure_npm_fallback_prefix`] for the install-only creation step.
pub(super) fn npm_fallback_prefix() -> PathBuf {
    cfgd_core::expand_tilde(Path::new("~/.npm-global"))
}

/// Create the fallback prefix directory. Called only from
/// [`PackageManager::install`] — the one npm operation that actually needs
/// the directory to exist before npm can write into it. Every other caller
/// of [`npm_fallback_prefix`] only names the location, never creates it.
fn ensure_npm_fallback_prefix(prefix: &Path) -> Result<()> {
    std::fs::create_dir_all(prefix)?;
    Ok(())
}

/// Find npm binary, checking PATH and common nvm install locations.
///
/// Not usable with the generic `resolve_tool_with_fallbacks` helper because the
/// nvm path is a wildcard `~/.nvm/versions/node/*/bin/npm` that requires a
/// directory scan rather than a fixed fallback list.
///
/// Honors the `CFGD_NPM_BIN` env-var seam for tests — when set and pointing
/// at a real file, short-circuits the PATH + nvm scan.
pub(super) fn find_npm() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("CFGD_NPM_BIN") {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }
    // The FULL resolved path, never the bare name: `command_path` searches the
    // bootstrapped-directory registry after `$PATH`, and a `Command::new("npm")`
    // spawn searches only `$PATH` — so an npm cfgd just provisioned through brew
    // would answer "available" and then die with ENOENT.
    if let Some(path) = cfgd_core::command_path("npm") {
        return Some(path);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    find_npm_in_nvm(&home)
}

/// Scan `<home>/.nvm/versions/node/*/bin/npm` and return the first match.
/// Split out so tests can drive the directory scan against a tempdir without
/// mutating `$HOME`.
pub(super) fn find_npm_in_nvm(home: &std::path::Path) -> Option<PathBuf> {
    let nvm_dir = home.join(".nvm/versions/node");
    let entries = std::fs::read_dir(&nvm_dir).ok()?;
    for entry in entries.flatten() {
        let npm_path = entry.path().join("bin/npm");
        if npm_path.exists() {
            return Some(npm_path);
        }
    }
    None
}

pub(super) fn npm_available() -> bool {
    find_npm().is_some()
}

pub(super) fn npm_cmd() -> Command {
    tool_cmd_with_resolver("npm", find_npm)
}

/// Append `--prefix <dir>` to `cmd` only when the resolver chose the
/// fallback prefix — a writable configured prefix needs no argv change at
/// all, keeping the working case identical to before this resolver existed.
pub(super) fn apply_prefix_flag(cmd: &mut Command, decision: &NpmPrefixDecision) {
    if let Some(prefix) = decision.fallback_prefix() {
        cmd.arg("--prefix").arg(prefix);
    }
}

/// Test-only mirror of [`PackageManager::path_dirs`] for npm, with `elevated`
/// injected for the same reason as [`resolve_npm_prefix_for`]:
/// `resolve_npm_prefix()` short-circuits under `is_root()` regardless of the
/// configured prefix's own writability, so a root-running test needs a way
/// to bypass that branch to exercise the mapping below it. Production
/// `path_dirs` deliberately routes through the cached `resolve_npm_prefix`
/// instead (see its doc comment), so this has no production caller.
///
/// `unix` as well as `test`: every caller lives in `mod npm_shim`, which
/// drives npm through a `/bin/sh` shim and is itself `#[cfg(unix)]`. Gating
/// on `test` alone leaves this uncallable on the Windows test job, which
/// builds with `-D warnings` and turns that into a dead-code error.
#[cfg(all(test, unix))]
pub(super) fn npm_path_dirs_for(elevated: bool) -> Vec<String> {
    match resolve_npm_prefix_for(elevated) {
        Ok(NpmPrefixDecision {
            prefix: Some(prefix),
            ..
        }) => vec![cfgd_core::to_posix_string(npm_bin_dir(&prefix))],
        Ok(NpmPrefixDecision { prefix: None, .. }) => Vec::new(),
        Err(e) => {
            // tracing-ok: the PATH contribution degrades to empty; the writable-prefix caveat is a different fact and is reported
            tracing::warn!(error = %e, "cannot resolve npm's global prefix for PATH");
            Vec::new()
        }
    }
}

/// The prefix decision behind both PATH-reporting methods, or `None` after
/// warning. The ONE degrade shared by them, so a resolver failure reads the
/// same whichever asked and neither reports a directory it could not resolve.
fn npm_prefix_for_path(state: &dyn PackageStateStore) -> Option<NpmPrefixDecision> {
    match resolve_npm_prefix(state) {
        Ok(decision) => Some(decision),
        Err(e) => {
            // tracing-ok: the PATH contribution degrades to empty; the writable-prefix caveat is a different fact and is reported
            tracing::warn!(error = %e, "cannot resolve npm's global prefix for PATH");
            None
        }
    }
}

impl PackageManager for NpmManager {
    fn name(&self) -> &str {
        "npm"
    }

    fn upgrade_verb(&self) -> Option<&'static str> {
        // `npm install -g <pkg>` installs latest, replacing an already-held
        // package — install itself is the raise, as for `go`.
        Some("install")
    }

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(npm_cmd().arg("--version"))
    }

    fn is_available(&self) -> bool {
        npm_available()
    }

    fn path_dirs(&self, cx: &PackageContext<'_>) -> Vec<String> {
        // Routed through the cached `resolve_npm_prefix`, not
        // `npm_path_dirs_for`/`resolve_npm_prefix_for` directly, so a
        // reconcile tick's PATH computation agrees with whatever prefix
        // `install()`/`installed_packages()` already persisted instead of
        // re-deriving (and potentially disagreeing, if live inputs shifted).
        npm_prefix_for_path(cx.state)
            .and_then(|decision| decision.prefix)
            .map(|prefix| vec![cfgd_core::to_posix_string(npm_bin_dir(&prefix))])
            .unwrap_or_default()
    }

    /// The bin directory of the `$HOME/.npm-global` fallback, which
    /// [`PackageManager::install`] creates — and only then. A writable
    /// configured prefix is npm's own or the system's, so nothing there is
    /// cfgd's to publish and this answers empty.
    fn created_path_dirs(&self, cx: &PackageContext<'_>) -> Vec<String> {
        let Some(decision) = npm_prefix_for_path(cx.state) else {
            return Vec::new();
        };
        decision
            .fallback_prefix()
            .map(|prefix| vec![cfgd_core::to_posix_string(npm_bin_dir(prefix))])
            .unwrap_or_default()
    }

    fn bootstrap_plan_given(&self, delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        // No declared PATH directory: npm's global bin lives under a prefix that
        // is only resolvable once node exists, which is what `path_dirs` reads
        // out of state after the install.
        match detect_brew_system_method(NPM_FALLBACK_METHOD, delivered) {
            NPM_FALLBACK_METHOD => {
                Some(BootstrapPlan::new(NPM_FALLBACK_METHOD).requiring(["curl"]))
            }
            method => Some(BootstrapPlan::new(method)),
        }
    }

    fn bootstrap(&self, cx: &PackageContext<'_>) -> Result<()> {
        // Returns false without probing anything when the plan named `nvm` —
        // npm's own fallback arm, which is the next thing below.
        if bootstrap_via_brew_then_system(
            cx,
            "npm",
            NPM_MEDIATED.brew.unwrap_or("node"),
            NPM_MEDIATED.system,
            NPM_FALLBACK_METHOD,
        )? {
            return Ok(());
        }

        // Fall back to nvm
        if command_available("curl") {
            let result = pkg_run(
                cx,
                Command::new("bash").arg("-c").arg(concat!(
                    "curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash && ",
                    "export NVM_DIR=\"$HOME/.nvm\" && [ -s \"$NVM_DIR/nvm.sh\" ] && . \"$NVM_DIR/nvm.sh\" && ",
                    "nvm install --lts"
                )),
                "Installing Node.js via nvm",
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "npm".into(),
                message: format!("nvm install failed: {}", e),
            })?;
            if result.status.success() {
                return Ok(());
            }
            if let Some(method) = cx.planned_method() {
                return Err(planned_method_failed("npm", method, &result).into());
            }
            report_abandoned_step(cx, "npm", "nvm", &result);
        } else if let Some(method) = cx.planned_method() {
            // `curl` is nvm's declared prerequisite, so the plan promised it.
            return Err(planned_method_unavailable("npm", method).into());
        }

        Err(PackageError::BootstrapFailed {
            manager: "npm".into(),
            message: "no method available to install npm".into(),
        }
        .into())
    }

    fn mediated_packages(&self, via: &str) -> Option<Vec<String>> {
        NPM_MEDIATED.packages_for(via)
    }

    fn installed_packages(&self, cx: &PackageContext<'_>) -> Result<HashSet<String>> {
        let decision = resolve_npm_prefix(cx.state)?;
        let mut cmd = npm_cmd();
        cmd.args(["list", "-g", "--depth=0", "--json"]);
        apply_prefix_flag(&mut cmd, &decision);
        let output = run_pkg_query("npm", &mut cmd)?;
        // npm list exits non-zero if there are peer dep issues, but still produces valid JSON
        parse_npm_list_packages(&String::from_utf8_lossy(&output.stdout))
    }

    fn install(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let decision = resolve_npm_prefix(cx.state)?;
        if let Some(prefix) = decision.fallback_prefix() {
            ensure_npm_fallback_prefix(prefix)?;
            // Where the packages went and why, and nothing about PATH: the bin
            // directory under this prefix is reported through
            // `created_path_dirs` and written into the generated env file, so
            // an instruction to add it by hand would be stale the moment the
            // env layer runs. `Warn`, not `Info`: the configured prefix was
            // refused and the install went somewhere else, which is a
            // degraded fallback whatever the packages did afterwards.
            // The tag already says npm; a body opening on it would print the
            // same fact twice (`[npm] npm has no writable …`).
            cx.report(
                Role::Warn,
                "npm",
                format!(
                    "no writable global prefix; installing into {}",
                    prefix.display(), // native-ok: human-facing terminal notice, not a persisted key
                ),
            );
        }
        let label = format!("npm install -g {}", packages.join(" "));
        let mut cmd = npm_cmd();
        cmd.arg("install").arg("-g").args(packages);
        apply_prefix_flag(&mut cmd, &decision);
        run_pkg_cmd_live(cx, "npm", &mut cmd, &label, "install")?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let decision = resolve_npm_prefix(cx.state)?;
        let label = format!("npm uninstall -g {}", packages.join(" "));
        let mut cmd = npm_cmd();
        cmd.arg("uninstall").arg("-g").args(packages);
        apply_prefix_flag(&mut cmd, &decision);
        run_pkg_cmd_live(cx, "npm", &mut cmd, &label, "uninstall")?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // npm view <pkg> version
        let output = run_pkg_query("npm", npm_cmd().args(["view", package, "version"]))?;
        if !output.status.success() {
            return Ok(None);
        }
        let version = cfgd_core::stdout_lossy_trimmed(&output);
        if version.is_empty() {
            Ok(None)
        } else {
            Ok(Some(version))
        }
    }

    fn installed_packages_with_versions(
        &self,
        cx: &PackageContext<'_>,
    ) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let decision = resolve_npm_prefix(cx.state)?;
        let mut cmd = npm_cmd();
        cmd.args(["list", "-g", "--depth=0", "--json"]);
        apply_prefix_flag(&mut cmd, &decision);
        let output = run_pkg_query("npm", &mut cmd)?;
        // npm list exits non-zero on peer dep issues but still produces valid JSON
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "npm".into(),
                message: format!("failed to parse npm list output: {}", e),
            })?;
        Ok(parse_npm_list_versions(&parsed))
    }
}

/// Parse `npm list -g --depth=0 --json` dependencies object into a name-only
/// `HashSet`. Shared between `installed_packages` and tests; the JSON-string
/// boundary is the natural contract since `npm list` exits non-zero on peer
/// dep issues but still produces valid JSON, which this parses.
pub(super) fn parse_npm_list_packages(stdout: &str) -> Result<HashSet<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: "npm".into(),
            message: format!("failed to parse npm list output: {}", e),
        })?;
    let mut packages = HashSet::new();
    if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_object()) {
        for key in deps.keys() {
            packages.insert(key.clone());
        }
    }
    Ok(packages)
}

/// Parse `npm list -g --depth=0 --json` dependencies object into PackageInfo.
/// JSON format: `{"dependencies": {"pkg": {"version": "1.2.3"}, ...}}`
pub(super) fn parse_npm_list_versions(
    parsed: &serde_json::Value,
) -> Vec<cfgd_core::providers::PackageInfo> {
    let mut packages = Vec::new();
    if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_object()) {
        for (name, info) in deps {
            let version = info
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or(cfgd_core::providers::UNKNOWN_PACKAGE_VERSION)
                .to_string();
            packages.push(cfgd_core::providers::PackageInfo {
                name: name.clone(),
                version,
            });
        }
    }
    packages
}

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;
    use cfgd_core::providers::PackageManagerExt;

    use super::super::shared::brew_available;
    use super::*;

    #[test]
    fn test_parse_npm_list_versions_basic() {
        let json = serde_json::json!({
            "dependencies": {
                "typescript": {"version": "5.3.3"},
                "eslint": {"version": "8.56.0"},
                "prettier": {"version": "3.2.0"}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "typescript" && p.version == "5.3.3")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "eslint" && p.version == "8.56.0")
        );
    }

    #[test]
    fn test_parse_npm_list_versions_no_deps() {
        let json = serde_json::json!({"name": "root"});
        let pkgs = parse_npm_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_parse_npm_list_versions_missing_version() {
        let json = serde_json::json!({
            "dependencies": {
                "some-pkg": {}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_npm_list_versions_nested_deps_ignored() {
        // Only top-level dependencies are parsed
        let json = serde_json::json!({
            "dependencies": {
                "typescript": {
                    "version": "5.3.3",
                    "dependencies": {
                        "nested-pkg": {"version": "1.0.0"}
                    }
                }
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "typescript");
    }

    #[test]
    fn npm_manager_name() {
        let mgr = NpmManager;
        assert_eq!(mgr.name(), "npm");
    }

    #[test]
    fn parse_npm_list_versions_empty_deps() {
        let json = serde_json::json!({"dependencies": {}});
        let pkgs = parse_npm_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_npm_list_versions_non_string_version() {
        let json = serde_json::json!({
            "dependencies": {
                "pkg": {"version": 123}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        // version is not a string, so it falls back to "unknown"
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_npm_list_versions_real_world_output() {
        let json = serde_json::json!({
            "version": "10.2.4",
            "name": "lib",
            "dependencies": {
                "corepack": {"version": "0.24.0"},
                "npm": {"version": "10.2.4"},
                "typescript": {"version": "5.3.3"},
                "eslint": {"version": "8.56.0"},
                "prettier": {"version": "3.2.0"}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 5);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "corepack" && p.version == "0.24.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "npm" && p.version == "10.2.4")
        );
    }

    #[test]
    fn parse_npm_list_versions_with_extra_fields() {
        let json = serde_json::json!({
            "version": "1.0.0",
            "dependencies": {
                "express": {
                    "version": "4.18.2",
                    "resolved": "https://registry.npmjs.org/express/-/express-4.18.2.tgz",
                    "overridden": false
                }
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "express");
        assert_eq!(pkgs[0].version, "4.18.2");
    }

    #[test]
    fn npm_bootstrap_plan_follows_the_brew_system_nvm_cascade() {
        // The cascade always has an arm — brew, a system manager, or nvm — so
        // the plan itself is unconditional; whether the nvm arm's `curl` can be
        // had is `feasible_bootstrap_plan`'s question.
        let plan = NpmManager.bootstrap_plan();
        assert!(plan.is_some());
        if let Some(plan) = plan {
            // The method names whichever arm of `bootstrap`'s cascade this host
            // reaches; only the nvm fallback shells out to a tool of its own,
            // and no arm creates a PATH dir the manager can name before node
            // exists (`path_dirs` reads the resolved prefix out of state).
            let can = |t: &str| command_available(t);
            let expected_method = if brew_available() {
                "brew"
            } else if can("apt") {
                "apt"
            } else if can("dnf") {
                "dnf"
            } else {
                "nvm"
            };
            assert_eq!(plan.method, expected_method);
            assert_eq!(
                plan.requires,
                if expected_method == "nvm" {
                    vec!["curl".to_string()]
                } else {
                    Vec::<String>::new()
                }
            );
            assert!(plan.creates_path_dirs.is_empty());
        }
    }

    #[test]
    #[serial_test::serial]
    fn npm_manager_is_available_checks_npm() {
        let mgr = NpmManager;
        let available = mgr.is_available();
        assert_eq!(available, npm_available());
    }

    // --- parse_npm_list_packages ---

    #[test]
    fn parse_npm_list_packages_returns_top_level_dep_names() {
        let stdout = r#"{"dependencies": {"typescript":{"version":"5.3.3"}, "eslint":{"version":"8.56.0"}}}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.contains("typescript"));
        assert!(pkgs.contains("eslint"));
    }

    #[test]
    fn parse_npm_list_packages_no_deps_field_yields_empty() {
        let stdout = r#"{"name":"root","version":"1.0.0"}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_npm_list_packages_empty_deps_object_yields_empty() {
        let stdout = r#"{"dependencies":{}}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_npm_list_packages_ignores_nested_deps() {
        // Only top-level keys are returned; nested dependency trees stay nested.
        let stdout = r#"{"dependencies":{"typescript":{"version":"5.3.3","dependencies":{"nested-pkg":{"version":"1.0.0"}}}}}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.contains("typescript"));
        assert!(
            !pkgs.contains("nested-pkg"),
            "nested deps must not leak into the top-level set"
        );
    }

    #[test]
    fn parse_npm_list_packages_errors_on_invalid_json() {
        let err = parse_npm_list_packages("not-json").expect_err("invalid JSON must error");
        let msg = err.to_string();
        assert!(
            msg.contains("npm") && msg.contains("failed to parse npm list output"),
            "error must include 'npm' and parse-failure context, got: {msg}"
        );
    }

    #[test]
    fn parse_npm_list_packages_dependencies_not_object_yields_empty() {
        // npm sometimes emits "dependencies": [] when there are issues —
        // treat that as no deps rather than panicking.
        let stdout = r#"{"dependencies":[]}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    // --- find_npm_in_nvm ---

    #[test]
    fn find_npm_in_nvm_returns_none_when_no_nvm_dir() {
        let home = tempfile::tempdir().unwrap();
        // No .nvm directory exists in the tempdir at all.
        assert!(find_npm_in_nvm(home.path()).is_none());
    }

    #[test]
    fn find_npm_in_nvm_returns_none_when_nvm_dir_empty() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".nvm/versions/node")).unwrap();
        // Directory exists but contains no node version subdirs.
        assert!(find_npm_in_nvm(home.path()).is_none());
    }

    #[test]
    fn find_npm_in_nvm_returns_first_npm_binary_found() {
        let home = tempfile::tempdir().unwrap();
        let v20 = home.path().join(".nvm/versions/node/v20.10.0/bin");
        std::fs::create_dir_all(&v20).unwrap();
        let npm = v20.join("npm");
        std::fs::write(&npm, b"#!/bin/sh\n").unwrap();

        let found = find_npm_in_nvm(home.path()).expect("npm should be found in nvm version dir");
        assert_eq!(
            found, npm,
            "must return the absolute path to the located npm binary"
        );
    }

    #[test]
    fn find_npm_in_nvm_skips_versions_without_bin_npm() {
        let home = tempfile::tempdir().unwrap();
        // v18 has no bin/npm; v20 does. Result must be from v20.
        std::fs::create_dir_all(home.path().join(".nvm/versions/node/v18.0.0")).unwrap();
        let v20bin = home.path().join(".nvm/versions/node/v20.10.0/bin");
        std::fs::create_dir_all(&v20bin).unwrap();
        std::fs::write(v20bin.join("npm"), b"").unwrap();

        let found = find_npm_in_nvm(home.path()).expect("v20 npm should be located");
        assert!(
            found.to_string_lossy().contains("v20.10.0"),
            "must skip the version that lacks bin/npm, got: {}",
            found.display()
        );
    }

    // --- find_npm CFGD_NPM_BIN seam ---

    #[test]
    #[serial_test::serial]
    fn find_npm_honors_cfgd_npm_bin_when_pointing_at_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("npm");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();
        let _g = cfgd_core::test_helpers::EnvVarGuard::set(
            "CFGD_NPM_BIN",
            fake.to_str().expect("utf8 tempdir path"),
        );
        let found = find_npm().expect("a real CFGD_NPM_BIN file must short-circuit detection");
        assert_eq!(
            found, fake,
            "find_npm must return the exact CFGD_NPM_BIN path when it is a file"
        );
    }

    #[test]
    #[serial_test::serial]
    fn find_npm_ignores_cfgd_npm_bin_when_path_is_not_a_file() {
        // A dangling CFGD_NPM_BIN must NOT be returned — find_npm falls through
        // to PATH / nvm detection instead of handing back a path that ENOENTs.
        let _g = cfgd_core::test_helpers::EnvVarGuard::set(
            "CFGD_NPM_BIN",
            "/nonexistent/cfgd-npm-bin-not-a-file",
        );
        let found = find_npm();
        assert_ne!(
            found.as_deref(),
            Some(std::path::Path::new("/nonexistent/cfgd-npm-bin-not-a-file")),
            "a non-file CFGD_NPM_BIN must be ignored, not returned verbatim"
        );
    }

    /// The bug this pins: `command_available` searches `$PATH` AND the
    /// bootstrapped-directory registry, while a `Command::new("npm")` spawn
    /// searches only `$PATH` — so an npm that brew had just landed answered
    /// "available" and then died with ENOENT. Only a full path can be spawned.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn find_npm_returns_a_full_path_for_an_npm_only_the_bootstrapped_registry_can_see() {
        use std::os::unix::fs::PermissionsExt;
        let _no_seam = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_NPM_BIN");
        let bootstrapped = tempfile::tempdir().unwrap();
        let npm = bootstrapped.path().join("npm");
        std::fs::write(&npm, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();

        // A PATH that cannot see it, so the registry is the ONLY way npm
        // resolves — which is the state right after a brew-mediated provision.
        let empty = tempfile::tempdir().unwrap();
        let _path_excl = cfgd_core::test_helpers::path_env_mutation_guard();
        let _path_env = cfgd_core::test_helpers::EnvVarGuard::set(
            "PATH",
            empty.path().to_str().expect("utf8 tempdir path"),
        );
        let _registry = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        cfgd_core::register_bootstrapped_path_dirs(&[cfgd_core::to_posix_string(
            bootstrapped.path(),
        )]);

        assert_eq!(
            find_npm().as_deref(),
            Some(npm.as_path()),
            "find_npm must hand back the resolved path, never the bare name"
        );
    }

    // ---------------------------------------------------------------------
    // Fallback-promotion revalidation, driven through the injectable seam so
    // every branch is deterministic on any platform: no real npm spawn, no
    // real permission bits.
    // ---------------------------------------------------------------------

    /// A persisted FALLBACK row whose configured prefix has become writable
    /// again is discarded, so the caller re-derives and promotes off the
    /// degraded `~/.npm-global` path automatically.
    #[test]
    fn a_fallback_row_is_discarded_when_the_configured_prefix_is_writable_again() {
        let state = cfgd_core::test_helpers::test_state();
        state
            .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
            .expect("seed a fallback decision");

        let configured = PathBuf::from("/usr/local");
        let decision =
            persisted_npm_prefix_decision_with(&state, |_| true, || Ok(Some(configured.clone())));

        assert_eq!(
            decision, None,
            "a writable configured prefix must discard the fallback row so \
             re-derivation promotes onto it"
        );
    }

    /// The configured prefix still failing its write-probe keeps the fallback:
    /// the machine is still in the degraded state the row records.
    #[test]
    fn a_fallback_row_survives_while_the_configured_prefix_stays_unwritable() {
        let state = cfgd_core::test_helpers::test_state();
        state
            .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
            .expect("seed a fallback decision");

        let fallback = PathBuf::from("/home/u/.npm-global");
        let decision = persisted_npm_prefix_decision_with(
            &state,
            |p| p == fallback, // the fallback probes writable; everything else does not
            || Ok(Some(PathBuf::from("/usr/local"))),
        );

        assert_eq!(
            decision,
            Some(NpmPrefixDecision {
                prefix: Some(fallback),
                is_fallback: true,
            }),
            "an unwritable configured prefix must keep the persisted fallback"
        );
    }

    /// "Could not check" is not "check failed": a configured-prefix query that
    /// errors, or that answers nothing at all, keeps the fallback rather than
    /// discarding a decision that still works.
    #[test]
    fn a_fallback_row_survives_when_the_configured_prefix_cannot_be_asked() {
        let state = cfgd_core::test_helpers::test_state();
        state
            .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
            .expect("seed a fallback decision");

        let expected = Some(NpmPrefixDecision {
            prefix: Some(PathBuf::from("/home/u/.npm-global")),
            is_fallback: true,
        });

        let on_error = persisted_npm_prefix_decision_with(
            &state,
            |_| true,
            || {
                Err(PackageError::CommandFailed {
                    manager: "npm".into(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                }
                .into())
            },
        );
        assert_eq!(
            on_error, expected,
            "a probe error must keep the fallback, never discard it"
        );

        let on_empty = persisted_npm_prefix_decision_with(&state, |_| true, || Ok(None));
        assert_eq!(
            on_empty, expected,
            "npm answering no prefix at all must keep the fallback"
        );
    }

    /// The promotion probe is the spawn the doc comment prices: it is paid
    /// only while the machine is in the degraded state. A non-fallback row
    /// never asks npm anything.
    #[test]
    fn a_non_fallback_row_never_pays_the_configured_prefix_spawn() {
        let state = cfgd_core::test_helpers::test_state();
        state
            .record_package_manager_prefix("npm", "/usr/local", false)
            .expect("seed a non-fallback decision");

        let asked = std::cell::Cell::new(0u32);
        let decision = persisted_npm_prefix_decision_with(
            &state,
            |_| true,
            || {
                asked.set(asked.get() + 1);
                Ok(Some(PathBuf::from("/usr/local")))
            },
        );

        assert_eq!(
            decision,
            Some(NpmPrefixDecision {
                prefix: Some(PathBuf::from("/usr/local")),
                is_fallback: false,
            }),
        );
        assert_eq!(
            asked.get(),
            0,
            "a healthy (non-fallback) row must not spawn npm to re-ask its \
             configured prefix"
        );
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_NPM_BIN ToolShim.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod npm_shim {
        use super::*;
        use cfgd_core::test_helpers::{EnvVarGuard, ToolShim, test_printer};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_NPM_BIN";

        /// Argv-branching CFGD_NPM_BIN shim: answers `npm config get prefix`
        /// with a caller-chosen directory, and every other invocation with a
        /// canned exit/stdout/stderr. `ToolShim`'s single fixed response is
        /// unusable here because the prefix resolver and the actual npm
        /// operation share this seam but must answer differently.
        struct NpmShim {
            // Declaration order is drop order: `_env_guard` must be restored
            // BEFORE `_tmp` deletes the shim script it points at, so the env
            // var never dangles at a just-deleted path even momentarily.
            _env_guard: EnvVarGuard,
            _tmp: tempfile::TempDir,
            log_path: PathBuf,
        }

        impl NpmShim {
            fn install(
                configured_prefix: &Path,
                exit_code: i32,
                stdout: &str,
                stderr: &str,
            ) -> Self {
                use std::os::unix::fs::PermissionsExt;
                let tmp = tempfile::TempDir::new().expect("tempdir");
                let bin_path = tmp.path().join("shim-npm");
                let log_path = tmp.path().join("argv.log");

                let stdout_lit = stdout.replace('\'', "'\\''");
                let stderr_lit = stderr.replace('\'', "'\\''");
                let log_lit = log_path.display().to_string().replace('\'', "'\\''");
                let prefix_lit = configured_prefix
                    .display()
                    .to_string()
                    .replace('\'', "'\\''");

                let script = format!(
                    "#!/bin/sh\n\
                     printf '%s\\n' \"$*\" >> '{log_lit}'\n\
                     case \"$*\" in\n\
                     'config get prefix') printf '%s' '{prefix_lit}' ;;\n\
                     *) printf '%s' '{stdout_lit}'; printf '%s' '{stderr_lit}' 1>&2; exit {exit_code} ;;\n\
                     esac\n",
                );
                std::fs::write(&bin_path, script).expect("write shim");
                let mut perms = std::fs::metadata(&bin_path).expect("stat").permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&bin_path, perms).expect("chmod");

                let env_guard = EnvVarGuard::set(SHIM_ENV, &bin_path.to_string_lossy());

                Self {
                    _env_guard: env_guard,
                    _tmp: tmp,
                    log_path,
                }
            }

            /// Answers `config get prefix` with a fresh writable tempdir —
            /// the common "working case" — returning it alongside the shim
            /// so callers can assert against it.
            fn with_writable_prefix(
                exit_code: i32,
                stdout: &str,
                stderr: &str,
            ) -> (Self, tempfile::TempDir) {
                let prefix_dir = tempfile::tempdir().expect("tempdir");
                let shim = Self::install(prefix_dir.path(), exit_code, stdout, stderr);
                (shim, prefix_dir)
            }

            fn argv_log(&self) -> String {
                std::fs::read_to_string(&self.log_path).unwrap_or_default()
            }
        }

        /// Unset both npm prefix env-vars for the test body so a real
        /// `npm_config_prefix`/`NPM_CONFIG_PREFIX` set in the ambient
        /// environment can never short-circuit the resolver ahead of the
        /// shim-driven scenario under test.
        fn clear_npm_env_prefix() -> (EnvVarGuard, EnvVarGuard) {
            (
                EnvVarGuard::unset("npm_config_prefix"),
                EnvVarGuard::unset("NPM_CONFIG_PREFIX"),
            )
        }

        /// A prefix whose `lib/node_modules` can never be created, so the
        /// write-probe fails and the resolver takes its `$HOME/.npm-global`
        /// fallback — the branch that performs the filesystem write.
        ///
        /// The block is structural, not permissions-based: the deepest
        /// existing ancestor is a regular FILE, so the probe's `File::create`
        /// fails with `ENOTDIR` on every OS and for every user, root included.
        /// A permission-denied path (`/proc/self/...`) only blocks where that
        /// path exists AND the runner is unprivileged — on FreeBSD, where
        /// procfs is not mounted, the probe walked up to `/`, found it
        /// writable as root, and the resolver never took the fallback.
        fn unwritable_prefix() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().expect("tempdir");
            let blocker = dir.path().join("not-a-directory");
            std::fs::write(&blocker, b"").expect("write blocker file");
            let prefix = blocker.join("cfgd-npm-prefix");
            (dir, prefix)
        }

        /// A module declaring one npm package, so the profile names npm and
        /// the planner has a reason to consider the manager at all.
        fn npm_module() -> cfgd_core::modules::ResolvedModule {
            let mut module = cfgd_core::test_helpers::make_resolved_module("tools");
            module.packages = vec![cfgd_core::modules::ResolvedPackage {
                canonical_name: "typescript".to_string(),
                resolved_name: "typescript".to_string(),
                manager: "npm".to_string(),
                manager_declared: false,
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            }];
            module
        }

        /// `cfgd plan` is read-only, and the daemon re-plans on every tick. A
        /// planner that reached for `PackageManager::path_dirs()` would spawn
        /// `npm config get prefix` on each of those ticks — needless process
        /// overhead for a command that promises to change nothing, and only
        /// `install()` is allowed to create `$HOME/.npm-global`. Planning
        /// reads the bootstrap record instead, and this pins that it stays
        /// that way.
        #[test]
        #[serial]
        fn reconciler_plan_neither_spawns_npm_nor_creates_a_global_prefix() {
            use cfgd_core::providers::ProviderRegistry;
            use cfgd_core::reconciler::{ReconcileContext, Reconciler};

            let _clear = clear_npm_env_prefix();
            // Force the unelevated branch: under root the resolver
            // short-circuits before the probe, which would make the assertions
            // below pass without proving anything.
            let _elevated = with_test_elevated_guard(false);
            let (_blocker_dir, unwritable) = unwritable_prefix();
            let shim = NpmShim::install(&unwritable, 0, "", "");

            let tmp_home = tempfile::tempdir().expect("tempdir");
            let _home = cfgd_core::with_test_home_guard(tmp_home.path());

            let state = cfgd_core::test_helpers::test_state();
            let mut registry = ProviderRegistry::new();
            registry.set_package_managers(crate::packages::all_package_managers());
            let reconciler = Reconciler::new(&registry, &state);

            reconciler
                .plan(
                    &cfgd_core::test_helpers::make_empty_resolved(),
                    Vec::new(),
                    Vec::new(),
                    vec![npm_module()],
                    ReconcileContext::Apply,
                )
                .expect("plan");

            let global_prefix = tmp_home.path().join(".npm-global");
            assert!(
                !global_prefix.exists(),
                "planning must not create {}",
                global_prefix.display()
            );
            let argv = shim.argv_log();
            assert!(
                !argv.contains("config get prefix"),
                "planning must not probe npm for its prefix: {argv}"
            );

            // Prove the two assertions above are not vacuous: `install()` —
            // the one operation that IS allowed to create the fallback
            // prefix and spawn npm's prefix probe — does exactly what
            // planning is asserted not to.
            let printer = test_printer();
            let cx = PackageContext::new(&printer, &state);
            NpmManager
                .install(&["cfgd-fixture-check-pkg".to_string()], &cx)
                .expect("install");
            assert!(
                global_prefix.exists(),
                "fixture check: install() is expected to create {}",
                global_prefix.display()
            );
            assert!(
                shim.argv_log().contains("config get prefix"),
                "fixture check: install() is expected to probe npm for its prefix"
            );
        }

        /// The apply tree renders one line per action, and a manager's own
        /// output window must not settle a second one for the same work. Every
        /// other test of that invariant uses a mock manager, which never opens a
        /// window at all — this one drives a REAL manager through a REAL
        /// shell-out, so a window that settles its own line is visible here.
        #[test]
        #[serial]
        fn a_real_install_shell_out_emits_one_line_under_apply() {
            use cfgd_core::providers::{PackageAction, ProviderRegistry};
            use cfgd_core::reconciler::{
                Action, Owner, Phase, PhaseName, Plan, ReconcileContext, Reconciler,
            };

            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let (_shim, _prefix_dir) = NpmShim::with_writable_prefix(0, "added 1 package\n", "");

            let state = cfgd_core::test_helpers::test_state();
            let mut registry = ProviderRegistry::new();
            registry.set_package_managers(crate::packages::all_package_managers());
            let reconciler = Reconciler::new(&registry, &state);

            let plan = Plan {
                phases: vec![Phase::from_actions(
                    PhaseName::Packages,
                    &Owner::profile("work"),
                    vec![Action::Package(PackageAction::Install {
                        manager: "npm".to_string(),
                        packages: vec!["typescript".to_string()],
                        origin: "local".to_string(),
                    })],
                )],
                warnings: vec![],
            };

            let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
            let result = reconciler
                .apply(
                    &plan,
                    &cfgd_core::test_helpers::make_empty_resolved(),
                    Path::new("."),
                    &printer,
                    None,
                    &[],
                    ReconcileContext::Apply,
                    false,
                    None,
                    &cfgd_core::AbortFlag::new(),
                )
                .expect("apply");
            assert_eq!(result.action_results.len(), 1, "one action was planned");

            let out = cfgd_core::output::strip_ansi(&cap.human());
            let settled: Vec<&str> = out
                .lines()
                .map(str::trim_start)
                .filter(|l| {
                    ['\u{2713}', '\u{2717}', '\u{26A0}', '\u{2014}', '\u{25C9}']
                        .iter()
                        .any(|g| l.starts_with(*g))
                })
                .collect();
            assert_eq!(
                settled.len(),
                1,
                "the tree's line and nothing settled by the window: {out}"
            );
            assert!(
                settled[0].contains("npm install typescript"),
                "the action's line is the tree's, built from the plan: {out}"
            );
        }

        #[test]
        #[serial]
        fn npm_install_passes_install_g_with_packages() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let (s, _prefix_dir) = NpmShim::with_writable_prefix(0, "", "");
            let p = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&p, &state);
            NpmManager
                .install(&["typescript".into(), "eslint".into()], &cx)
                .expect("Ok");
            let argv = s.argv_log();
            assert!(
                argv.contains("install -g typescript eslint"),
                "argv must include install -g + packages: {argv}"
            );
            assert!(
                !argv.contains("--prefix"),
                "a writable configured prefix must not add --prefix (no gratuitous \
                 change to the working case): {argv}"
            );
        }

        #[test]
        #[serial]
        fn npm_install_skips_command_when_empty() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&p, &state);
            NpmManager.install(&[], &cx).expect("Ok");
            assert_eq!(s.invocation_count(), 0);
        }

        #[test]
        #[serial]
        fn npm_uninstall_passes_uninstall_g_with_packages() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let (s, _prefix_dir) = NpmShim::with_writable_prefix(0, "", "");
            let p = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&p, &state);
            NpmManager
                .uninstall(&["typescript".into()], &cx)
                .expect("Ok");
            let argv = s.argv_log();
            assert!(argv.contains("uninstall -g typescript"));
            assert!(!argv.contains("--prefix"), "got: {argv}");
        }

        #[test]
        #[serial]
        fn npm_declares_no_index_and_refreshing_upgrades_nothing() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let (s, _prefix_dir) = NpmShim::with_writable_prefix(0, "", "");
            let p = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&p, &state);
            assert!(
                !NpmManager.has_index(),
                "npm resolves the registry on every install"
            );
            NpmManager.refresh_index(&cx).expect("Ok");
            let argv = s.argv_log();
            assert!(
                !argv.contains("update"),
                "`npm update -g` upgrades every global package the user never declared: {argv}"
            );
        }

        #[test]
        #[serial]
        fn npm_available_version_runs_view_and_returns_trimmed_stdout() {
            let _s = ToolShim::install(SHIM_ENV, 0, "5.3.3\n", "");
            let v = NpmManager.available_version("typescript").expect("Ok");
            assert_eq!(v.as_deref(), Some("5.3.3"));
        }

        #[test]
        #[serial]
        fn npm_available_version_passes_view_subcommand_with_package_and_field() {
            let s = ToolShim::install(SHIM_ENV, 0, "1.0.0", "");
            NpmManager.available_version("typescript").expect("Ok");
            let argv = s.argv_log();
            assert!(
                argv.contains("view typescript version"),
                "argv must include view <pkg> version: {argv}"
            );
        }

        #[test]
        #[serial]
        fn npm_available_version_returns_none_on_nonzero_exit() {
            let _s = ToolShim::install(SHIM_ENV, 1, "", "404 not found");
            let v = NpmManager
                .available_version("nonexistent")
                .expect("non-zero → Ok(None) not Err");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn npm_available_version_returns_none_on_empty_stdout() {
            let _s = ToolShim::install(SHIM_ENV, 0, "\n   \n", "");
            let v = NpmManager
                .available_version("weird-pkg")
                .expect("empty stdout → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn npm_installed_packages_parses_npm_list_json() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let json = r#"{"dependencies":{"typescript":{"version":"5.3.3"},"eslint":{"version":"8.0.0"}}}"#;
            // npm list exits non-zero on peer dep issues; stdout still valid JSON.
            let (_s, _prefix_dir) = NpmShim::with_writable_prefix(1, json, "peer dep issues");
            let printer = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            let pkgs = NpmManager.installed_packages(&cx).expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("typescript"));
            assert!(pkgs.contains("eslint"));
        }

        #[test]
        #[serial]
        fn npm_installed_packages_with_versions_includes_versions() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let json = r#"{"dependencies":{"typescript":{"version":"5.3.3"}}}"#;
            let (_s, _prefix_dir) = NpmShim::with_writable_prefix(0, json, "");
            let printer = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            let pkgs = NpmManager
                .installed_packages_with_versions(&cx)
                .expect("Ok");
            let ts = pkgs
                .iter()
                .find(|p| p.name == "typescript")
                .expect("typescript present");
            assert_eq!(ts.version, "5.3.3");
        }

        /// A writable configured prefix must not add `--prefix` to a listing
        /// call either — the argv-omission guarantee applies uniformly, not
        /// just to `install`.
        #[test]
        #[serial]
        fn npm_installed_packages_uses_configured_prefix_without_prefix_flag() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let (s, _prefix_dir) = NpmShim::with_writable_prefix(0, "{}", "");
            let printer = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            NpmManager.installed_packages(&cx).expect("Ok");
            let argv = s.argv_log();
            assert!(
                !argv.contains("--prefix"),
                "a writable configured prefix must not add --prefix: {argv}"
            );
        }

        /// `installed_packages()` is reached from `cfgd verify` and the daemon
        /// sync loop on every tick — a query path with no license to mutate
        /// the filesystem. It must still resolve the CORRECT fallback prefix
        /// (proven via the `--prefix` argv value) even though it must not
        /// bring `$HOME/.npm-global` into existence to do so.
        #[test]
        #[serial]
        fn npm_installed_packages_resolves_fallback_prefix_without_creating_it() {
            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(false);
            let (_blocker_dir, unwritable) = unwritable_prefix();
            let s = NpmShim::install(&unwritable, 0, "{}", "");

            let tmp_home = tempfile::tempdir().expect("tempdir");
            let global_prefix = tmp_home.path().join(".npm-global");
            let printer = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            let dirs = cfgd_core::with_test_home(tmp_home.path(), || {
                NpmManager.installed_packages(&cx).expect("Ok");
                npm_path_dirs_for(false)
            });

            assert!(
                !global_prefix.exists(),
                "a query path must not create {}",
                global_prefix.display()
            );
            assert_eq!(
                dirs,
                vec![cfgd_core::to_posix_string(global_prefix.join("bin"))],
                "the resolved fallback prefix must still be correct, even though \
                 nothing on this path may create it"
            );
            let argv = s.argv_log();
            assert!(
                argv.contains(&format!("--prefix {}", global_prefix.display())),
                "installed_packages() must pass the correct (uncreated) fallback \
                 prefix on argv: {argv}"
            );
        }

        // bootstrap: brew-first cascade. A successful brew shim exercises the
        // early-return path through `bootstrap_via_brew_then_system`.
        #[test]
        #[serial]
        fn npm_bootstrap_via_brew_returns_ok() {
            let s = ToolShim::install("CFGD_BREW_BIN", 0, "", "");
            let p = test_printer();
            NpmManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect("bootstrap Ok via brew");
            assert!(
                s.argv_log().contains("install node"),
                "brew argv must include `install node`: {}",
                s.argv_log()
            );
        }

        /// Point the seam env-var at a non-existent path so the spawned
        /// `Command` fails with ENOENT, exercising the `CommandFailed` map_err
        /// arm rather than a non-zero exit (which the shim handles differently).
        fn install_unspawnable() -> EnvVarGuard {
            EnvVarGuard::set(SHIM_ENV, "/nonexistent/cfgd-npm-shim-does-not-exist")
        }

        #[test]
        #[serial]
        fn npm_installed_packages_spawn_failure_maps_to_command_failed() {
            let _g = install_unspawnable();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let printer = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            let err = NpmManager
                .installed_packages(&cx)
                .expect_err("ENOENT spawn must surface as CommandFailed, not a panic");
            assert!(
                matches!(err, cfgd_core::errors::CfgdError::Package(
                    PackageError::CommandFailed { ref manager, .. }) if manager == "npm"),
                "spawn failure must be PackageError::CommandFailed{{manager:\"npm\"}}, got: {err:?}"
            );
        }

        #[test]
        #[serial]
        fn npm_available_version_spawn_failure_maps_to_command_failed() {
            let _g = install_unspawnable();
            let err = NpmManager
                .available_version("typescript")
                .expect_err("ENOENT spawn must surface as CommandFailed");
            assert!(
                matches!(err, cfgd_core::errors::CfgdError::Package(
                    PackageError::CommandFailed { ref manager, .. }) if manager == "npm"),
                "got: {err:?}"
            );
        }

        #[test]
        #[serial]
        fn npm_installed_packages_with_versions_spawn_failure_maps_to_command_failed() {
            let _g = install_unspawnable();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            let printer = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            let err = NpmManager
                .installed_packages_with_versions(&cx)
                .expect_err("ENOENT spawn must surface as CommandFailed");
            assert!(
                matches!(err, cfgd_core::errors::CfgdError::Package(
                    PackageError::CommandFailed { ref manager, .. }) if manager == "npm"),
                "got: {err:?}"
            );
        }

        #[test]
        #[serial]
        fn npm_installed_packages_with_versions_invalid_json_maps_to_list_failed() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());
            // npm list exits 0 here but emits non-JSON; the version path must
            // surface ListFailed with the parse-error context, not panic.
            let (_s, _prefix_dir) = NpmShim::with_writable_prefix(0, "this is not json", "");
            let printer = test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            let err = NpmManager
                .installed_packages_with_versions(&cx)
                .expect_err("invalid JSON must surface as ListFailed");
            let msg = err.to_string();
            assert!(
                msg.contains("npm") && msg.contains("failed to parse npm list output"),
                "error must name npm + parse-failure context, got: {msg}"
            );
        }

        // -------------------------------------------------------------
        // Prefix-resolution behaviour (the actual defect this file fixes).
        // -------------------------------------------------------------

        /// Toggle write permission on `path`. Restores to a normal writable
        /// mode before the tempdir's own `Drop` removes it, since an empty
        /// directory only needs write on its PARENT to be rmdir'd, but
        /// leaving a 0o555 directory around outlives the guarantee this test
        /// relies on to clean up after itself.
        fn set_writable(path: &Path, writable: bool) {
            let mode = if writable { 0o755 } else { 0o555 };
            cfgd_core::set_file_permissions(path, mode).expect("chmod probe dir");
        }

        #[test]
        #[serial]
        fn npm_install_uses_fallback_prefix_on_argv_when_configured_prefix_unwritable() {
            // The decision is driven directly via `resolve_npm_prefix_with`
            // with `is_writable` forced to `false`, so this runs for real at
            // any uid — a root process bypasses the real write-probe (see
            // npm_prefix_is_writable_returns_false_for_unwritable_directory
            // for the one place that genuinely needs a root guard).
            let _clear = clear_npm_env_prefix();
            let rejected = tempfile::tempdir().expect("tempdir");
            let home = tempfile::tempdir().expect("tempdir");
            let _shim = NpmShim::install(rejected.path(), 0, "", "");
            let decision = cfgd_core::with_test_home(home.path(), || {
                resolve_npm_prefix_with(false, |_| false)
            })
            .expect("Ok");
            assert!(
                decision.is_fallback,
                "an unwritable configured prefix must resolve to the fallback branch"
            );

            let mut cmd = npm_cmd();
            apply_prefix_flag(&mut cmd, &decision);
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                args,
                vec![
                    "--prefix".to_string(),
                    decision
                        .prefix
                        .as_deref()
                        .expect("fallback decision carries a prefix")
                        .to_string_lossy()
                        .into_owned(),
                ],
                "an unwritable configured prefix must fall back onto argv: {args:?}"
            );
            assert_ne!(
                args[1],
                rejected.path().to_string_lossy(),
                "the rejected unwritable prefix must never itself land on argv"
            );
        }

        #[test]
        #[serial]
        fn npm_install_and_installed_packages_resolve_to_same_fallback_prefix() {
            // Same root-independence rationale as the test above: forces
            // both resolutions into the fallback branch via the injected
            // `is_writable` predicate instead of a real unwritable directory.
            let _clear = clear_npm_env_prefix();
            let rejected = tempfile::tempdir().expect("tempdir");
            let home = tempfile::tempdir().expect("tempdir");
            let _shim = NpmShim::install(rejected.path(), 0, "{}", "");
            let (install_side, listing_side) = cfgd_core::with_test_home(home.path(), || {
                (
                    resolve_npm_prefix_with(false, |_| false),
                    resolve_npm_prefix_with(false, |_| false),
                )
            });
            let install_side = install_side.expect("install-side resolve Ok");
            let listing_side = listing_side.expect("installed_packages-side resolve Ok");
            assert!(
                install_side.is_fallback && listing_side.is_fallback,
                "both resolutions must land in the fallback branch: {install_side:?} / {listing_side:?}"
            );
            assert_eq!(
                install_side, listing_side,
                "install and installed_packages must resolve to the identical \
                 fallback prefix — divergence here means state never converges"
            );
        }

        /// End-to-end proof of the single most load-bearing property:
        /// `NpmManager::install()` and `NpmManager::installed_packages()`
        /// converge on ONE prefix through the real, uninjected composition —
        /// real `npm_configured_prefix()`, real `npm_prefix_is_writable()`,
        /// real `npm_fallback_prefix()`, real argv construction. Only the
        /// elevation answer is injected (`with_test_elevated`); the two tests
        /// above prove the resolver is deterministic in isolation, but this is
        /// the one that proves the production methods actually land in the
        /// same place, and it also exercises `install()`'s fallback-notice
        /// branch, which nothing else in this suite reaches.
        ///
        /// The shim answers `npm config get prefix` with a RELATIVE path whose
        /// first segment does not exist anywhere under this crate's test CWD
        /// (`crates/cfgd/`, verified: no `cfgd-absent-prefix-root` entry
        /// exists there). `npm_prefix_is_writable` computes
        /// `deepest_existing_ancestor` on `<configured_prefix>/lib/node_modules`;
        /// walking `.parent()` up a chain of nonexistent relative components
        /// terminates at the empty path (`""`), whose own `.parent()` is
        /// `None` — so the real write-probe returns `false` deterministically,
        /// without ever touching the filesystem, on root and non-root alike.
        /// Root's Unix-DAC bypass is irrelevant here: there is no real
        /// directory for it to bypass permissions on.
        #[test]
        #[serial]
        fn npm_install_and_installed_packages_converge_through_real_composition() {
            let _clear = clear_npm_env_prefix();
            let configured = Path::new("cfgd-absent-prefix-root/relative-prefix");
            let shim = NpmShim::install(configured, 0, "{}", "");
            let home = tempfile::tempdir().expect("tempdir");
            let (printer, buf) =
                cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

            // Two INDEPENDENT in-memory state stores, not one shared between
            // the two calls: the point of this test is proving both methods
            // land on the same prefix by independently re-deriving it through
            // the real composition, not by one reading the other's cached
            // decision. A shared store would make the second call a cache
            // hit, which proves nothing about the resolver itself.
            let install_state = cfgd_core::test_helpers::test_state();
            let installed_state = cfgd_core::test_helpers::test_state();
            let install_cx = PackageContext::new(&printer, &install_state);
            let installed_cx = PackageContext::new(&printer, &installed_state);

            let (install_result, installed_result) = cfgd_core::with_test_home(home.path(), || {
                with_test_elevated(false, || {
                    (
                        NpmManager.install(&["typescript".to_string()], &install_cx),
                        NpmManager.installed_packages(&installed_cx),
                    )
                })
            });
            install_result.expect("install() must resolve through the real fallback composition");
            installed_result
                .expect("installed_packages() must resolve through the real fallback composition");

            let expected_fallback = cfgd_core::with_test_home(home.path(), npm_fallback_prefix);

            let log = shim.argv_log();
            let prefix_values: Vec<String> = log
                .lines()
                .filter(|line| line.contains("--prefix"))
                .filter_map(|line| {
                    line.split_whitespace()
                        .skip_while(|tok| *tok != "--prefix")
                        .nth(1)
                        .map(str::to_string)
                })
                .collect();
            assert_eq!(
                prefix_values.len(),
                2,
                "expected exactly two real `--prefix`-bearing invocations \
                 (install + installed_packages), got argv log:\n{log}"
            );
            assert_eq!(
                prefix_values[0], prefix_values[1],
                "install() and installed_packages() must converge on the \
                 identical prefix through the real, uninjected composition: {prefix_values:?}"
            );
            assert_eq!(
                prefix_values[0],
                expected_fallback.to_string_lossy(),
                "the converged prefix must be the real npm_fallback_prefix(), \
                 not merely equal to itself"
            );

            let captured = cfgd_core::test_helpers::captured_text(&buf);
            assert!(
                // The tag says npm; the body must not (`no_provider_note_repeats_its_own_tag`).
                captured.contains("no writable global prefix"),
                "install()'s fallback-notice branch must have executed: {captured}"
            );
        }

        /// The two halves of the persistence contract, through the real shim:
        /// while the configured prefix stays unwritable a fallback decision is
        /// REUSED unchanged across calls (stability — packages installed under
        /// it must stay visible), and the moment the configured prefix probes
        /// writable an EMPTY fallback is DISCARDED and the resolve promotes
        /// onto it, re-persisting `is_fallback: false`. The promoted decision
        /// then enjoys the same stability, without re-asking npm. The home
        /// here is a fresh tempdir, so the fallback holds nothing and
        /// promotion costs nothing — the sibling below covers the fallback
        /// that does hold packages.
        #[test]
        #[serial]
        fn resolve_npm_prefix_reuses_a_fallback_until_the_configured_prefix_heals() {
            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(false);
            let home = tempfile::tempdir().expect("tempdir");
            let state = cfgd_core::test_helpers::test_state();

            let (_blocker_dir, unwritable) = unwritable_prefix();
            let _first_shim = NpmShim::install(&unwritable, 0, "", "");

            let first = cfgd_core::with_test_home(home.path(), || resolve_npm_prefix(&state))
                .expect("first resolve must succeed");
            assert!(
                first.is_fallback,
                "the unwritable configured prefix must resolve to the fallback branch first"
            );

            let second = cfgd_core::with_test_home(home.path(), || resolve_npm_prefix(&state))
                .expect("second resolve must succeed");
            assert_eq!(
                second, first,
                "while the configured prefix stays unwritable, the fallback \
                 decision must be reused unchanged"
            );

            // The machine heals: the shim now answers a genuinely writable
            // configured prefix. The revalidation probe must notice and
            // promote off the fallback.
            let writable_dir = tempfile::tempdir().expect("tempdir");
            let _second_shim = NpmShim::install(writable_dir.path(), 0, "", "");

            let promoted = cfgd_core::with_test_home(home.path(), || resolve_npm_prefix(&state))
                .expect("promoting resolve must succeed");
            assert_eq!(
                promoted,
                NpmPrefixDecision {
                    prefix: Some(writable_dir.path().to_path_buf()),
                    is_fallback: false,
                },
                "a configured prefix that probes writable again must be \
                 promoted onto automatically"
            );

            let (persisted_prefix, persisted_is_fallback) = state
                .package_manager_prefix("npm")
                .expect("read back")
                .expect("the promotion must overwrite the fallback row");
            assert_eq!(
                persisted_prefix,
                cfgd_core::to_posix_fs_key(writable_dir.path()),
                "the promoted decision must be persisted, not merely returned"
            );
            assert!(!persisted_is_fallback);

            let steady = cfgd_core::with_test_home(home.path(), || resolve_npm_prefix(&state))
                .expect("steady-state resolve must succeed");
            assert_eq!(
                steady, promoted,
                "the promoted decision must be reused unchanged on later calls"
            );
        }

        /// The convergence guarantee across a healing machine, end to end
        /// through the production methods: `install()` puts packages under the
        /// fallback, npm's configured prefix then becomes writable, and
        /// `installed_packages()` must STILL ask the fallback. Promoting there
        /// would report every package installed under it as missing, so the
        /// next plan re-queues work that is already done and the module's
        /// post-scripts re-run on a converged machine — which is exactly what
        /// a `--prefix`-less second invocation in the argv log would mean.
        #[test]
        #[serial]
        fn installed_packages_keeps_asking_a_fallback_prefix_that_holds_packages() {
            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(false);
            let home = tempfile::tempdir().expect("tempdir");
            let state = cfgd_core::test_helpers::test_state();
            let (printer, _buf) =
                cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
            let cx = PackageContext::new(&printer, &state);

            let (_blocker_dir, unwritable) = unwritable_prefix();
            let first_shim = NpmShim::install(&unwritable, 0, "{}", "");
            cfgd_core::with_test_home(home.path(), || {
                NpmManager.install(&["yarn".to_string()], &cx)
            })
            .expect("install must resolve through the fallback composition");
            let fallback = cfgd_core::with_test_home(home.path(), npm_fallback_prefix);
            assert!(
                first_shim
                    .argv_log()
                    .contains(&format!("--prefix {}", fallback.display())),
                "install must have targeted the fallback: {}",
                first_shim.argv_log()
            );
            // The shim spawns no real npm, so stand in for what the install
            // just claimed to do: a package now lives under the fallback.
            std::fs::create_dir_all(npm_global_modules_dir(&fallback).join("yarn"))
                .expect("seed the installed package");

            // The machine heals — a writable configured prefix appears.
            let (second_shim, writable_dir) = NpmShim::with_writable_prefix(0, "{}", "");
            cfgd_core::with_test_home(home.path(), || NpmManager.installed_packages(&cx))
                .expect("installed_packages must succeed");

            let log = second_shim.argv_log();
            assert!(
                log.contains(&format!("--prefix {}", fallback.display())),
                "the query must still ask the prefix the install wrote to: {log}"
            );
            assert!(
                !log.contains(&writable_dir.path().display().to_string()),
                "promoting onto the healed prefix would orphan every package \
                 already installed under the fallback: {log}"
            );
        }

        /// CRIT-1 regression, exercised through the actual caching entry
        /// point rather than the uncached `resolve_npm_prefix_for` directly:
        /// an elevated run must not adopt a decision an earlier unelevated
        /// run persisted. Without the ordering fix, the cache read at the
        /// top of `resolve_npm_prefix` would return the seeded row before
        /// the elevation short-circuit ever ran.
        #[test]
        #[serial]
        fn resolve_npm_prefix_ignores_a_persisted_decision_when_elevated() {
            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(true);
            let state = cfgd_core::test_helpers::test_state();
            state
                .record_package_manager_prefix("npm", "/some/persisted/prefix", false)
                .expect("seed a persisted decision");

            let decision = resolve_npm_prefix(&state).expect("resolve must succeed");

            assert_eq!(
                decision,
                NpmPrefixDecision {
                    prefix: None,
                    is_fallback: false,
                },
                "an elevated process must never adopt a persisted decision \
                 recorded by an earlier unelevated run"
            );
        }

        /// CRIT-1 regression, env-pinned half: a persisted row must not
        /// preempt an `npm_config_prefix`/`NPM_CONFIG_PREFIX` pin set after
        /// the row was recorded.
        #[test]
        #[serial]
        fn resolve_npm_prefix_ignores_a_persisted_decision_when_env_pinned() {
            let _elevated = with_test_elevated_guard(false);
            let _pin = EnvVarGuard::set("npm_config_prefix", "/opt/node");
            let _upper_clear = EnvVarGuard::unset("NPM_CONFIG_PREFIX");
            let state = cfgd_core::test_helpers::test_state();
            state
                .record_package_manager_prefix("npm", "/some/persisted/prefix", true)
                .expect("seed a persisted decision");

            let decision = resolve_npm_prefix(&state).expect("resolve must succeed");

            assert_eq!(
                decision,
                NpmPrefixDecision {
                    prefix: None,
                    is_fallback: false,
                },
                "an env-pinned run must never adopt a persisted decision \
                 recorded before the pin was set"
            );
        }

        /// CRIT-2 regression: a persisted decision that no longer passes the
        /// write-probe (permissions revoked, or the directory gone since the
        /// row was recorded) must be discarded rather than trusted verbatim,
        /// and the fresh decision derived in its place must overwrite the
        /// stale row rather than merely winning in-memory for this one call.
        #[test]
        #[serial]
        fn resolve_npm_prefix_discards_a_persisted_decision_that_is_no_longer_writable() {
            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(false);
            let home = tempfile::tempdir().expect("tempdir");
            let state = cfgd_core::test_helpers::test_state();

            let (_blocker_dir, stale_unwritable) = unwritable_prefix();
            state
                .record_package_manager_prefix(
                    "npm",
                    &cfgd_core::to_posix_fs_key(&stale_unwritable),
                    false,
                )
                .expect("seed a stale persisted decision");

            let (_shim, writable_dir) = NpmShim::with_writable_prefix(0, "", "");

            let decision = cfgd_core::with_test_home(home.path(), || resolve_npm_prefix(&state))
                .expect("resolve must succeed");

            assert_eq!(
                decision,
                NpmPrefixDecision {
                    prefix: Some(writable_dir.path().to_path_buf()),
                    is_fallback: false,
                },
                "a persisted decision that fails today's write-probe must be \
                 discarded and re-derived from the live composition"
            );

            let (persisted_prefix, persisted_is_fallback) = state
                .package_manager_prefix("npm")
                .expect("read back")
                .expect("a fresh decision must have been persisted, overwriting the stale row");
            assert_eq!(
                persisted_prefix,
                cfgd_core::to_posix_fs_key(writable_dir.path()),
                "the stale row must be overwritten, not merely bypassed for this call"
            );
            assert!(!persisted_is_fallback);
        }

        /// The probe's boundary proven at ANY uid: a non-directory ancestor
        /// fails `File::create` with `ENOTDIR`, which no privilege level
        /// bypasses. The `0o555` sibling below can only run unelevated, so on
        /// the root CI runners it is this assertion that keeps the probe
        /// covered at all.
        #[test]
        fn npm_prefix_is_writable_returns_false_when_ancestor_is_not_a_directory() {
            let (_blocker_dir, prefix) = unwritable_prefix();
            assert!(
                !npm_prefix_is_writable(&prefix),
                "a prefix nested under a regular file must fail the write-probe"
            );
        }

        #[test]
        fn npm_prefix_is_writable_returns_false_for_unwritable_directory() {
            // The write-probe performs a real filesystem write, and root
            // bypasses Unix DAC permission checks entirely, so an unwritable
            // directory cannot be constructed under root. The ENOTDIR case
            // above covers the probe at any uid; this one covers the DAC path
            // specifically, which is only observable unelevated.
            if cfgd_core::is_root() {
                return;
            }
            let dir = tempfile::tempdir().expect("tempdir");
            set_writable(dir.path(), false);
            let writable = npm_prefix_is_writable(dir.path());
            set_writable(dir.path(), true);
            assert!(
                !writable,
                "a chmod 0o555 directory must fail the write-probe"
            );
        }

        #[test]
        #[serial]
        fn npm_path_dirs_reports_configured_prefix_bin_dir_when_writable() {
            let _clear = clear_npm_env_prefix();
            let (_shim, prefix_dir) = NpmShim::with_writable_prefix(0, "", "");
            let dirs = npm_path_dirs_for(false);
            assert_eq!(
                dirs,
                vec![cfgd_core::to_posix_string(npm_bin_dir(prefix_dir.path()))],
                "path_dirs must report the writable configured prefix's bin dir"
            );
        }

        #[test]
        #[serial]
        fn npm_path_dirs_reports_fallback_prefix_bin_dir_when_configured_prefix_missing() {
            let _clear = clear_npm_env_prefix();
            let home = tempfile::tempdir().expect("tempdir");
            // Empty stdout for `config get prefix` makes npm_configured_prefix()
            // return Ok(None), landing in the fallback branch without ever
            // invoking the write-probe — root-independent by construction.
            let _shim = NpmShim::install(Path::new(""), 0, "", "");
            let (dirs, fallback) = cfgd_core::with_test_home(home.path(), || {
                (npm_path_dirs_for(false), npm_fallback_prefix())
            });
            assert_eq!(
                dirs,
                vec![cfgd_core::to_posix_string(npm_bin_dir(&fallback))],
                "path_dirs must report the fallback prefix's bin dir when npm has no usable configured prefix"
            );
        }

        /// `created_path_dirs` answers for the directory cfgd MADE, which is
        /// only ever the `$HOME/.npm-global` fallback: a configured prefix
        /// belongs to npm or to the system, so it stays out of the generated
        /// env file even though it is exactly where npm's binaries live —
        /// which `path_dirs` keeps saying.
        #[test]
        #[serial]
        fn npm_created_path_dirs_reports_only_the_prefix_cfgd_makes_itself() {
            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(false);
            let printer = test_printer();

            let home = tempfile::tempdir().expect("tempdir");
            // Empty stdout for `config get prefix` lands in the fallback branch
            // without invoking the write-probe — root-independent by
            // construction.
            let fallback_shim = NpmShim::install(Path::new(""), 0, "", "");
            let fallback_state = cfgd_core::test_helpers::test_state();
            let fallback_cx = PackageContext::new(&printer, &fallback_state);
            let (created, fallback) = cfgd_core::with_test_home(home.path(), || {
                (
                    NpmManager.created_path_dirs(&fallback_cx),
                    npm_fallback_prefix(),
                )
            });
            assert_eq!(
                created,
                vec![cfgd_core::to_posix_string(npm_bin_dir(&fallback))],
                "the fallback prefix is cfgd's own creation and must be reported"
            );
            drop(fallback_shim);

            let (_shim, prefix_dir) = NpmShim::with_writable_prefix(0, "", "");
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            assert!(
                NpmManager.created_path_dirs(&cx).is_empty(),
                "a writable configured prefix is not cfgd's to publish"
            );
            assert_eq!(
                NpmManager.path_dirs(&cx),
                vec![cfgd_core::to_posix_string(npm_bin_dir(prefix_dir.path()))],
                "that same prefix is still where npm's binaries live"
            );
        }

        /// A directory cfgd created earns an env entry however the manager got
        /// onto the machine. Nothing provisions npm here — the plan holds one
        /// install and no `Provision` — yet the prefix `install()` created has
        /// to reach the recorded PATH directories, which is what the generated
        /// env file is built from. Recorded through the install path, this is
        /// the case that used to reach the user only as an instruction to edit
        /// their own PATH.
        #[test]
        #[serial]
        fn an_install_that_creates_a_prefix_records_it_with_no_provision_in_the_run() {
            use cfgd_core::providers::{PackageAction, ProviderRegistry};
            use cfgd_core::reconciler::{
                Action, Owner, Phase, PhaseName, Plan, ReconcileContext, Reconciler,
            };

            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(false);
            let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture();
            let (_blocker_dir, unwritable) = unwritable_prefix();
            let _shim = NpmShim::install(&unwritable, 0, "", "");

            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());

            let state = cfgd_core::test_helpers::test_state();
            let mut registry = ProviderRegistry::new();
            registry.set_package_managers(crate::packages::all_package_managers());
            let reconciler = Reconciler::new(&registry, &state);

            let plan = Plan {
                phases: vec![Phase::from_actions(
                    PhaseName::Packages,
                    &Owner::profile("work"),
                    vec![Action::Package(PackageAction::Install {
                        manager: "npm".to_string(),
                        packages: vec!["typescript".to_string()],
                        origin: "local".to_string(),
                    })],
                )],
                warnings: vec![],
            };

            let printer = test_printer();
            reconciler
                .apply(
                    &plan,
                    &cfgd_core::test_helpers::make_empty_resolved(),
                    Path::new("."),
                    &printer,
                    None,
                    &[],
                    ReconcileContext::Apply,
                    false,
                    None,
                    &cfgd_core::AbortFlag::new(),
                )
                .expect("apply");

            assert_eq!(
                state
                    .bootstrapped_managers()
                    .expect("read path-dir records"),
                vec![(
                    "npm".to_string(),
                    vec![cfgd_core::to_posix_string(
                        home.path().join(".npm-global").join("bin")
                    )]
                )],
                "the prefix install() created must be recorded for the env file"
            );
        }

        /// cfgd owns `~/.cfgd.env`, so a directory it just created is written
        /// there rather than handed to the user as an errand. The note keeps
        /// only what writing a file cannot fix — where the packages went, and
        /// why the prefix moved.
        #[test]
        #[serial]
        fn the_fallback_prefix_note_tells_the_user_nothing_to_do_about_path() {
            let _clear = clear_npm_env_prefix();
            let _elevated = with_test_elevated_guard(false);
            let (_blocker_dir, unwritable) = unwritable_prefix();
            let _shim = NpmShim::install(&unwritable, 0, "", "");

            let home = tempfile::tempdir().expect("tempdir");
            let _home_guard = cfgd_core::with_test_home_guard(home.path());

            let (printer, buf) =
                cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
            let state = cfgd_core::test_helpers::test_state();
            let cx = PackageContext::new(&printer, &state);
            NpmManager
                .install(&["typescript".to_string()], &cx)
                .expect("install");

            let out = cfgd_core::test_helpers::captured_text(&buf);
            let note = out
                .lines()
                .find(|l| l.contains("no writable global prefix"))
                .expect("the fallback note is reported");
            let prefix = home.path().join(".npm-global").display().to_string();
            assert!(
                note.contains(&prefix),
                "the note must still say where the packages went: {note}"
            );
            // The prefix is a random tempdir path, and any substring test would
            // be answering about THAT rather than about the sentence: `add`
            // turns up in a tempdir name roughly once in 100k runs.
            let sentence = note.replace(&prefix, "<prefix>");
            assert!(
                !sentence.contains("PATH") && !sentence.contains("add"),
                "the note must not ask the user to edit PATH: {note}"
            );
        }

        #[test]
        #[serial]
        fn npm_config_prefix_env_var_is_left_alone() {
            let _g = EnvVarGuard::set("npm_config_prefix", "/wherever/the/user/pinned/it");
            let _unset_upper = EnvVarGuard::unset("NPM_CONFIG_PREFIX");
            let decision = resolve_npm_prefix_for(false).expect("Ok");
            assert_eq!(
                decision,
                NpmPrefixDecision {
                    prefix: None,
                    is_fallback: false,
                },
                "a pre-set npm_config_prefix must leave the resolver's decision \
                 as a no-op, never a fallback"
            );
        }

        #[test]
        #[serial]
        fn npm_elevated_run_is_not_overridden() {
            // Drive the decision function directly with elevated=true rather
            // than requiring real root: this keeps the test honest regardless
            // of which user runs the suite.
            let _clear = clear_npm_env_prefix();
            let decision = resolve_npm_prefix_for(true).expect("Ok");
            assert_eq!(
                decision,
                NpmPrefixDecision {
                    prefix: None,
                    is_fallback: false,
                },
                "an elevated process must never have its prefix overridden"
            );
        }
    }
}
