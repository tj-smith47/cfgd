//! Shared helpers used across package manager implementations.
//!
//! Process execution wrappers (`run_pkg_cmd*`), sudo helpers, brew detection +
//! invocation, generic system bootstrap routines, post-install caveat extraction,
//! and small string-trimming helpers for package name normalization.

use std::path::PathBuf;
use std::process::{Command, Output};

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::{CommandOutput, Role, collapse_to_subject_line};
use cfgd_core::providers::{ActionNote, PackageContext};

/// Compute the canonical env-var seam name for a package-manager binary.
/// Pattern: `CFGD_<NAME>_BIN`, with hyphens turned into underscores so
/// `brew-cask` maps to `CFGD_BREW_CASK_BIN`. Used by tests via ToolShim.
pub(super) fn tool_seam_var(name: &str) -> String {
    format!("CFGD_{}_BIN", name.to_uppercase().replace('-', "_"))
}

/// Canonical package name for the managers whose package ids are matched
/// case-INSENSITIVELY (Windows: chocolatey, scoop, winget). `choco list` /
/// `scoop export` / `winget list` echo an id in its REGISTERED case (e.g. `Wget`,
/// `Cosign`), while a user writes `wget` in the profile. Folding both the
/// installed-side parse AND `package_identity` (desired side) through this makes
/// install-idempotency, prune, and per-package tracking keys agree regardless of
/// case. Must NOT be applied to the case-sensitive Unix managers (apt/dnf/brew/…),
/// where distinct-case names can be distinct packages.
pub(super) fn canonical_ci_pkg_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Locate a package-manager binary. First checks the `CFGD_<NAME>_BIN` env-var
/// seam (tests inject a ToolShim path here); then `$PATH` via
/// `command_available`; on miss, walks each entry in `fallbacks` and returns
/// the first that exists. Returns `None` if nothing is found — matches the
/// `find_X() -> Option<PathBuf>` shape that cargo/pipx/go managers had
/// open-coded.
pub(super) fn resolve_tool_with_fallbacks(name: &str, fallbacks: &[PathBuf]) -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(tool_seam_var(name)) {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = cfgd_core::command_path(name) {
        return Some(p);
    }
    fallbacks.iter().find(|p| p.exists()).cloned()
}

/// Compute the leading argv for invoking a package-manager binary `name` given
/// its resolved full path (`None` when it was not found on `$PATH`) — the program
/// plus any fixed pre-arguments, WITHOUT the subcommand the caller appends.
///
/// On Windows a script shim cannot be launched by `Command::new(name)` (that only
/// finds `name.exe`), so a PowerShell shim is run via `powershell -File` and a
/// `.cmd`/`.bat` shim via `cmd /c` — both propagate the tool's real exit code so a
/// failed install is never mistaken for success. A `.exe`/`.com` (or any Unix
/// binary) is launched directly by its resolved path. When the tool was not found,
/// fall back to the bare name so the caller surfaces the normal "not found" error.
///
/// Pure and platform-neutral so it is unit-testable off Windows; the Windows-only
/// wiring lives in [`build_pkg_command`].
#[cfg(any(windows, test))]
pub(super) fn windows_pkg_argv(name: &str, resolved: Option<&std::path::Path>) -> Vec<String> {
    let Some(path) = resolved else {
        return vec![name.to_string()];
    };
    let p = path.to_string_lossy().into_owned(); // native-ok: Windows CreateProcess argv (backslash path), not a cross-OS key
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("ps1") => vec![
            "powershell".into(),
            "-NoProfile".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
            p,
        ],
        // Pass the shim path UNQUOTED: Rust wraps a space-bearing argv token in quotes
        // itself, and cmd.exe's "exactly two quotes around an executable file" rule then
        // preserves them. Quoting here instead would get the inner quotes backslash-escaped
        // and reach cmd.exe malformed.
        Some("cmd") | Some("bat") => vec!["cmd".into(), "/c".into(), p],
        _ => vec![p],
    }
}

/// Build a base `Command` for a package-manager binary, resolving it to a full
/// path so a Windows script shim (`.ps1`/`.cmd`) is invoked correctly rather than
/// dying with "program not found" (see [`windows_pkg_argv`]). On non-Windows this
/// is just `Command::new(<resolved-or-name>)`.
fn build_pkg_command(name: &str, resolved: Option<PathBuf>) -> Command {
    #[cfg(windows)]
    {
        let argv = windows_pkg_argv(name, resolved.as_deref());
        let mut it = argv.into_iter();
        let prog = it.next().unwrap_or_else(|| name.to_string());
        let mut cmd = Command::new(prog);
        cmd.args(it);
        cmd
    }
    #[cfg(not(windows))]
    {
        Command::new(resolved.unwrap_or_else(|| PathBuf::from(name)))
    }
}

/// Build a `Command` for `name`, using `resolver` for the binary path and
/// falling back to a plain `Command::new(name)` when `resolver` returns `None`.
/// Honors the `CFGD_<NAME>_BIN` env-var seam first, short-circuiting the
/// resolver entirely (tests don't want resolver-side filesystem checks
/// running). On Windows the resolved path is invoked shim-aware so `.cmd`/`.ps1`
/// managers (scoop, npm) actually run. Mirrors the `X_cmd()` pattern that
/// cargo/pipx/go had open-coded.
pub(super) fn tool_cmd_with_resolver<F>(name: &str, resolver: F) -> Command
where
    F: FnOnce() -> Option<PathBuf>,
{
    if let Ok(custom) = std::env::var(tool_seam_var(name)) {
        return Command::new(custom);
    }
    build_pkg_command(name, resolver())
}

