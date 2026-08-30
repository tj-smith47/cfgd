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
/// dying with "program not found" (see `windows_pkg_argv`). On non-Windows this
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

/// The leading self-tag a manager stamps on its own advisory lines, stripped
/// before the line becomes a caveat body.
///
/// Every caveat already renders under the `[<manager>]` owner tag
/// `ActionNote::body` composes, so a body that opens with the manager's own
/// word says it twice: `⚠ [cargo] warning: be sure to add …`. Matched
/// case-insensitively and only at the START — a marker in the middle of a
/// sentence is part of what the manager is saying.
const CAVEAT_SELF_TAGS: &[&str] = &["warning:", "warn:", "caveat:", "caveats:", "note:"];

/// One advisory line, with the manager's own tag taken off the front.
fn strip_caveat_self_tag(line: &str) -> &str {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    for tag in CAVEAT_SELF_TAGS {
        if let Some(rest) = lower.strip_prefix(tag) {
            return trimmed[trimmed.len() - rest.len()..].trim_start();
        }
    }
    trimmed
}

/// npm's per-line advisory shape: `npm warn <code> <text>`, one line per
/// physical line of ONE message.
///
/// Returns `(code, text)`. npm repeats the prefix and the code on every line it
/// wraps, so four lines of a single `install-scripts` advisory arrived as four
/// caveats — one of them the blank spacer line npm puts in the middle, which
/// rendered as a bare `⚠ [npm] npm warn install-scripts` with nothing after it.
fn npm_warn_parts(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_end();
    let rest = trimmed
        .trim_start()
        .strip_prefix("npm warn ")
        .or_else(|| trimmed.trim_start().strip_prefix("npm WARN "))?;
    let rest = rest.trim_start();
    match rest.split_once(char::is_whitespace) {
        Some((code, text)) => Some((code, text)),
        // The spacer line: the code alone, with nothing said under it.
        None => Some((rest, "")),
    }
}

/// Extract caveats/warnings from package manager output.
///
/// Every arm yields BODIES; the tail is what turns one into an [`ActionNote`],
/// so the "no empty caveat, ever" rule is stated once instead of per manager. A
/// blank body paints a lone glyph beside an owner tag and tells a reader
/// nothing — see `no_manager_can_emit_an_empty_caveat`.
pub(super) fn extract_caveats(manager: &str, output: &CommandOutput) -> Vec<ActionNote> {
    let brew = cfgd_core::manager_family(manager) == "brew";
    extract_caveat_bodies(manager, output)
        .into_iter()
        .filter(|body| !body.trim().is_empty())
        .map(|body| {
            let body = body.trim_end();
            // A line a tool itself labelled `warning:` / `WARNING:` / `npm
            // warn` keeps that severity. Brew's `==> Caveats` is a SECTION,
            // not a severity — it holds "installed to:" reports beside "run
            // `brew link`" instructions — so a brew body is read for which.
            if brew && !brew_caveat_asks_the_reader_to_act(body) {
                ActionNote::info(manager, body)
            } else {
                ActionNote::warn(manager, body)
            }
        })
        .collect()
}

/// Whether a brew caveat body tells the reader to DO something, rather than
/// reporting where something went. `⚠` means "act on this"; a "here is where
/// it went" note is `◉` whatever section of brew's output it was scraped from.
///
/// Read off the markers brew's own caveat templates use for an instruction —
/// second person (`you`, `your`), an imperative opening (`Add`, `Run`,
/// `Restart`, `Set`, `Source`, `Edit`), a purpose clause (`To start`, `To
/// use`), a service line (`brew services`), or a shell prompt line — none of
/// which a bare "X has been installed to:\n  \<path\>" carries. A body this
/// cannot classify stays a warning: a missed instruction costs the reader a
/// step they had to take, a missed report costs them a glance.
fn brew_caveat_asks_the_reader_to_act(body: &str) -> bool {
    const INSTRUCTION_MARKERS: &[&str] = &[
        "you ",
        "you'",
        "your ",
        "brew services",
        "brew link",
        "to start ",
        "to use ",
        "to enable ",
        "to activate ",
        "to run ",
        "to have ",
        "to load ",
        "need to",
        "needs to",
        "must ",
        "should ",
        "please ",
        "$ ",
        "echo ",
        "export ",
        "source ",
    ];
    const IMPERATIVE_OPENERS: &[&str] = &[
        "add ",
        "run ",
        "restart ",
        "set ",
        "source ",
        "edit ",
        "install ",
        "open ",
        "use ",
        "make sure",
        "ensure ",
        "consider ",
        "see ",
        "enable ",
        "put ",
        "create ",
    ];
    let lower = body.to_lowercase();
    if INSTRUCTION_MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    lower.lines().any(|line| {
        let line = line.trim_start();
        IMPERATIVE_OPENERS.iter().any(|o| line.starts_with(o))
    })
}