/// Extract caveats/warnings from package manager output.
pub(super) fn extract_caveats(manager: &str, output: &CommandOutput) -> Vec<ActionNote> {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let mut notes = Vec::new();

    match manager {
        "brew" | "brew-cask" => {
            // Homebrew prints "==> Caveats" followed by caveat text until next "==> " or end
            let mut in_caveats = false;
            let mut caveat_lines = Vec::new();
            for line in combined.lines() {
                if line.starts_with("==> Caveats") {
                    in_caveats = true;
                    caveat_lines.clear();
                    continue;
                }
                if in_caveats {
                    if line.starts_with("==> ") {
                        if !caveat_lines.is_empty() {
                            notes.push(ActionNote::warn(manager, caveat_lines.join("\n").trim()));
                        }
                        in_caveats = false;
                    } else {
                        caveat_lines.push(line.to_string());
                    }
                }
            }
            if in_caveats && !caveat_lines.is_empty() {
                notes.push(ActionNote::warn(manager, caveat_lines.join("\n").trim()));
            }
        }
        "npm" | "pnpm" => {
            for line in combined.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("npm warn") || trimmed.starts_with("npm WARN") {
                    notes.push(ActionNote::warn(manager, trimmed));
                }
            }
        }
        "pip" | "pipx" => {
            for line in combined.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("WARNING:") {
                    notes.push(ActionNote::warn(manager, trimmed));
                }
            }
        }
        _ => {
            // Generic: capture any line containing warning/caveat/note from stderr
            for line in output.stderr.lines() {
                let trimmed = line.trim();
                let lower = trimmed.to_lowercase();
                if lower.contains("warning:") || lower.contains("caveat") || lower.contains("note:")
                {
                    notes.push(ActionNote::warn(manager, trimmed));
                }
            }
        }
    }
    notes
}

/// Run a command, mapping IO errors to PackageError::CommandFailed and non-zero
/// exit to the appropriate PackageError variant based on `error_kind`.
/// `error_kind` should be one of: "install", "uninstall", "list", "update".
/// For "list", returns ListFailed. For "update", returns InstallFailed (matching
/// existing convention). An optional `msg_prefix` is prepended to the error message.
pub(super) fn run_pkg_cmd(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
) -> std::result::Result<Output, PackageError> {
    run_pkg_cmd_prefixed(manager, cmd, error_kind, None)
}

/// Like `run_pkg_cmd` but prepends a custom prefix to the error message.
pub(super) fn run_pkg_cmd_msg(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
    msg_prefix: &str,
) -> std::result::Result<Output, PackageError> {
    run_pkg_cmd_prefixed(manager, cmd, error_kind, Some(msg_prefix))
}

/// Timeout for package manager operations (10 minutes — installs can be slow).
const PKG_CMD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

fn run_pkg_cmd_prefixed(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
    msg_prefix: Option<&str>,
) -> std::result::Result<Output, PackageError> {
    hand_child_bootstrapped_path(cmd);
    // Ensure stdout/stderr are captured for timeout-based execution
    let output = cfgd_core::command_output_with_timeout(cmd, PKG_CMD_TIMEOUT).map_err(|e| {
        PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        }
    })?;
    if !output.status.success() {
        let stderr = cfgd_core::stderr_lossy_trimmed(&output);
        let message = match msg_prefix {
            Some(prefix) if !prefix.is_empty() => format!("{}: {}", prefix, stderr),
            _ => stderr,
        };
        return Err(match error_kind {
            "install" => PackageError::InstallFailed {
                manager: manager.into(),
                message,
            },
            "uninstall" => PackageError::UninstallFailed {
                manager: manager.into(),
                message,
            },
            "list" => PackageError::ListFailed {
                manager: manager.into(),
                message,
            },
            _ => PackageError::InstallFailed {
                manager: manager.into(),
                message,
            },
        });
    }
    Ok(output)
}

/// Run a package-manager *query* command (e.g. `export`, `info`) with the standard
/// timeout, returning its captured output REGARDLESS of exit status. Some managers
/// exit non-zero for a benign result — scoop's `export`/`list` returns 1 when no
/// apps are installed and `info` returns non-zero for an unknown package — so the
/// caller parses stdout and decides what the result means; only a spawn/timeout
/// failure is surfaced as `CommandFailed`. Contrast with
/// `run_pkg_cmd`, which treats any non-zero exit as an error (correct for mutating
/// install/uninstall commands, wrong for a read whose exit code is unreliable).
pub(super) fn run_pkg_query(
    manager: &str,
    cmd: &mut Command,
) -> std::result::Result<Output, PackageError> {
    hand_child_bootstrapped_path(cmd);
    cfgd_core::command_output_with_timeout(cmd, PKG_CMD_TIMEOUT).map_err(|e| {
        PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        }
    })
}

/// Report the failure of a bootstrap step the chain is about to abandon.
///
/// A fallback chain tries methods until one works, so every step but the last
/// has its exit status inspected and discarded — and the error the chain
/// finally returns names only the method it stopped on. With the window
/// collapsing silently under a caller-owned status, this is the one place the
/// abandoned step's diagnostic survives.
pub(super) fn report_abandoned_step(
    cx: &PackageContext<'_>,
    manager: &str,
    method: &str,
    output: &CommandOutput,
) {
    cx.report(
        Role::Warn,
        manager,
        format!(
            "{method} could not install {manager} ({}); trying the next method",
            command_failure_reason(output)
        ),
    );
}

/// One line naming why a package command failed: its exit code, plus the stderr
/// it produced when there is any.
///
/// The `CommandOutput` counterpart of `collapse_to_subject_line`, and the only
/// place that shape is built. Two callers need it and neither may print a raw
/// tail of its own: `run_pkg_cmd_live`, whose error IS the caller's status
/// detail (a downstream branch also matches on its substring — snap's
/// classic-confinement retry), and a fallback chain that inspects the exit
/// status itself and reports the step it is about to abandon. Collapsed,
/// because both destinations are a single status subject/detail and
/// `Renderer::write_line` debug-asserts on an embedded newline. The exit code
/// stays in the message so an operator can still tell "unknown failure" from a
/// tool that exited non-zero saying nothing.
pub(super) fn command_failure_reason(output: &CommandOutput) -> String {
    let reason = cfgd_core::exit_status_reason(&output.status);
    let stderr = collapse_to_subject_line(output.stderr.trim());
    if stderr.is_empty() {
        reason
    } else {
        format!("{reason}: {stderr}")
    }
}

/// Run `cmd` through a live output window, letting the CONTEXT decide whether
/// that window settles a status line of its own.
///
/// The one entry point for a package manager's shell-outs. Under the
/// reconciler the action already has exactly one status line, built from the
/// plan and carrying the phase's alignment column, so a window that settled its
/// own would render the same install twice; standalone (`cfgd doctor`, a manual
/// bootstrap) the window IS the only line and must settle.
///
/// A concurrent install lane is checked first: its window already exists,
/// created by the coordinator at the action's depth, and `Printer::run` would
/// open a second one at the ambient depth — which in a concurrent phase is
/// whatever the last renderer state happened to be, shared by every lane.
pub(super) fn pkg_run(
    cx: &PackageContext<'_>,
    cmd: &mut Command,
    label: impl Into<String>,
) -> std::io::Result<CommandOutput> {
    hand_child_bootstrapped_path(cmd);
    if let Some(lane) = cx.lane() {
        lane.run(cmd)
    } else if cx.caller_owns_status {
        cx.printer.run_silent(cmd, label)
    } else {
        cx.printer.run(cmd, label)
    }
}

/// Give a package-manager child the PATH directories cfgd bootstrapped during
/// this run.
///
/// Resolving the manager's own binary through the registry
/// (`cfgd_core::command_path`) is not enough: npm shells out to `node` and
/// `git`, pipx to a Python, and those grandchildren resolve through the PATH
/// they inherit — which is the one cfgd started with, and which cannot name a
/// prefix that did not exist then. Lifecycle scripts already get this treatment
/// (`reconciler::scripts`), and both compose the string the same way.
///
/// A PATH the command builder set deliberately is left alone: `brew_cmd`'s
/// augmented PATH is how brew finds its own tools, and overwriting it would
/// undo a decision made with more context than this has.
///
/// Every spawn wrapper in this module calls it — `pkg_run`, `run_pkg_cmd*` and
/// `run_pkg_query` alike. A manager's install path reaches all three (npm's
/// `install` asks `npm config get prefix` through `run_pkg_query` before it
/// builds the install command), so augmenting one of them leaves the same
/// availability/spawn disagreement live one call earlier.
fn hand_child_bootstrapped_path(cmd: &mut Command) {
    let dirs = cfgd_core::bootstrapped_path_dirs();
    if dirs.is_empty()
        || cmd
            .get_envs()
            .any(|(key, _)| key.eq_ignore_ascii_case("PATH"))
    {
        return;
    }
    // Through the core reader, which brackets the process `PATH` read with the
    // same guard every other production reader takes: the read happens BEFORE
    // the guarded spawn, so an unsynchronized one re-opens the window the lock
    // exists to close.
    if let Some(joined) = cfgd_core::process_path_with_dirs_prepended(&dirs) {
        cmd.env("PATH", joined);
    }
}