fn extract_caveat_bodies(manager: &str, output: &CommandOutput) -> Vec<String> {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let mut notes: Vec<String> = Vec::new();

    // By FAMILY, not by name: `brew-tap` prints the same `==> Caveats` block
    // its formulae and casks do, and matching the two spelled-out names left a
    // tap's caveat to the generic arm, one line at a time.
    match (cfgd_core::manager_family(manager), manager) {
        ("brew", _) => {
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
                            notes.push(caveat_lines.join("\n").trim().to_string());
                        }
                        in_caveats = false;
                    } else {
                        caveat_lines.push(line.to_string());
                    }
                }
            }
            if in_caveats && !caveat_lines.is_empty() {
                notes.push(caveat_lines.join("\n").trim().to_string());
            }
        }
        (_, "npm" | "pnpm") => {
            // One caveat per CODE-run, not per line: npm repeats
            // `npm warn <code>` on every physical line of one message.
            let mut open: Option<(String, Vec<String>)> = None;
            for line in combined.lines() {
                let Some((code, text)) = npm_warn_parts(line) else {
                    continue;
                };
                match &mut open {
                    Some((current, body)) if current == code => body.push(text.to_string()),
                    _ => {
                        if let Some(note) = open.take().map(npm_caveat_body) {
                            notes.push(note);
                        }
                        open = Some((code.to_string(), vec![text.to_string()]));
                    }
                }
            }
            notes.extend(open.map(npm_caveat_body));
        }
        (_, "pip" | "pipx") => {
            for line in combined.lines() {
                let trimmed = line.trim();
                if trimmed.to_ascii_uppercase().starts_with("WARNING:") {
                    notes.push(strip_caveat_self_tag(trimmed).to_string());
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
                    notes.push(strip_caveat_self_tag(trimmed).to_string());
                }
            }
        }
    }
    notes
}

/// `install-scripts: 1 package has …` plus the run's remaining lines, each
/// indented two columns so the message reads as one advisory. The blank spacer
/// lines npm wraps with are dropped: they carry nothing, and each one painted
/// as a caveat of its own — the empty `⚠ [npm]` row beside three real ones.
///
/// A run that says nothing but its code keeps the code: the spacers that
/// carry no text are the ones REPEATING an open code, so a run reduced to
/// nothing is a line npm printed as `npm warn <text>` with no code at all.
fn npm_caveat_body((code, lines): (String, Vec<String>)) -> String {
    let mut body = String::new();
    for line in lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if body.is_empty() {
            body.push_str(&format!("{code}: {line}"));
        } else {
            body.push_str(&format!("\n  {line}"));
        }
    }
    if body.is_empty() {
        return code;
    }
    body
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
            // The reason sits mid-sentence here, inside parentheses: the one
            // destination of `command_failure_reason` that needs its bounded
            // tail on one physical row rather than as continuation lines.
            collapse_to_subject_line(command_failure_reason(output))
        ),
    );
}

/// One line naming why a package command failed: its exit code, plus the stderr
/// it produced when there is any.
///
/// The `CommandOutput` counterpart of `captured_output_detail`, and the ONE
/// place a package manager's stderr becomes a message. Its destinations are a
/// status detail (`run_pkg_cmd_live`, whose error IS the caller's detail — a
/// downstream branch also matches on its substring, snap's classic-confinement
/// retry) and the tail of a bootstrap-failure sentence, both of which the
/// renderer lays out as continuation lines; the one destination that needs a
/// single physical row collapses it itself. Bounded there too: cargo writes its
/// download progress to stderr, so an uncapped fold put forty `Downloaded
/// <crate>` lines in one action row. The exit code stays in the message so an
/// operator can still tell "unknown failure" from a tool that exited non-zero
/// saying nothing, and the full text survives in the journal and in
/// `-o json`.
pub(super) fn command_failure_reason(output: &CommandOutput) -> String {
    let reason = cfgd_core::exit_status_reason(&output.status);
    let stderr = cfgd_core::output::captured_output_detail(output.stderr.trim());
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
/// Every spawn of a manager binary under `packages/` calls it — through
/// `pkg_run`, `run_pkg_cmd*` and `run_pkg_query`, and directly from the two
/// probes that spawn on their own (`tool_version_from`, `pip_python_version`).
/// A manager's install path reaches several of them (npm's `install` asks
/// `npm config get prefix` through `run_pkg_query` before it builds the
/// install command; every `available_version` prices through `run_pkg_query`;
/// a provision's settled row reads `tool_version_from`), so augmenting some
/// and not others leaves the same availability/spawn disagreement live one
/// call earlier — a brew this run had just bootstrapped answered `is_available`
/// and then reported no version, because `brew --version` shells out to a
/// `ruby`/`git` its shim finds through the PATH it inherits. A spawn that is
/// NOT a manager binary (`stat`, `useradd`) says so with `// own-path-ok:` on
/// the line or the line above; `every_manager_spawn_under_packages_inherits_the_bootstrapped_dirs`
/// walks the crate for one that neither routes here nor says why.
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
    // A brew that landed after this process started (a daemon that predates
    // the bootstrap) is not on the inherited PATH; the known prefixes are.
    if cfg!(target_os = "linux") {
        return std::path::Path::new(LINUXBREW_PATH).exists();
    }
    cfg!(target_os = "macos")
        && MACOS_BREW_PREFIXES
            .iter()
            .any(|prefix| brew_prefix_holds_brew(std::path::Path::new(prefix)))
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

/// One manager's mediated bootstrap: the packages a mediating manager installs
/// to deliver it, per mediator family.
///
/// Declared once per manager and read twice — by its `bootstrap`, which hands
/// these lists to the cascade helpers below, and by its
/// [`cfgd_core::providers::PackageManager::mediated_packages`], which answers
/// what a BATCHED provision asks the same mediator to install. Two hand-written
/// copies of the names is how a batch would come to install something the solo
/// bootstrap never does.
pub(super) struct MediatedArms {
    /// The brew formula, or `None` for a manager with no brew arm.
    pub(super) brew: Option<&'static str>,
    /// The package names the system arms install.
    pub(super) system: &'static [&'static str],
    /// Which system arms deliver it — the same table the manager's own
    /// bootstrap cascade walks.
    pub(super) system_arms: &'static [SystemArm],
}