/// Run a package manager command with live progress display via Printer.
/// Use for long-running operations (install, uninstall, update, bootstrap).
/// Maps spawn errors to `PackageError::CommandFailed` and non-zero exit to
/// the appropriate variant based on `error_kind`.
pub(super) fn run_pkg_cmd_live(
    cx: &PackageContext<'_>,
    manager: &str,
    cmd: &mut Command,
    label: &str,
    error_kind: &str,
) -> std::result::Result<CommandOutput, PackageError> {
    let output = pkg_run(cx, cmd, label).map_err(|e| PackageError::CommandFailed {
        manager: manager.into(),
        source: e,
    })?;
    if !output.status.success() {
        let message = command_failure_reason(&output);
        return Err(match error_kind {
            "install" => PackageError::InstallFailed {
                manager: manager.into(),
                message,
            },
            "uninstall" => PackageError::UninstallFailed {
                manager: manager.into(),
                message,
            },
            _ => PackageError::InstallFailed {
                manager: manager.into(),
                message,
            },
        });
    }
    // Post-install caveats travel back to the reconciler instead of printing
    // here: `run_pkg_cmd_live` returns before the action's own status line is
    // emitted, so anything printed from inside it lands above the line it
    // belongs to.
    if error_kind == "install" {
        for note in extract_caveats(manager, &output) {
            cx.notes.push(note);
        }
    }
    Ok(output)
}

/// Install `packages` as a single batch; if the batch fails, retry each package
/// on its own so one bad spec (e.g. a name that isn't a real formula) doesn't
/// block the valid ones. `build_cmd` constructs the install `Command` for a
/// given package subset, so the caller controls the exact argv (formula vs
/// `--cask`, extra flags, etc.).
///
/// Returns `Ok(())` when everything installs. When some packages still fail
/// after the per-package retry, the valid ones remain installed and the error
/// names exactly the packages that failed. A single-package batch is not
/// retried — there is nothing to isolate, so its original error is surfaced
/// verbatim.
pub(super) fn install_batch_then_per_package<F>(
    cx: &PackageContext<'_>,
    manager: &str,
    packages: &[String],
    build_cmd: F,
) -> std::result::Result<(), PackageError>
where
    F: Fn(&[String]) -> Command,
{
    if packages.is_empty() {
        return Ok(());
    }

    let batch_label = format!("{} install {}", manager, packages.join(" "));
    let mut batch = build_cmd(packages);
    match run_pkg_cmd_live(cx, manager, &mut batch, &batch_label, "install") {
        Ok(_) => return Ok(()),
        Err(e) => {
            if packages.len() == 1 {
                return Err(e);
            }
            // The batch failure is the reason the retry is happening AND the
            // only place its diagnostic survives: a caller-owned window
            // collapses without printing, and the error below names the
            // packages that failed on their own, not why the batch did.
            cx.report(
                Role::Warn,
                manager,
                format!(
                    "batch install failed; retrying each package individually: {}",
                    collapse_to_subject_line(&e)
                ),
            );
        }
    }

    // Each retry's cause travels into the returned error. Dropping it left the
    // caller's one status line reading `failed to install: a, b` with nothing
    // to act on, because the window that saw the stderr settled silently.
    let mut failed: Vec<String> = Vec::new();
    for pkg in packages {
        let label = format!("{} install {}", manager, pkg);
        let mut cmd = build_cmd(std::slice::from_ref(pkg));
        if let Err(e) = run_pkg_cmd_live(cx, manager, &mut cmd, &label, "install") {
            failed.push(format!("{} ({})", pkg, collapse_to_subject_line(&e)));
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(PackageError::InstallFailed {
            manager: manager.into(),
            message: format!("failed to install: {}", failed.join("; ")),
        })
    }
}

const LINUXBREW_PATH: &str = "/home/linuxbrew/.linuxbrew/bin/brew";

/// Env-var seam for the `brew` binary path. Production reads no env var.
/// Tests set this to a `cfgd_core::test_helpers::ToolShim` script path,
/// short-circuiting the linuxbrew detection logic so install/uninstall/etc
/// flows can be exercised without a real Homebrew installation.
const BREW_BIN_ENV: &str = "CFGD_BREW_BIN";

/// Check if brew is available, including linuxbrew fallback on Linux.
/// Honors `CFGD_BREW_BIN` for tests.
pub(super) fn brew_available() -> bool {
    if std::env::var(BREW_BIN_ENV).is_ok_and(|v| std::path::Path::new(&v).is_file()) {
        return true;
    }
    if command_available("brew") {
        return true;
    }
    cfg!(target_os = "linux") && std::path::Path::new(LINUXBREW_PATH).exists()
}

/// A system manager a bootstrap cascade can mediate through, as
/// `(plan method, command)`.
///
/// The plan names the MANAGER — `apt` is what the user reads on the line and
/// what the concurrency lane is keyed on — while the binary a bootstrap spawns
/// is `apt-get`. Both halves live in one table so a method and the arm it
/// authorizes cannot drift apart, and so the detector that PICKS a method
/// probes exactly the command that will run it. That pairing is what makes a
/// planned method safe to treat as binding: a plan can only name a mediator
/// execution can spawn.
type SystemArm = (&'static str, &'static str);

/// The system arms of [`bootstrap_via_brew_then_system`].
const BREW_SYSTEM_ARMS: &[SystemArm] = &[("apt", "apt-get"), ("dnf", "dnf")];

/// The arms of [`bootstrap_via_system_manager`], which reaches one manager more
/// than the brew cascade does.
const SYSTEM_MANAGER_ARMS: &[SystemArm] =
    &[("apt", "apt-get"), ("dnf", "dnf"), ("zypper", "zypper")];

/// The first arm of `arms` this host can actually run, or `None`.
fn detect_system_arm(arms: &[SystemArm]) -> Option<&'static str> {
    arms.iter()
        .find(|(_, tool)| system_tool_available(tool))
        .map(|(method, _)| *method)
}

/// Which manager a brew→apt→dnf cascade would pick, or `fallback` when none of
/// them is available. The name a `BootstrapPlan` carries as its method.
///
/// The answer is BINDING on execution, not a preview of it: the plan line the
/// user reads names this mediator and the action is serialized on this
/// mediator's concurrency lane, so `bootstrap` runs this arm alone (see
/// [`PackageContext::planned_method`] and [`bootstrap_via_brew_then_system`])
/// and fails rather than substituting a manager that became available after
/// planning.
///
/// `fallback` must be the caller's OWN bootstrap arm (npm's `nvm`, pipx's
/// `pip`) — the same string it hands the cascade — because a method naming
/// neither this cascade nor that arm is a provision nothing can run.
pub(super) fn detect_brew_system_method(fallback: &'static str) -> &'static str {
    detect_brew_or_system_method(BREW_SYSTEM_ARMS).unwrap_or(fallback)
}

/// The mediator a brew-then-system bootstrap can actually run on this host, or
/// `None` when none is present.
///
/// The strict counterpart of [`detect_brew_system_method`], for a manager with
/// no bootstrap arm of its own: naming a mediator the host cannot run used to
/// degrade into a cascade that tried something else, and under a binding plan
/// it would be a guaranteed failure instead.
pub(super) fn detect_brew_or_system_method(arms: &[SystemArm]) -> Option<&'static str> {
    if brew_available() {
        return Some("brew");
    }
    detect_system_arm(arms)
}

/// Which manager an apt→dnf→zypper cascade can run here, or `None` when none of
/// them is present. Linux-only, like the two managers whose plans resolve their
/// method through it. Binding on execution for the same reason
/// [`detect_brew_system_method`] is.
#[cfg(target_os = "linux")]
pub(super) fn detect_system_method() -> Option<&'static str> {
    detect_system_arm(SYSTEM_MANAGER_ARMS)
}

/// Every mediator a `go` bootstrap can run: brew, then the full system cascade
/// (`bootstrap_via_system_manager`, which reaches zypper as well).
pub(super) fn detect_go_bootstrap_method() -> Option<&'static str> {
    detect_brew_or_system_method(SYSTEM_MANAGER_ARMS)
}

/// The plan named a mediator that cannot deliver on this host any more.
///
/// Never a fall-through: substituting whatever else is available would run the
/// install outside the lane the action was serialized on, and would contradict
/// the line the user approved.
pub(super) fn planned_method_unavailable(manager: &str, method: &str) -> PackageError {
    PackageError::BootstrapFailed {
        manager: manager.into(),
        message: format!(
            "the plan installs {manager} via {method}, which is not available on this host; re-run to re-plan"
        ),
    }
}

/// The mediator the plan named ran and failed. Its diagnostic travels in the
/// error rather than in a note, because there is no next method to narrate
/// toward and a caller-owned window settles no line of its own.
pub(super) fn planned_method_failed(
    manager: &str,
    method: &str,
    output: &CommandOutput,
) -> PackageError {
    PackageError::BootstrapFailed {
        manager: manager.into(),
        message: format!(
            "{method} could not install {manager}: {}",
            command_failure_reason(output)
        ),
    }
}

/// A `~`-relative directory an installer creates, or `None` when no home
/// resolves — a literal `~` handed to a PATH entry names nothing, so a
/// bootstrap plan declares no directory rather than an unusable one.
pub(super) fn home_relative_dir(rel: &str) -> Option<std::path::PathBuf> {
    let rel = std::path::Path::new(rel);
    let expanded = cfgd_core::expand_tilde(rel);
    (expanded != rel).then_some(expanded)
}

/// The directory `pip install --user <tool>` writes console scripts into, or
/// `None` when it cannot be named.
///
/// Unix hands every interpreter the same `~/.local/bin`. Windows does not: the
/// scripts land under roaming AppData in a directory carrying the interpreter's
/// OWN version (`%APPDATA%\Python\Python314\Scripts`, CPython's `nt_user`
/// install scheme), so the answer has to come from the pip that will run the
/// install. When the version cannot be read, the plan declares nothing rather
/// than a guess — a declared directory reaches the generated env file, where a
/// wrong one is worse than a missing one.
pub(super) fn pip_user_scripts_dir(pip_tool: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        windows_pip_user_scripts_dir(pip_tool)
    } else {
        home_relative_dir("~/.local/bin")
    }
}

/// The Windows arm of [`pip_user_scripts_dir`], split out so the composition it
/// performs is reachable from a test on any host.
fn windows_pip_user_scripts_dir(pip_tool: &str) -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    compose_pip_user_scripts_dir(&appdata, &pip_python_version(pip_tool)?)
}

/// Join the roaming AppData root and a dotted `X.Y` interpreter version into the
/// `nt_user` scripts directory. The version loses its dots there, so `3.14`
/// names `Python314`.
pub(super) fn compose_pip_user_scripts_dir(appdata: &str, python_version: &str) -> Option<PathBuf> {
    let mut parts = python_version.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if appdata.is_empty() || !numeric(major) || !numeric(minor) {
        return None;
    }
    Some(
        PathBuf::from(appdata)
            .join("Python")
            .join(format!("Python{major}{minor}"))
            .join("Scripts"),
    )
}