impl MediatedArms {
    /// The packages `via` installs for this manager, or `None` when `via` is
    /// not a mediator these arms describe. Answered on `via`'s FAMILY, so
    /// `brew-cask` reads as brew — the same collapse the provision lane makes.
    pub(super) fn packages_for(&self, via: &str) -> Option<Vec<String>> {
        let family = cfgd_core::manager_family(via);
        if family == "brew" {
            return self.brew.map(|pkg| vec![pkg.to_string()]);
        }
        self.system_arms
            .iter()
            .any(|(arm, _)| *arm == family)
            .then(|| self.system.iter().map(|p| (*p).to_string()).collect())
    }
}

/// The arms of a manager whose bootstrap runs [`bootstrap_via_brew_then_system`].
pub(super) const fn brew_then_system_arms(
    brew: &'static str,
    system: &'static [&'static str],
) -> MediatedArms {
    MediatedArms {
        brew: Some(brew),
        system,
        system_arms: BREW_SYSTEM_ARMS,
    }
}

/// The arms of a manager whose bootstrap runs [`bootstrap_via_system_manager`],
/// optionally after a brew arm of its own.
pub(super) const fn system_manager_arms(
    brew: Option<&'static str>,
    system: &'static [&'static str],
) -> MediatedArms {
    MediatedArms {
        brew,
        system,
        system_arms: SYSTEM_MANAGER_ARMS,
    }
}

/// The first arm of `arms` the run can actually use, or `None`: one the run
/// delivers first, else one this host already has.
///
/// `delivered` is asked before the host on every arm, so every detector in
/// this family reads the plan being built the same way; a system manager is
/// never provisioned today, which makes the first half a no-op for these arms
/// and keeps it from being a second rule when one is.
fn detect_system_arm(arms: &[SystemArm], delivered: &dyn Fn(&str) -> bool) -> Option<&'static str> {
    arms.iter()
        .find(|(method, tool)| delivered(method) || system_tool_available(tool))
        .map(|(method, _)| *method)
}

/// Which manager a brew→apt→dnf cascade would pick, or `fallback` when none of
/// them is available or delivered. The name a `BootstrapPlan` carries as its
/// method. `delivered` is the plan-time question — will the run have put this
/// mediator on the machine — and is asked before the host is probed.
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
pub(super) fn detect_brew_system_method(
    fallback: &'static str,
    delivered: &dyn Fn(&str) -> bool,
) -> &'static str {
    detect_brew_or_system_method(BREW_SYSTEM_ARMS, delivered).unwrap_or(fallback)
}

/// The mediator a brew-then-system bootstrap can actually run on this host, or
/// `None` when none is present.
///
/// The strict counterpart of [`detect_brew_system_method`], for a manager with
/// no bootstrap arm of its own: naming a mediator the host cannot run used to
/// degrade into a cascade that tried something else, and under a binding plan
/// it would be a guaranteed failure instead.
pub(super) fn detect_brew_or_system_method(
    arms: &[SystemArm],
    delivered: &dyn Fn(&str) -> bool,
) -> Option<&'static str> {
    if delivered("brew") || brew_available() {
        return Some("brew");
    }
    detect_system_arm(arms, delivered)
}