/// The interpreter version a given pip belongs to, read from its `--version`
/// banner.
///
/// Only a SUCCESSFUL answer is cached, for the process: a plan is derived once
/// per manager on every planning run and again on every doctor pass, and the
/// interpreter behind a resolved pip cannot change while cfgd runs, so the probe
/// is worth exactly one spawn. A failure is deliberately not remembered — a test
/// that empties `PATH` for its own assertion would otherwise pin every later
/// plan in the binary to a dir-less answer it never asked for.
fn pip_python_version(pip_tool: &str) -> Option<String> {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(cached) = VERSION.get() {
        return Some(cached.clone());
    }
    let mut cmd = tool_cmd_with_resolver(pip_tool, || resolve_tool_with_fallbacks(pip_tool, &[]));
    cmd.arg("--version");
    let out = cfgd_core::command_output_with_timeout(&mut cmd, cfgd_core::COMMAND_TIMEOUT).ok()?;
    let probed = parse_pip_python_version(&cfgd_core::stdout_lossy_trimmed(&out))?;
    Some(VERSION.get_or_init(|| probed).clone())
}

/// The `X.Y` inside the `(python X.Y)` tail of a `pip --version` banner
/// (`pip 25.2 from C:\Python314\Lib\site-packages\pip (python 3.14)`).
pub(super) fn parse_pip_python_version(banner: &str) -> Option<String> {
    const MARKER: &str = "(python ";
    let rest = &banner[banner.rfind(MARKER)? + MARKER.len()..];
    let end = rest.find(')')?;
    let version = rest[..end].trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// Return the brew bin/sbin directories for the current platform.
/// Mirrors `BrewManager::path_dirs`; kept here so `path_with_brew` doesn't need
/// to depend on the brew submodule.
pub(super) fn brew_path_dirs() -> Vec<String> {
    if cfg!(target_os = "linux") {
        vec![
            "/home/linuxbrew/.linuxbrew/bin".to_string(),
            "/home/linuxbrew/.linuxbrew/sbin".to_string(),
        ]
    } else if cfg!(target_os = "macos") {
        // Apple Silicon vs Intel
        if std::path::Path::new("/opt/homebrew/bin").exists() {
            vec![
                "/opt/homebrew/bin".to_string(),
                "/opt/homebrew/sbin".to_string(),
            ]
        } else {
            vec!["/usr/local/bin".to_string(), "/usr/local/sbin".to_string()]
        }
    } else {
        Vec::new()
    }
}

/// After brew bootstrap, add brew's bin directories to the current process PATH
/// so that brew-installed binaries (and post-apply scripts that use them) work
/// immediately without requiring a new shell session.
/// Build a PATH string that includes brew's bin directories.
fn path_with_brew() -> Option<String> {
    let dirs = brew_path_dirs();
    if dirs.is_empty() {
        return None;
    }

    if let Ok(current_path) = std::env::var("PATH")
        && !current_path.contains(&dirs[0])
    {
        let prefix = dirs.join(":");
        return Some(format!("{}:{}", prefix, current_path));
    }
    None
}

/// The brew-augmented PATH, cached at first call.
pub(super) fn brew_path() -> Option<&'static str> {
    use std::sync::OnceLock;
    static BREW_PATH: OnceLock<Option<String>> = OnceLock::new();
    BREW_PATH.get_or_init(path_with_brew).as_deref()
}

/// Build a Command for brew, handling linuxbrew paths.
/// On Linux as root, detects the owner of the brew installation and runs via
/// `sudo -u <owner>` since brew refuses to run as root.
/// On Linux as non-root, uses LINUXBREW_PATH directly if brew is not in PATH.
///
/// Honors `CFGD_BREW_BIN` for tests: when set, short-circuits all detection
/// and runs the shim directly. The shim is responsible for any sudo / PATH
/// setup the test cares about.
pub(super) fn brew_cmd() -> Command {
    if let Ok(custom) = std::env::var(BREW_BIN_ENV) {
        return Command::new(custom);
    }
    if cfg!(target_os = "linux") && std::path::Path::new(LINUXBREW_PATH).exists() {
        if cfgd_core::is_root() {
            if let Some(owner) = brew_owner() {
                let mut cmd = Command::new("sudo");
                cmd.args(["-u", &owner, LINUXBREW_PATH]);
                // cwd must be readable by the brew user — /root is 700
                cmd.current_dir("/tmp");
                return cmd;
            }
            let mut cmd = Command::new(LINUXBREW_PATH);
            cmd.current_dir("/tmp");
            return cmd;
        }
        if !command_available("brew") {
            return Command::new(LINUXBREW_PATH);
        }
    }
    let mut cmd = Command::new("brew");
    // Augment PATH for brew lookups without modifying the global environment
    if let Some(augmented_path) = brew_path() {
        cmd.env("PATH", augmented_path);
    }
    cmd
}

/// Detect the user who owns the brew installation.
fn brew_owner() -> Option<String> {
    let output = Command::new("stat")
        .args(["-c", "%U", LINUXBREW_PATH])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let owner = cfgd_core::stdout_lossy_trimmed(&output);
    if owner.is_empty() || owner == "root" {
        None
    } else {
        Some(owner)
    }
}

/// Run the brew arm of a bootstrap cascade, honoring the method the plan chose.
///
/// `Ok(true)` — brew installed `brew_pkg`. `Ok(false)` — the caller continues
/// to its next arm, because brew is absent or the plan named a different
/// mediator. `Err` — the plan named brew and brew could not deliver, which is
/// the end of the attempt rather than the start of a fallback.
pub(super) fn bootstrap_brew_arm(
    cx: &PackageContext<'_>,
    manager_name: &str,
    brew_pkg: &str,
) -> Result<bool> {
    let planned = cx.planned_method();
    if planned.is_some_and(|method| method != "brew") {
        return Ok(false);
    }
    if !brew_available() {
        return match planned {
            Some(method) => Err(planned_method_unavailable(manager_name, method).into()),
            None => Ok(false),
        };
    }
    let result = pkg_run(
        cx,
        brew_cmd().args(["install", brew_pkg]),
        format!("Installing {} via brew", brew_pkg),
    )
    .map_err(|e| PackageError::BootstrapFailed {
        manager: manager_name.into(),
        message: format!("brew install {} failed: {}", brew_pkg, e),
    })?;
    if result.status.success() {
        return Ok(true);
    }
    match planned {
        Some(method) => Err(planned_method_failed(manager_name, method, &result).into()),
        None => {
            report_abandoned_step(cx, manager_name, "brew", &result);
            Ok(false)
        }
    }
}

/// Install `pkgs` from the first of `arms` this host can run — or, when the
/// plan chose one of them, from that one alone.
///
/// `subject` is what the live window says is being installed. `fallback_method`
/// is the caller's OWN bootstrap arm, the one thing this cascade may decline
/// toward: `Ok(false)` under a planned method means the plan named exactly that
/// string, never merely "a method this cascade does not recognize". A method
/// that is neither an arm here nor the caller's own arm is a provision nothing
/// can run, and fails naming itself rather than being answered by whatever the
/// caller happens to try next.
fn bootstrap_system_arms(
    cx: &PackageContext<'_>,
    manager_name: &str,
    subject: &str,
    pkgs: &[&str],
    arms: &[SystemArm],
    fallback_method: Option<&str>,
) -> Result<bool> {
    if let Some(method) = cx.planned_method() {
        if fallback_method == Some(method) {
            return Ok(false);
        }
        let Some((_, tool)) = arms.iter().find(|(arm, _)| *arm == method) else {
            return Err(planned_method_unavailable(manager_name, method).into());
        };
        if !system_tool_available(tool) {
            return Err(planned_method_unavailable(manager_name, method).into());
        }
        let result = run_system_install(cx, manager_name, subject, pkgs, method, tool)?;
        return if result.status.success() {
            Ok(true)
        } else {
            Err(planned_method_failed(manager_name, method, &result).into())
        };
    }

    for (method, tool) in arms {
        if system_tool_available(tool) {
            let result = run_system_install(cx, manager_name, subject, pkgs, method, tool)?;
            if result.status.success() {
                return Ok(true);
            }
            report_abandoned_step(cx, manager_name, tool, &result);
        }
    }
    Ok(false)
}

/// Probe a system manager through the same `CFGD_<NAME>_BIN` seam
/// [`sudo_cmd_with_seam`] honors — a seam-shimmed tool must look available on
/// hosts that lack the real binary (see `require_tool_with_seam`'s pairing
/// note), or the probe answers from `$PATH` while the spawn answers from the
/// seam.
fn system_tool_available(tool: &str) -> bool {
    cfgd_core::command_available_with_seam(&tool_seam_var(tool), tool)
}

/// Run one system arm's install. The window's label names the COMMAND that is
/// running (`apt-get`), while a failure names the METHOD (`apt`) — the manager
/// the plan line, the concurrency lane and every other binding failure use.
fn run_system_install(
    cx: &PackageContext<'_>,
    manager_name: &str,
    subject: &str,
    pkgs: &[&str],
    method: &str,
    tool: &str,
) -> Result<CommandOutput> {
    pkg_run(
        cx,
        sudo_cmd_with_seam(tool).args(["install", "-y"]).args(pkgs),
        format!("Installing {} via {}", subject, tool),
    )
    .map_err(|e| {
        PackageError::BootstrapFailed {
            manager: manager_name.into(),
            message: format!("{} install failed: {}", method, e),
        }
        .into()
    })
}

/// Try to install a package via common system package managers (apt, then dnf, then zypper).
/// Returns `Ok(())` on first success, or a `BootstrapFailed` error if all attempts fail.
///
/// There is no fallback arm past this one: a caller reaching here has nothing
/// else to try, so a planned method these arms cannot run fails naming itself.
pub(super) fn bootstrap_via_system_manager(
    cx: &PackageContext<'_>,
    target_pkg: &str,
    manager_name: &str,
) -> Result<()> {
    if bootstrap_system_arms(
        cx,
        manager_name,
        target_pkg,
        &[target_pkg],
        SYSTEM_MANAGER_ARMS,
        None,
    )? {
        return Ok(());
    }
    Err(PackageError::BootstrapFailed {
        manager: manager_name.into(),
        message: format!("failed to install {} via apt, dnf, or zypper", target_pkg),
    }
    .into())
}