/// Which manager an apt→dnf→zypper cascade can run here, or `None` when none of
/// them is present. Linux-only, like the two managers whose plans resolve their
/// method through it. Binding on execution for the same reason
/// [`detect_brew_system_method`] is.
#[cfg(target_os = "linux")]
pub(super) fn detect_system_method(delivered: &dyn Fn(&str) -> bool) -> Option<&'static str> {
    detect_system_arm(SYSTEM_MANAGER_ARMS, delivered)
}

/// Every mediator a `go` bootstrap can run: brew, then the full system cascade
/// (`bootstrap_via_system_manager`, which reaches zypper as well).
pub(super) fn detect_go_bootstrap_method(delivered: &dyn Fn(&str) -> bool) -> Option<&'static str> {
    detect_brew_or_system_method(SYSTEM_MANAGER_ARMS, delivered)
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
    hand_child_bootstrapped_path(&mut cmd);
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
        let prefix = macos_brew_prefix();
        vec![format!("{prefix}/bin"), format!("{prefix}/sbin")]
    } else {
        Vec::new()
    }
}

/// The macOS Homebrew prefixes, in the order an installed one is looked for.
const MACOS_BREW_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];

/// Which macOS prefix brew lives under — the same answer before and after a
/// bootstrap.
///
/// This value is read twice per run, once while the plan is built and once
/// right after brew is installed, and the two reads have to agree or the plan
/// promises one directory while the apply records another. So an installed brew
/// answers for itself (including an Intel-prefix brew running under Rosetta on
/// Apple Silicon), and a machine with no brew answers from the architecture the
/// installer will target rather than from anything the install would change.
fn macos_brew_prefix() -> &'static str {
    macos_brew_prefix_from(&MACOS_BREW_PREFIXES, std::env::consts::ARCH)
}

/// The derivation itself, over the candidates and architecture it is given: the
/// first candidate holding a real brew wins, and a machine holding none answers
/// from `arch`.
///
/// Parameterized so the wiring is drivable on any host. Reading the real
/// absolutes would make the search untestable everywhere except a Mac with the
/// right brew already installed, and the order candidates are tried in is the
/// half of this that decides what an Intel-prefix brew on Apple Silicon
/// answers.
fn macos_brew_prefix_from<'p>(candidates: &[&'p str], arch: &str) -> &'p str {
    candidates
        .iter()
        .copied()
        .find(|prefix| brew_prefix_holds_brew(std::path::Path::new(prefix)))
        .unwrap_or_else(|| macos_brew_prefix_for_arch(arch))
}

/// Whether `prefix` holds a real brew. The probe is the `bin/brew` binary and
/// never the bin directory: stock macOS ships a `/usr/local/bin` with no brew
/// in it, so a directory probe would answer `/usr/local` on a bare Apple
/// Silicon machine and `/opt/homebrew` once the installer had run — the moving
/// answer this derivation exists to rule out.
fn brew_prefix_holds_brew(prefix: &std::path::Path) -> bool {
    prefix.join("bin/brew").is_file()
}

/// Where the Homebrew installer puts a prefix on an `arch` machine that has
/// none yet: Apple Silicon gets `/opt/homebrew`, Intel keeps `/usr/local`.
fn macos_brew_prefix_for_arch(arch: &str) -> &'static str {
    if arch == "aarch64" {
        MACOS_BREW_PREFIXES[0]
    } else {
        MACOS_BREW_PREFIXES[1]
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
        // own-path-ok: stat is coreutils, not a manager this run could have bootstrapped
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

/// The version a manager's own binary reports, for
/// [`cfgd_core::providers::PackageManager::tool_version`]: `cmd` run to completion, its first
/// dotted number taken. `None` on a spawn failure, a non-zero exit or a
/// banner holding no version, so a row that cannot state the fact states
/// nothing rather than a guess.
pub(super) fn tool_version_from(cmd: &mut Command) -> Option<String> {
    hand_child_bootstrapped_path(cmd);
    let out = cfgd_core::command_output_with_timeout(cmd, cfgd_core::COMMAND_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    parse_tool_version(&cfgd_core::stdout_lossy_trimmed(&out))
}

/// The first dotted number in a `--version` banner, its `v`/`go` prefix and
/// trailing punctuation dropped: `Homebrew 4.6.3` → `4.6.3`, `go version
/// go1.24.1 linux/amd64` → `1.24.1`, `v1.8.1911` → `1.8.1911`, `apk-tools
/// 2.14.4, compiled for x86_64.` → `2.14.4`. A token with no digit after its
/// letters, or no dot, is a word.
pub(super) fn parse_tool_version(banner: &str) -> Option<String> {
    banner
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .map(|token| token.trim_start_matches(|c: char| c.is_ascii_alphabetic()))
        .find(|token| {
            token.contains('.')
                && token.chars().next().is_some_and(|c| c.is_ascii_digit())
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        })
        .map(str::to_string)
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