/// Try to install packages via brew first, then fall back to system package managers.
/// `brew_pkg` is the brew formula name, `system_pkgs` are the system package names.
/// Returns `Ok(true)` if installed, `Ok(false)` if no method succeeded (caller should
/// try alternative), or `Err` on command execution failure.
///
/// When the context carries a planned method ([`PackageContext::planned_method`])
/// exactly one arm runs: the one the plan named. `Ok(false)` then means the plan
/// named `fallback_method` — the caller's own arm, which it runs next — never
/// that this cascade tried and gave up. `fallback_method` must be the same
/// string the caller passed `detect_brew_system_method`.
pub(super) fn bootstrap_via_brew_then_system(
    cx: &PackageContext<'_>,
    manager_name: &str,
    brew_pkg: &str,
    system_pkgs: &[&str],
    fallback_method: &str,
) -> Result<bool> {
    if bootstrap_brew_arm(cx, manager_name, brew_pkg)? {
        return Ok(true);
    }
    bootstrap_system_arms(
        cx,
        manager_name,
        manager_name,
        system_pkgs,
        BREW_SYSTEM_ARMS,
        Some(fallback_method),
    )
}

/// Run a `sh -c <script>` install pipeline and surface non-zero exits as
/// `PackageError::BootstrapFailed`. Used by managers that bootstrap via a
/// vendor-supplied shell-pipe installer (rustup, nix, get-pip, etc.).
///
/// The outer shell is POSIX `sh`, not `bash`: FreeBSD base and minimal
/// containers ship only `/bin/sh`, and every caller's pipeline is POSIX-clean
/// (e.g. `curl … | sh -s`). A manager whose bootstrap genuinely needs bash
/// (npm's nvm path) invokes `bash` inside its own script string rather than
/// relying on this helper's outer interpreter.
pub(super) fn bootstrap_via_shell_script(
    cx: &PackageContext<'_>,
    manager_name: &str,
    label: impl Into<String>,
    script: &str,
) -> Result<()> {
    let result = pkg_run(cx, Command::new("sh").arg("-c").arg(script), label).map_err(|e| {
        PackageError::BootstrapFailed {
            manager: manager_name.into(),
            message: format!("{manager_name} install failed: {e}"),
        }
    })?;
    if !result.status.success() {
        return Err(PackageError::BootstrapFailed {
            manager: manager_name.into(),
            message: format!(
                "{manager_name} install script failed: {}",
                command_failure_reason(&result)
            ),
        }
        .into());
    }
    Ok(())
}

/// Strip trailing "-VERSION" from package names where version starts with a digit.
/// Used by apk, pkg, and nix-env which output "name-version" format.
pub(super) fn strip_version_suffix(name: &str) -> String {
    let bytes = name.as_bytes();
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            return name[..i].to_string();
        }
    }
    name.to_string()
}

/// Strip architecture suffix (e.g., ".x86_64", ".noarch") from package names.
/// Used by dnf and yum which output "name.arch" format.
pub(super) fn strip_arch_suffix(name: &str) -> String {
    name.rsplit_once('.').map_or(name, |(n, _)| n).to_string()
}

/// Strip leading `"sudo"` from a command slice when the wrapper would be
/// redundant or harmful: already running as root, or the wrapped tool's
/// `CFGD_<NAME>_BIN` seam is set (the test shim runs as the test user, and
/// routing through the real sudo would bypass the seam — same rationale as
/// [`sudo_cmd_with_seam`]). Returns the effective command slice.
pub(super) fn strip_sudo_for_exec<'a>(cmd: &'a [&'a str]) -> &'a [&'a str] {
    if cmd.first() == Some(&"sudo") {
        if cfgd_core::is_root() {
            return &cmd[1..];
        }
        if let Some(tool) = cmd.get(1)
            && std::env::var(tool_seam_var(tool)).is_ok()
        {
            return &cmd[1..];
        }
    }
    cmd
}

/// Build a Command that prepends `sudo` only when not already running as root.
pub(super) fn sudo_cmd(program: &str) -> Command {
    if cfgd_core::is_root() {
        Command::new(program)
    } else {
        let mut cmd = Command::new("sudo");
        cmd.arg(program);
        cmd
    }
}

/// Build a Command for `program`, honoring the `CFGD_<NAME>_BIN` env-var seam
/// the same way [`tool_cmd_with_resolver`] does, but for tools that normally
/// require `sudo`. When the seam is set, returns a direct
/// `Command::new(<seam path>)` (skipping the sudo wrapper entirely — the test
/// shim already runs as the test user). When the seam is unset, falls back
/// to [`sudo_cmd`].
pub(super) fn sudo_cmd_with_seam(program: &str) -> Command {
    if let Ok(custom) = std::env::var(tool_seam_var(program)) {
        let p = PathBuf::from(custom);
        return Command::new(p);
    }
    sudo_cmd(program)
}

/// Parse a "Version: X.Y.Z" line from command output.
/// Used by flatpak, winget, and scoop version queries.
pub(super) fn parse_version_field(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Version:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests;
