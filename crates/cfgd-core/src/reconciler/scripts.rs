use std::io::IsTerminal;

use crate::PathDisplayExt;
use crate::config::{ScriptCommand, ScriptEntry, ScriptShell};
use crate::errors::{CfgdError, ConfigError, Result};
use crate::output::{OutputWindow, Printer, Role, condense_script_label};

use super::format::DisplaySubject;
use super::types::{ReconcileContext, ScriptPhase};

// ---------------------------------------------------------------------------
// Unified script executor
// ---------------------------------------------------------------------------

/// Default timeout for module-level scripts.
pub(crate) const MODULE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How `execute_script` should treat a script given its `interactive` flag and
/// whether stdin is a TTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveDisposition {
    /// Interactive and a TTY is present: run attached to the terminal.
    Run,
    /// Interactive but no TTY (CI, piped stdin, daemon): skip with a warning.
    SkipNoTty,
    /// Not an interactive script: run via the normal piped/spinner path.
    NotInteractive,
}

/// Decide how to run a script from its `interactive` flag and stdin TTY state.
///
/// Pure so both terminal-attached and skip-with-warn paths are unit-testable
/// without a PTY.
fn interactive_disposition(interactive: bool, stdin_is_tty: bool) -> InteractiveDisposition {
    match (interactive, stdin_is_tty) {
        (false, _) => InteractiveDisposition::NotInteractive,
        (true, true) => InteractiveDisposition::Run,
        (true, false) => InteractiveDisposition::SkipNoTty,
    }
}

/// How a `run:` string resolves to something [`execute_script_inner`] /
/// [`run_filter_script`] can spawn.
enum RunTarget {
    /// `run_str`, taken as a whole, names an existing file: direct exec, the
    /// OS's own shebang handling selects the interpreter, no shell involved
    /// and no argv beyond the file itself.
    File(std::path::PathBuf),
    /// Everything else: an inline command a shell interprets. When the
    /// leading whitespace-delimited token alone names an existing file
    /// (relative to `script_dir`, or absolute), it has already been resolved
    /// and substituted back in, quoted for the target shell — the remainder
    /// of the string is untouched, so the shell, not this function, parses
    /// it.
    Inline(String),
}

/// Resolve `run_str` into a [`RunTarget`].
///
/// The whole-string existence test runs first and is unconditional: a
/// `run:` naming only a script path, with no trailing text, keeps running
/// exactly as it always did — direct exec, no shell, no argv splitting. That
/// scope is deliberate and narrower than "does the leading token resolve": a
/// prior version of this function split on the first whitespace unconditionally
/// and args-ified everything after, which reinterpreted `&&`, redirects,
/// quoted arguments, and later lines of a multi-line body as literal argv
/// entries instead of shell syntax — the second command in
/// `scripts/deploy.sh && echo done` silently never ran. Only the LEADING
/// token of a string that already needs the shell arm (because the whole
/// string isn't a file) is ever resolved; the shell reparses everything
/// else in `tail` byte-for-byte, so metacharacters, quoting, and later lines
/// in a multi-line body behave exactly as they did before this function
/// existed — the substitution only fixes the leading token's own resolution
/// against `script_dir` instead of the caller's `working_dir`.
fn resolve_run_target(
    run_str: &str,
    script_dir: &std::path::Path,
    shell: ScriptShell,
) -> RunTarget {
    if let Some(resolved) = resolve_existing_script(run_str, script_dir) {
        return RunTarget::File(resolved);
    }
    let Some(idx) = run_str.find(char::is_whitespace) else {
        return RunTarget::Inline(run_str.to_string());
    };
    let (command, tail) = run_str.split_at(idx);
    match resolve_existing_script(command, script_dir) {
        Some(resolved) => {
            let quoted = quote_resolved_script_path(&resolved, shell);
            RunTarget::Inline(format!("{quoted}{tail}"))
        }
        None => RunTarget::Inline(run_str.to_string()),
    }
}

/// Resolve `candidate` against `script_dir` when relative, returning it
/// absolutized and only when it names a file that actually exists on disk —
/// the shared file-vs-inline test both call sites in [`resolve_run_target`]
/// use.
fn resolve_existing_script(
    candidate: &str,
    script_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(candidate);
    let resolved = if path.is_relative() {
        script_dir.join(path)
    } else {
        path.to_path_buf()
    };
    // `is_file()`, not `exists()`: a leading `.` (the POSIX dot-source
    // builtin, e.g. `run: . ~/.venv/bin/activate && python app.py`) or an
    // empty leading token (a block scalar opening with a blank line) both
    // resolve to `script_dir` itself, which exists as a directory. `exists()`
    // would substitute the directory in as argv[0]/the sourced target,
    // destroying both idioms; `is_file()` correctly falls through to
    // `RunTarget::Inline` and leaves the text untouched for the shell.
    //
    // `crate::absolutize_path` rather than the bare join: the whole point of
    // substituting this token back into the run string is that it must not
    // depend on the shell's own `cwd` (`working_dir`), so the value this
    // function hands back has to be absolute on its own rather than resting
    // on every caller always passing an already-absolute `script_dir`.
    resolved
        .is_file()
        .then(|| crate::absolutize_path(&resolved))
}

/// Render `resolved` as a single word for substitution into an inline
/// `run_str` body executed by `shell` — quoted so the resolved token cannot
/// reopen, or be extended by, whatever the author wrote after it.
///
/// `Cmd` (and `Auto` on Windows, which dispatches to it) goes through
/// [`crate::cmd_double_quoted`] rather than a bare `"…"` wrap: `%` is a legal
/// NTFS filename character and `cmd.exe` expands `%NAME%` even inside double
/// quotes, so a resolved path such as `deploy%PATH%.cmd` would otherwise
/// splice the caller's own environment into the substituted token.
fn quote_resolved_script_path(resolved: &std::path::Path, shell: ScriptShell) -> String {
    match shell {
        ScriptShell::Pwsh => crate::powershell_single_quoted(&resolved.to_string_lossy()),
        ScriptShell::Cmd => crate::cmd_double_quoted(&resolved.to_string_lossy()),
        ScriptShell::Auto if cfg!(windows) => crate::cmd_double_quoted(&resolved.to_string_lossy()),
        ScriptShell::Auto | ScriptShell::Sh | ScriptShell::Bash | ScriptShell::Zsh => {
            crate::posix_single_quoted(&resolved.to_string_lossy())
        }
    }
}

/// Prepend PATH directories cfgd recorded when it bootstrapped a package
/// manager onto a script's process environment.
///
/// A manager cfgd installed this run put its binaries somewhere no ancestor of
/// this process had on PATH, and the generated env file that carries them is
/// only read by a shell started later — so without this a `postApply` script
/// that calls the very binary the module just asked for dies with
/// `command not found`.
///
/// Prepending (rather than appending) matches `generate_env_file_content`: a
/// script and the login shell that follows it must resolve a command the same
/// way, and cfgd installed the manager's copy on purpose.
fn prepend_bootstrapped_path_dirs(env: &mut Vec<(String, String)>, path_dirs: &[String]) {
    if path_dirs.is_empty() {
        return;
    }
    // A PATH the script's own declared environment set wins as the base: the
    // author asked for it, so the bootstrapped directories go in FRONT of that
    // rather than in front of whatever this process inherited.
    let current = env
        .iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let dirs: Vec<std::path::PathBuf> = path_dirs.iter().map(std::path::PathBuf::from).collect();
    let Some(joined) = crate::path_with_dirs_prepended(&current, &dirs) else {
        return;
    };
    match env.iter_mut().rev().find(|(k, _)| k == "PATH") {
        Some(slot) => slot.1 = joined,
        None => env.push(("PATH".to_string(), joined)),
    }
}

/// Everything a lifecycle script's environment is derived from.
///
/// A struct rather than a positional argument list: six of the seven fields are
/// a path, an optional path, or a string, so a transposed pair would compile and
/// silently mislabel `CFGD_MODULE_DIR` or the profile name.
pub(crate) struct ScriptEnvContext<'a> {
    pub config_dir: &'a std::path::Path,
    pub profile_name: &'a str,
    pub context: ReconcileContext,
    pub phase: &'a ScriptPhase,
    pub module_name: Option<&'a str>,
    pub module_dir: Option<&'a std::path::Path>,
    /// PATH entries of package managers cfgd bootstrapped; they land ahead of
    /// the inherited PATH so a script can reach a binary the same apply just
    /// installed.
    pub path_dirs: &'a [String],
}

/// Build environment variables injected into every script invocation.
pub(crate) fn build_script_env(ctx: &ScriptEnvContext<'_>) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "CFGD_CONFIG_DIR".to_string(),
            ctx.config_dir.display().to_string(),
        ),
        ("CFGD_PROFILE".to_string(), ctx.profile_name.to_string()),
        (
            "CFGD_CONTEXT".to_string(),
            match ctx.context {
                ReconcileContext::Apply => "apply".to_string(),
                ReconcileContext::Reconcile => "reconcile".to_string(),
            },
        ),
        (
            "CFGD_PHASE".to_string(),
            ctx.phase.display_name().to_string(),
        ),
    ];
    if let Some(name) = ctx.module_name {
        env.push(("CFGD_MODULE_NAME".to_string(), name.to_string()));
    }
    if let Some(dir) = ctx.module_dir {
        env.push(("CFGD_MODULE_DIR".to_string(), dir.display().to_string()));
    }
    prepend_bootstrapped_path_dirs(&mut env, ctx.path_dirs);
    env
}

/// Build environment variables for a module lifecycle script.
///
/// Extends `build_script_env` with the module's declared `spec.env` vars.
/// CFGD_* names in `spec.env` are silently dropped: runtime-injected metadata
/// must not be shadowed by user-supplied values.
pub(crate) fn build_module_script_env(
    ctx: &ScriptEnvContext<'_>,
    module_env: &[crate::config::EnvVar],
) -> Vec<(String, String)> {
    let mut env = build_script_env(ctx);
    // Expand `$VAR`/`${VAR}` in each declared value before injecting it into the
    // process environment: no shell is present here to do it, so a literal
    // `PATH: ...:$PATH` would overwrite PATH with garbage and the interpreter
    // itself would fail to spawn (os error 2). Resolve against the current
    // process env plus the metadata + already-expanded module vars, so a later
    // var can reference an earlier one (fold-left, as a shell would).
    let mut resolved: std::collections::HashMap<String, String> = std::env::vars().collect();
    for (k, v) in &env {
        resolved.insert(k.clone(), v.clone());
    }
    for ev in module_env {
        // Expand a leading `~` to home BEFORE `$VAR`/`${VAR}`: like the managed
        // env files, a literal `~/.local/bin` injected straight into the child
        // process environment would never be expanded (no shell is present).
        let tilded = crate::expand_env_value_tilde(&ev.value);
        let value = crate::expand_env_vars(&tilded, &|name| resolved.get(name).cloned());
        resolved.insert(ev.name.clone(), value.clone());
        env.push((ev.name.clone(), value));
    }
    env
}

/// Default working directory for a lifecycle script: the user's home directory.
///
/// Scripts reach module-bundled assets and the config tree through the
/// always-injected `$CFGD_MODULE_DIR` / `$CFGD_CONFIG_DIR` env vars, so the
/// config *source* tree is never the implicit CWD — a relative write from a
/// script lands in `$HOME`, not the user's version-controlled GitOps repo. A
/// per-script `workdir:` overrides this (see `resolve_script_workdir`). Falls
/// back to `config_dir` only when the home directory cannot be resolved.
pub(crate) fn script_default_workdir(config_dir: &std::path::Path) -> std::path::PathBuf {
    crate::home_dir_var()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config_dir.to_path_buf())
}

/// Resolve a `workdir:` value: expand `$VAR`/`${VAR}` against the script
/// environment, then a leading `~` to the user's home directory.
fn resolve_script_workdir(raw: &str, env_vars: &[(String, String)]) -> std::path::PathBuf {
    let expanded = crate::expand_env_vars(raw, &|name| {
        env_vars
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    });
    crate::expand_tilde(std::path::Path::new(&expanded))
}

/// What one [`execute_script`] invocation renders as its status subject.
///
/// Three variants rather than an optional marker beside an optional body: a
/// planned action's subject is derived ONCE, by
/// [`action_display_subject`](crate::reconciler::action_display_subject), and
/// two independent `Option`s are exactly what lets the marker and the body it
/// belongs to drift apart at one arm.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum ScriptSubject<'a> {
    /// No hook and no planned action names it: the condensed body alone.
    #[default]
    Bare,
    /// A hook phase names it, but no planned action does — backup hooks, the
    /// daemon's `onDrift`, a file's `onChange`. The marker is known; the body
    /// is condensed from the entry.
    Hook(&'a str),
    /// A planned action: the subject the preview bullet printed and the
    /// phase's alignment column measured, rendered verbatim so the three are
    /// one string.
    Planned(&'a DisplaySubject),
}

/// How one [`execute_script`] invocation reports its single status line.
///
/// One parameter carrying both facts rather than two: every call site that
/// names the subject also knows whether its failure stops the run, and a second
/// bare parameter is what lets the two drift apart at one arm.
#[derive(Clone, Copy, Default)]
pub(crate) struct ScriptReport<'a> {
    pub subject: ScriptSubject<'a>,
    /// The caller has already decided a failure here will not stop the run, so
    /// the one line renders `Role::Warn` rather than `Role::Fail`.
    pub non_fatal: bool,
    /// Set when this script runs INSIDE a concurrent lane, which owns the whole
    /// of the worker's terminal: the body flows into the lane and the one
    /// status line is the coordinator's, not this call's. Without it a script
    /// install would stream at ambient depth beside every other lane's output
    /// and settle a second line for an action the coordinator also settles.
    pub lane: Option<&'a dyn crate::output::LaneOutput>,
}

impl std::fmt::Debug for ScriptReport<'_> {
    /// Hand-written because `dyn LaneOutput` is not `Debug`; the lane is
    /// reported as present or absent, which is the only fact about it a
    /// diagnostic needs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptReport")
            .field("subject", &self.subject)
            .field("non_fatal", &self.non_fatal)
            .field("lane", &self.lane.is_some())
            .finish()
    }
}

/// The script's single status line, and the window it may collapse into.
///
/// The invocation-wide failure role plus a state machine — NOT a struct with an
/// `Option<OutputWindow>` and a `bool`: the window and the "already reported"
/// flag are the same fact, so representing them separately is what lets a
/// `Drop`-emitted `Info` slip out behind an emitted status.
struct ScriptStatus<'p> {
    /// The role a FAILURE renders as for this invocation. Held here, not passed
    /// per call, so every failure exit — a guard's `?`, the windowed failure,
    /// the post-window `?`, the wrapper's own `Err` arm — resolves it from one
    /// field. A per-call-site role parameter is the shape that lets one arm
    /// disagree.
    failure_role: Role,
    /// The subject's marker, unsuffixed — the status renderer appends the `:`
    /// it paints in its own style, and [`DisplaySubject`] appends it when the
    /// two halves are joined into one string.
    marker: Option<String>,
    state: ScriptState<'p>,
}

enum ScriptState<'p> {
    /// Nothing emitted, no window. Every guard arm reports from here.
    Pending {
        printer: &'p Printer,
        subject: String,
    },
    /// Inside a concurrent lane: the body goes to the lane and NOTHING is ever
    /// settled here. Terminal in the same sense as `Reported` — the status the
    /// caller would have emitted is the coordinator's to write.
    Laned {
        lane: &'p dyn crate::output::LaneOutput,
    },
    /// The window is open. It is INSIDE the state, so the only way to reach it
    /// is a method that also moves the state to `Reported`.
    Windowed {
        window: OutputWindow<'p>,
        subject: String,
    },
    /// The one status has been emitted. Terminal; nothing else can emit.
    Reported,
}

impl<'p> ScriptStatus<'p> {
    /// `run` is the raw script body; every subject a planned action does not
    /// supply is derived from it by `format.rs`, which is also where a caller
    /// that must size an alignment column before the script runs derives the
    /// same string.
    fn new(printer: &'p Printer, run: &str, report: ScriptReport<'p>) -> Self {
        let DisplaySubject { marker, body } = match report.subject {
            ScriptSubject::Bare => super::format::bare_script_subject(run),
            ScriptSubject::Hook(hook) => super::format::hook_script_subject(hook, run),
            // Verbatim, both halves: the renderer composes them as
            // `<marker> <subject>`, which is `DisplaySubject`'s own `Display`.
            ScriptSubject::Planned(subject) => subject.clone(),
        };
        Self {
            failure_role: if report.non_fatal {
                Role::Warn
            } else {
                Role::Fail
            },
            marker,
            state: match report.lane {
                Some(lane) => ScriptState::Laned { lane },
                None => ScriptState::Pending {
                    printer,
                    subject: body,
                },
            },
        }
    }

    /// Every emission in one place. `Pending` emits a plain status; `Windowed`
    /// FINISHES the window with this role, never drops it; `Reported` is a
    /// no-op. All three become `Reported`, so the window is moved out rather
    /// than left for `Drop`.
    fn settle(&mut self, role: Role, detail: Option<&str>, duration: Option<std::time::Duration>) {
        let marker = self.marker.as_ref().map(|m| format!("{m}:"));
        let apply = |mut builder: crate::output::StatusBuilder<'_>| {
            if let Some(m) = marker {
                builder = builder.marker(m);
            }
            if let Some(d) = detail {
                builder = builder.detail(d);
            }
            if let Some(d) = duration {
                builder = builder.duration(d);
            }
            drop(builder);
        };
        match std::mem::replace(&mut self.state, ScriptState::Reported) {
            ScriptState::Pending { printer, subject } => {
                apply(printer.action_status(role, subject))
            }
            ScriptState::Windowed { window, subject } => apply(window.finish_action(role, subject)),
            // Deliberately silent, and deliberately still consumed: a lane's
            // action has exactly one line and the coordinator writes it.
            ScriptState::Laned { .. } => {}
            ScriptState::Reported => {
                debug_assert!(false, "a script emitted a second status line");
            }
        }
    }

    /// Pre-window arms (guards, no-TTY skip, interactive). Calling it after
    /// `open_window` is not a defect: it routes through `settle`, which finishes
    /// the open window as that role. There is no state in which a status has
    /// been emitted AND a window is still open.
    fn status(&mut self, role: Role, detail: Option<&str>) {
        self.settle(role, detail, None);
    }

    /// `Pending` -> `Windowed`, at the inherited depth.
    ///
    /// The window's label is the SAME subject the settle writes, joined by
    /// [`DisplaySubject`]'s own `Display`. One action must not be spelled two
    /// ways on one screen: the running line read `Running script: nvim …` while
    /// the settled line replacing it read `postApply: nvim …`, so the two
    /// halves of one step's life named it differently.
    fn open_window(&mut self) {
        let marker = self.marker.clone();
        let taken = std::mem::replace(&mut self.state, ScriptState::Reported);
        self.state = match taken {
            ScriptState::Pending { printer, subject } => {
                let label = DisplaySubject {
                    marker,
                    body: subject.clone(),
                }
                .to_string();
                ScriptState::Windowed {
                    window: printer.output_window(&label),
                    subject,
                }
            }
            other => other,
        };
    }

    /// Feed the window, or the lane; a no-op in `Pending` and `Reported`.
    fn push_line(&mut self, raw: &str) {
        match &mut self.state {
            ScriptState::Windowed { window, .. } => window.push_line(raw),
            ScriptState::Laned { lane } => lane.push_line(raw),
            ScriptState::Pending { .. } | ScriptState::Reported => {}
        }
    }

    fn finish_ok(&mut self, duration: std::time::Duration) {
        self.settle(Role::Ok, None, Some(duration));
    }

    /// The one failure emitter. Reads `failure_role` rather than taking one, so
    /// `continueOnError` cannot render `Fail` on one exit and `Warn` on another.
    fn finish_fail(&mut self, detail: &str, duration: Option<std::time::Duration>) {
        self.settle(self.failure_role, Some(detail), duration);
    }

    fn reported(&self) -> bool {
        matches!(self.state, ScriptState::Reported)
    }
}

/// Unified script executor for all hook types at both profile and module level.
///
/// `shell_override` forces every inline command to run under the supplied
/// interpreter, ignoring any `shell:` field on the entry. Set by
/// `cfgd apply --shell <shell>` for debugging. File/shebang scripts ignore the
/// override (the shebang owns the interpreter choice) and emit a debug log.
///
/// Returns (description, changed, captured_output). All scripts set changed=true.
///
/// Emits EXACTLY ONE status line per call, whatever the exit path, because the
/// line is the wrapper's rather than each arm's: `execute_script_inner` reports
/// the outcomes it recognises through [`ScriptStatus`], and the tail below
/// covers every arm that returns without reporting — branching on the inner
/// OUTCOME, never on whether anything printed, so an inner `Ok` that reported
/// nothing renders as the success it is.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_script(
    entry: &ScriptEntry,
    script_dir: &std::path::Path,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    default_timeout: std::time::Duration,
    printer: &Printer,
    shell_override: Option<ScriptShell>,
    abort: Option<&crate::AbortFlag>,
    report: ScriptReport<'_>,
) -> Result<(String, bool, Option<String>)> {
    // The one environment read the inner body must not make for itself:
    // passing it in is what lets a test drive the interactive arm.
    execute_script_with_tty(
        std::io::stdin().is_terminal(),
        entry,
        script_dir,
        working_dir,
        env_vars,
        default_timeout,
        printer,
        shell_override,
        abort,
        report,
    )
}

/// [`execute_script`] with the TTY read supplied rather than made. The shipped
/// path passes the real answer; a test passes `true` to reach the interactive
/// arm on a host whose stdin is a pipe.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_script_with_tty(
    stdin_is_tty: bool,
    entry: &ScriptEntry,
    script_dir: &std::path::Path,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    default_timeout: std::time::Duration,
    printer: &Printer,
    shell_override: Option<ScriptShell>,
    abort: Option<&crate::AbortFlag>,
    report: ScriptReport<'_>,
) -> Result<(String, bool, Option<String>)> {
    let mut st = ScriptStatus::new(printer, entry.run_str(), report);
    let started = std::time::Instant::now();
    let out = execute_script_inner(
        &mut st,
        stdin_is_tty,
        entry,
        script_dir,
        working_dir,
        env_vars,
        default_timeout,
        shell_override,
        abort,
    );
    // A user script is the one thing cfgd runs whose effects it cannot predict:
    // a `preApply` hook that installs a toolchain must be visible to everything
    // planned after it, so no memoized command resolution outlives one.
    crate::invalidate_command_resolution();
    if !st.reported() {
        match &out {
            Ok(_) => st.finish_ok(started.elapsed()),
            // A failed hook's message carries the script's own captured output,
            // the same shape a failed package command's does.
            Err(e) => st.finish_fail(&crate::output::captured_output_detail(e), None),
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn execute_script_inner(
    st: &mut ScriptStatus<'_>,
    stdin_is_tty: bool,
    entry: &ScriptEntry,
    script_dir: &std::path::Path,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    default_timeout: std::time::Duration,
    shell_override: Option<ScriptShell>,
    abort: Option<&crate::AbortFlag>,
) -> Result<(String, bool, Option<String>)> {
    let run_str = entry.run_str();
    let run_label = condense_script_label(run_str);

    // Hold the PATH read-lock across interpreter resolution + spawn: a
    // concurrent test emptying `PATH` (command-not-found paths) is a data race
    // on `environ` that surfaces here as a spurious ENOENT. Compiled out of
    // release builds.
    #[cfg(any(test, feature = "test-helpers"))]
    let _path_guard = crate::test_helpers::path_env_read_guard();

    // `script_dir` is where the script's bundled files live (the module / config
    // source tree); a relative file-path `run:` resolves against it. `working_dir`
    // is the directory the script *runs in* — the home default by default, so a
    // relative write can't pollute the source tree. A per-script `workdir:`
    // overrides the run directory only (not file resolution): authors target the
    // deploy dir (`workdir: ~/.local/share/app`), the module source
    // (`workdir: $CFGD_MODULE_DIR`), or any absolute path.
    let workdir_override = match entry {
        ScriptEntry::Full(ScriptCommand {
            workdir: Some(w), ..
        }) => Some(resolve_script_workdir(w, env_vars)),
        _ => None,
    };
    let working_dir = workdir_override.as_deref().unwrap_or(working_dir);

    ensure_working_dir(&run_label, working_dir)?;

    // Resource-id / state-matching key, NOT a display string: the onChange /
    // module-onChange callers in `apply.rs` push this return value straight
    // into `ActionResult.description`, which `parse_resource_from_description`
    // parses back into a `managed_resource` id. The window's own label (the
    // condensed subject `ScriptStatus` holds) is correct for the running line
    // but must never be returned — that would reshape the id and orphan every
    // already-recorded state row for a module with a multi-line inline
    // script. Mirrors `format_action_description` in `format.rs` and
    // `apply_script_action` in `scripts_apply.rs`, which already return the
    // raw body for this same reason.
    let resource_desc = format!("Running script: {}", run_str);

    // Idempotency guards run BEFORE the body: `creates` (path existence),
    // then `onlyIf` (run only on zero exit), then `unless` (run only on
    // non-zero exit). Any guard that says "skip" short-circuits with
    // changed=false. A skip is a clean no-op, not a failure.
    if let ScriptEntry::Full(ScriptCommand {
        only_if,
        unless,
        creates,
        shell,
        ..
    }) = entry
    {
        let guard_shell = shell_override.unwrap_or(*shell);

        if let Some(path) = creates {
            let resolved_creates = resolve_creates_path(path, working_dir);
            if resolved_creates.exists() {
                st.status(
                    Role::Skipped,
                    Some(&format!(
                        "creates path already exists: {}",
                        resolved_creates.posix()
                    )),
                );
                return Ok((resource_desc, false, None));
            }
        }

        if let Some(cmd) = only_if {
            let success =
                run_guard_command(cmd, guard_shell, working_dir, env_vars, default_timeout)?;
            if !success {
                st.status(
                    Role::Skipped,
                    Some(&format!("onlyIf condition not met: {cmd}")),
                );
                return Ok((resource_desc, false, None));
            }
        }

        if let Some(cmd) = unless {
            let success =
                run_guard_command(cmd, guard_shell, working_dir, env_vars, default_timeout)?;
            if success {
                st.status(
                    Role::Skipped,
                    Some(&format!("unless condition already holds: {cmd}")),
                );
                return Ok((resource_desc, false, None));
            }
        }
    }

    tracing::debug!(
        run = %run_str,
        working_dir = %working_dir.display(),
        "executing script"
    );

    // Held apart from `effective_timeout` because the interactive `Run` arm
    // must NOT inherit `default_timeout` — only an author-declared `timeout:`
    // bounds an interactive step (see the disposition match below).
    let explicit_timeout = match entry {
        ScriptEntry::Full(ScriptCommand {
            timeout: Some(t), ..
        }) => Some(
            crate::parse_duration_str(t)
                .map_err(|e| CfgdError::Config(ConfigError::Invalid { message: e }))?,
        ),
        _ => None,
    };
    let effective_timeout = explicit_timeout.unwrap_or(default_timeout);
    let idle_timeout = match entry {
        ScriptEntry::Full(ScriptCommand {
            idle_timeout: Some(t),
            ..
        }) => Some(
            crate::parse_duration_str(t)
                .map_err(|e| CfgdError::Config(ConfigError::Invalid { message: e }))?,
        ),
        _ => None,
    };

    let entry_shell = match entry {
        ScriptEntry::Full(ScriptCommand { shell, .. }) => *shell,
        ScriptEntry::Simple(_) => ScriptShell::Auto,
    };
    let shell = shell_override.unwrap_or(entry_shell);

    let interactive = matches!(
        entry,
        ScriptEntry::Full(ScriptCommand {
            interactive: true,
            ..
        })
    );
    let disposition = interactive_disposition(interactive, stdin_is_tty);
    // Every other arm still gets its own process group, so a timeout/idle
    // kill (`kill_script_child`) can `kill(-pid, …)` the whole subtree
    // without hitting cfgd itself. The attached-to-terminal `Run` arm is the
    // one exception: it must share cfgd's own group so the terminal's
    // foreground group still includes the child (see the `Run` arm below).
    let set_process_group = !matches!(disposition, InteractiveDisposition::Run);

    let target = resolve_run_target(run_str, script_dir, shell);

    let cfgd_env_path = cfgd_env_path_for(shell);

    let mut cmd = match target {
        RunTarget::File(resolved) => {
            // File path — check executable bit, run directly (OS handles shebang).
            // The override silently drops out on file scripts: the shebang owns
            // interpreter choice, so wrapping the file in `bash -c` would either
            // double-interpret it or break exec semantics. The entry's own
            // `shell:` field on a file is still a config bug.
            if entry_shell != ScriptShell::Auto {
                return Err(CfgdError::Config(ConfigError::Invalid {
                    message: format!(
                        "shell field cannot be set on file-shebang scripts — set the shebang line inside '{}' itself",
                        resolved.posix(),
                    ),
                }));
            }
            if shell_override.is_some() {
                tracing::debug!(
                    script = %resolved.posix(),
                    shell_override = ?shell_override,
                    "--shell override ignored on file-shebang script"
                );
            }
            let meta = std::fs::metadata(&resolved)?;
            if !crate::is_executable(&resolved, &meta) {
                #[cfg(unix)]
                let hint = "chmod +x";
                #[cfg(windows)]
                let hint = "use a .exe, .cmd, .bat, or .ps1 extension";
                return Err(CfgdError::Config(ConfigError::Invalid {
                    message: format!(
                        "script '{}' exists but is not executable ({})",
                        resolved.posix(),
                        hint,
                    ),
                }));
            }
            let mut c = std::process::Command::new(&resolved);
            c.current_dir(working_dir);
            #[cfg(unix)]
            if set_process_group {
                use std::os::unix::process::CommandExt;
                c.process_group(0);
            }
            c
        }
        RunTarget::Inline(command) => {
            // Inline command — interpreter selected by shell field
            build_inline_command(
                shell,
                &command,
                working_dir,
                cfgd_env_path.as_deref(),
                set_process_group,
            )
        }
    };

    // Inject environment variables
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    match disposition {
        InteractiveDisposition::Run => {
            // Attach to the controlling terminal so the script can prompt the
            // user (e.g. `read`, `sudo`, a full-screen TUI). No spinner and
            // no capture — the user drives the pace.
            //
            // The child stays in cfgd's own process group (`set_process_group`
            // is false for this arm, above): a blanket `process_group(0)` put
            // every interactive child in a brand-new, non-foreground group,
            // so the terminal driver never delivered a terminal-generated
            // Ctrl-C to it, and a background-group terminal read stalls on
            // SIGTTIN instead of prompting. Sharing cfgd's group restores
            // both.
            //
            // No idle timeout: the user drives the pace and a lull is
            // expected. No absolute timeout unless the author declares one:
            // Ctrl-C now reaches the child directly (the escape hatch this
            // fix restores), so force-killing it by default would risk
            // SIGKILLing a TUI mid-raw-mode — worse than an unbounded wait.
            // A script that legitimately needs a ceiling opts in with its
            // own `timeout:`, honored below.
            cmd.stdin(std::process::Stdio::inherit());
            cmd.stdout(std::process::Stdio::inherit());
            cmd.stderr(std::process::Stdio::inherit());
            // Spawn-then-wait rather than `status()`: identical semantics with
            // stdio already inherited, but it routes through the ETXTBSY retry.
            let mut child = spawn_retry_on_busy(&mut cmd)?;
            let status = match explicit_timeout {
                Some(timeout) => wait_interactive_with_timeout(&mut child, timeout, &run_label)?,
                None => child.wait()?,
            };
            if !status.success() {
                let reason = crate::exit_status_reason(&status);
                return Err(CfgdError::Config(ConfigError::Invalid {
                    message: format!("script '{run_label}' failed ({reason})"),
                }));
            }
            return Ok((resource_desc, true, None));
        }
        InteractiveDisposition::SkipNoTty => {
            // No TTY (CI, piped stdin, or any daemon-run phase): skip rather than
            // hang on instant EOF. changed=false records this as a clean no-op.
            st.status(
                Role::Warn,
                Some("interactive script skipped: no TTY available"),
            );
            return Ok((resource_desc, false, None));
        }
        InteractiveDisposition::NotInteractive => {}
    }

    // Execute with timeout
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // A spawn ENOENT here almost never means "the script is missing" — the
    // interpreter couldn't be resolved, either because it is not installed
    // (e.g. `shell: bash` on a FreeBSD base that ships only POSIX sh) or
    // because a `spec.env` PATH entry overwrote PATH. Name the real causes
    // instead of a bare os error 2.
    let mut child = spawn_retry_on_busy(&mut cmd).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "could not spawn the script interpreter ({e}) — the interpreter \
                     may not be installed, or a spec.env PATH dropped the system bin dirs"
                ),
            })
        } else {
            CfgdError::from(e)
        }
    })?;

    // A script's output is not the answer to anything the user asked, so it
    // goes through the same bounded window every other child-process surface
    // uses: a five-line tail under the label, collapsed to one status line the
    // moment the script exits. The window sanitizes each line itself — a child
    // like `nvim --headless` emits screen-reset and cursor-move sequences that
    // would otherwise execute against the real terminal.
    st.open_window();

    // Channel for live display + Arc buffers for final capture.
    // Reader threads feed both, for live scrolling output AND full capture.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let last_output = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

    let stdout_handle = spawn_pipe_reader(
        child.stdout.take(),
        std::sync::Arc::clone(&stdout_buf),
        std::sync::Arc::clone(&last_output),
        tx.clone(),
    );
    let stderr_handle = spawn_pipe_reader(
        child.stderr.take(),
        std::sync::Arc::clone(&stderr_buf),
        std::sync::Arc::clone(&last_output),
        tx.clone(),
    );
    drop(tx);

    let start = std::time::Instant::now();
    loop {
        // Drain pending output and stream it, one line at a time, in order.
        while let Ok(line) = rx.try_recv() {
            st.push_line(&line);
        }

        match child.try_wait()? {
            Some(status) => {
                // Wait for reader threads to finish draining
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                // A script that exits between two loop iterations leaves its
                // lines queued in the channel: the drain above ran before the
                // readers had them, and `try_wait` then answered. Without this
                // final drain a fast script's body is complete in `captured`
                // and absent from the window/lane the reader is watching — the
                // faster the script, the likelier it renders nothing at all.
                for line in rx.try_iter() {
                    st.push_line(&line);
                }

                let stdout_str = std::sync::Arc::try_unwrap(stdout_buf)
                    .ok()
                    .and_then(|m| m.into_inner().ok())
                    .unwrap_or_default();
                let stderr_str = std::sync::Arc::try_unwrap(stderr_buf)
                    .ok()
                    .and_then(|m| m.into_inner().ok())
                    .unwrap_or_default();

                let captured = combine_script_output(&stdout_str, &stderr_str);

                if !status.success() {
                    let reason = crate::exit_status_reason(&status);
                    st.finish_fail(&reason, Some(start.elapsed()));
                    let base = format!("script '{run_label}' failed ({reason})");
                    let message = match captured.as_deref().filter(|s| !s.is_empty()) {
                        Some(c) => format!("{base}\n{c}"),
                        None => base,
                    };
                    return Err(CfgdError::Config(ConfigError::Invalid { message }));
                }

                st.finish_ok(start.elapsed());
                return Ok((resource_desc, true, captured));
            }
            None => {
                let elapsed = start.elapsed();
                let mut kill_reason = None;
                // Check absolute timeout
                if elapsed > effective_timeout {
                    kill_reason = Some(("timed out", effective_timeout));
                }
                // Check idle timeout (no output for N seconds)
                if kill_reason.is_none()
                    && let Some(idle_dur) = idle_timeout
                {
                    let last = *last_output.lock().unwrap_or_else(|e| e.into_inner());
                    if last.elapsed() > idle_dur {
                        kill_reason = Some(("idle (no output)", idle_dur));
                    }
                }
                // Cooperative abort: abort flag was set while the script was running.
                // Kill immediately (no grace period — already in an interrupt path).
                if kill_reason.is_none()
                    && let Some(a) = abort
                    && a.aborted().is_some()
                {
                    // Settling closes the window, so anything already queued
                    // has to be shown first or it is dropped unrendered.
                    for line in rx.try_iter() {
                        st.push_line(&line);
                    }
                    st.finish_fail("Interrupted", Some(elapsed));
                    kill_script_child(&mut child, false);
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    return Err(CfgdError::Config(ConfigError::Invalid {
                        message: format!("script '{}' interrupted by signal", run_label),
                    }));
                }
                if let Some((reason, duration)) = kill_reason {
                    for line in rx.try_iter() {
                        st.push_line(&line);
                    }
                    st.finish_fail(
                        &format!("{reason} after {}s", duration.as_secs()),
                        Some(elapsed),
                    );
                    kill_script_child(&mut child, true);
                    // Join reader threads to capture partial output
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    let stdout_str = std::sync::Arc::try_unwrap(stdout_buf)
                        .ok()
                        .and_then(|m| m.into_inner().ok())
                        .unwrap_or_default();
                    let stderr_str = std::sync::Arc::try_unwrap(stderr_buf)
                        .ok()
                        .and_then(|m| m.into_inner().ok())
                        .unwrap_or_default();
                    let captured = combine_script_output(&stdout_str, &stderr_str);
                    let base = format!(
                        "script '{}' {} after {}s",
                        run_label,
                        reason,
                        duration.as_secs()
                    );
                    let message = match captured.as_deref().filter(|s| !s.is_empty()) {
                        Some(c) => format!("{base}\n{c}"),
                        None => base,
                    };
                    return Err(CfgdError::Config(ConfigError::Invalid { message }));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// Reject a working directory that cannot host a script before spawning.
///
/// A stale `working_dir` (e.g. a tempdir from a prior `cfgd init` test that was
/// cleaned up off tmpfs) would otherwise surface as a cryptic `io error: No such
/// file or directory (os error 2)` from `cmd.spawn()` — naming neither the path
/// nor the script. One metadata syscall makes the failure point at the offender.
fn ensure_working_dir(run_str: &str, working_dir: &std::path::Path) -> Result<()> {
    let invalid = |message: String| CfgdError::Config(ConfigError::Invalid { message });
    match std::fs::metadata(working_dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(meta) => {
            let kind = if meta.is_file() { "file" } else { "other" };
            Err(invalid(format!(
                "script '{}' cannot run: working directory is not a directory ({}): {}",
                run_str,
                kind,
                working_dir.posix()
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(invalid(format!(
            "script '{}' cannot run: working directory does not exist: {}",
            run_str,
            working_dir.posix()
        ))),
        Err(e) => Err(invalid(format!(
            "script '{}' cannot run: working directory inaccessible ({}): {}",
            run_str,
            e,
            working_dir.posix()
        ))),
    }
}

/// Spawn a command, retrying briefly while the OS reports the executable busy.
///
/// A `fork` in any other thread duplicates every open write descriptor, so a
/// script this process just finished writing can still be held open by an
/// unrelated child at the moment it `exec`s — the kernel answers `ETXTBSY`. The window
/// closes the instant the racing child execs (its descriptors are `CLOEXEC`),
/// so a short bounded retry converges where a single attempt fails at random.
/// Every other spawn error is returned untouched on the first attempt.
fn spawn_retry_on_busy(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    const ATTEMPTS: u32 = 5;
    let mut delay = std::time::Duration::from_millis(10);
    let mut outcome = cmd.spawn();
    for _ in 1..ATTEMPTS {
        match &outcome {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {}
            _ => return outcome,
        }
        std::thread::sleep(delay);
        delay *= 2;
        outcome = cmd.spawn();
    }
    outcome
}

/// Result of running a script as a content filter (see [`run_filter_script`]).
pub(crate) struct FilterScriptOutcome {
    /// Everything the script wrote to stdout — the new file content.
    pub stdout: String,
    /// Everything the script wrote to stderr, trimmed; carried into the error
    /// message on failure so the operator sees why the filter refused.
    pub stderr: String,
    /// Process exit code; `None` when the process was killed by a signal.
    pub exit_code: Option<i32>,
    pub success: bool,
    pub timed_out: bool,
}

/// Run a script as a *content filter*: `stdin_content` goes in on stdin, the
/// script's stdout comes back out as the new content.
///
/// Distinct from [`execute_script`] on three axes, which is why it is a separate
/// entry point rather than a flag on that function: stdin carries data instead
/// of being `/dev/null`, stdout is captured as a value instead of being merged
/// with stderr for display, and there is no spinner (the caller is computing a
/// value, not narrating a phase). It shares this module's interpreter
/// resolution, environment injection, and process-group kill semantics so
/// filters cannot become a second, divergent execution path.
///
/// A non-zero exit is reported through the returned outcome, not an error — the
/// caller owns the typed error because it knows which target file is involved.
pub(crate) fn run_filter_script(
    run_str: &str,
    script_dir: &std::path::Path,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    stdin_content: &str,
    timeout: std::time::Duration,
) -> Result<FilterScriptOutcome> {
    // Same `environ` race as `execute_script`: hold the read lock across
    // interpreter resolution and spawn. Compiled out of release builds.
    #[cfg(any(test, feature = "test-helpers"))]
    let _path_guard = crate::test_helpers::path_env_read_guard();

    ensure_working_dir(run_str, working_dir)?;

    let target = resolve_run_target(run_str, script_dir, ScriptShell::Auto);

    let mut cmd = match target {
        RunTarget::File(resolved) => {
            let meta = std::fs::metadata(&resolved)?;
            if !crate::is_executable(&resolved, &meta) {
                #[cfg(unix)]
                let hint = "chmod +x";
                #[cfg(windows)]
                let hint = "use a .exe, .cmd, .bat, or .ps1 extension";
                return Err(CfgdError::Config(ConfigError::Invalid {
                    message: format!(
                        "patch script '{}' exists but is not executable ({})",
                        resolved.posix(),
                        hint,
                    ),
                }));
            }
            let mut c = std::process::Command::new(&resolved);
            c.current_dir(working_dir);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                c.process_group(0);
            }
            c
        }
        RunTarget::Inline(command) => {
            // A filter script is never interactive — always its own process
            // group so a timeout kill can reach the whole subtree.
            build_inline_command(ScriptShell::Auto, &command, working_dir, None, true)
        }
    };

    for (key, value) in env_vars {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = spawn_retry_on_busy(&mut cmd)?;

    // Feed stdin from its own thread while stdout/stderr drain on theirs: a
    // filter whose output exceeds the pipe buffer would deadlock against a
    // synchronous `write_all` here.
    let input = stdin_content.to_string();
    let mut stdin_pipe = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(pipe) = stdin_pipe.as_mut() {
            use std::io::Write;
            let _ = pipe.write_all(input.as_bytes());
            let _ = pipe.flush();
        }
        drop(stdin_pipe);
    });
    let stdout_handle = spawn_capture_reader(child.stdout.take());
    let stderr_handle = spawn_capture_reader(child.stderr.take());

    let start = std::time::Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait()? {
            Some(status) => break (Some(status), false),
            None => {
                if start.elapsed() > timeout {
                    kill_script_child(&mut child, true);
                    break (None, true);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };

    let _ = writer.join();
    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    Ok(FilterScriptOutcome {
        stdout,
        stderr: stderr.trim().to_string(),
        exit_code: status.and_then(|s| s.code()),
        success: !timed_out && status.is_some_and(|s| s.success()),
        timed_out,
    })
}

/// Drain a child pipe to a `String` on a dedicated thread (lossy UTF-8).
fn spawn_capture_reader<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return String::new();
        };
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// `cmd.exe /C <run>` with the script text passed through **verbatim**.
///
/// `Command::arg` escapes for `CommandLineToArgvW`, which `cmd.exe` does not
/// implement: it turns every `"` in the script into `\"`, and `cmd` reads that
/// backslash as part of the filename. A hook as ordinary as
/// `echo hi > "C:\path\marker"` therefore redirected into `\C:\path\marker\`
/// and silently wrote nothing. `raw_arg` appends the text unmodified, which is
/// what `cmd.exe` is documented to expect and what a user typing the same line
/// at a prompt gets.
///
/// Defined on every platform because `shell: cmd` is a config value a unix host
/// can parse and dispatch; the spawn simply fails there, as it always has.
fn cmd_command(run_str: &str) -> std::process::Command {
    let mut c = std::process::Command::new("cmd.exe");
    c.arg("/C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.raw_arg(run_str);
    }
    #[cfg(not(windows))]
    c.arg(run_str);
    c
}

/// Build the `Command` for an inline script based on the chosen shell interpreter.
///
/// When `cfgd_env_path` is `Some`, bash and zsh commands are prepended with a
/// preamble that sources `~/.cfgd.env` (with alias expansion enabled) so that
/// profile-level env vars and aliases are available to lifecycle scripts.
fn build_inline_command(
    shell: ScriptShell,
    run_str: &str,
    working_dir: &std::path::Path,
    cfgd_env_path: Option<&std::path::Path>,
    // Only read inside `#[cfg(unix)]` below: process groups are a POSIX
    // concept and there is no non-unix arm to consume it in.
    #[cfg_attr(not(unix), allow(unused_variables))] set_process_group: bool,
) -> std::process::Command {
    let mut c = match shell {
        ScriptShell::Auto => {
            #[cfg(unix)]
            {
                let mut c = std::process::Command::new("sh");
                c.arg("-c").arg(run_str);
                c
            }
            #[cfg(windows)]
            {
                cmd_command(run_str)
            }
        }
        ScriptShell::Sh => {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(run_str);
            c
        }
        ScriptShell::Bash => {
            let cmd_str = match cfgd_env_path {
                Some(p) => format!(
                    "shopt -s expand_aliases; source \"{}\" 2>/dev/null; {}",
                    p.display(),
                    run_str,
                ),
                None => run_str.to_string(),
            };
            let mut c = std::process::Command::new("bash");
            c.arg("-c").arg(cmd_str);
            c
        }
        ScriptShell::Zsh => {
            let cmd_str = match cfgd_env_path {
                Some(p) => format!(
                    "setopt aliases; source \"{}\" 2>/dev/null; {}",
                    p.display(),
                    run_str,
                ),
                None => run_str.to_string(),
            };
            let mut c = std::process::Command::new("zsh");
            c.arg("-c").arg(cmd_str);
            c
        }
        ScriptShell::Pwsh => {
            let mut c = std::process::Command::new("pwsh");
            c.arg("-NoProfile").arg("-Command").arg(run_str);
            c
        }
        ScriptShell::Cmd => cmd_command(run_str),
    };
    c.current_dir(working_dir);
    #[cfg(unix)]
    if set_process_group {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c
}

/// Resolve the `~/.cfgd.env` preamble path for bash/zsh inline commands.
/// Returns the expanded path only when it exists; `None` for other shells or
/// when the env file is absent.
fn cfgd_env_path_for(shell: ScriptShell) -> Option<std::path::PathBuf> {
    match shell {
        ScriptShell::Bash | ScriptShell::Zsh => {
            let p = crate::expand_tilde(std::path::Path::new("~/.cfgd.env"));
            if p.exists() { Some(p) } else { None }
        }
        _ => None,
    }
}

/// Resolve a `creates:` guard path.
///
/// A leading `~` expands to the home directory; a relative path resolves
/// against the script's `working_dir`. Absolute paths are used verbatim.
fn resolve_creates_path(path: &str, working_dir: &std::path::Path) -> std::path::PathBuf {
    let expanded = crate::expand_tilde(std::path::Path::new(path));
    if expanded.is_relative() {
        working_dir.join(expanded)
    } else {
        expanded
    }
}

/// Run an `onlyIf` / `unless` guard command and report whether it succeeded
/// (exited zero). Uses the same interpreter, working directory, and environment
/// as the script body, bounded by `timeout` so a guard can never hang the
/// reconcile.
///
/// Two failure modes are real errors, distinct from a non-zero exit (the normal
/// condition signal): a spawn failure (e.g. a missing interpreter, surfaced via
/// `?`), and a timeout (a hung guard is an environment fault, not a "skip" or
/// "run" decision — the watchdog's signal-kill exit status would otherwise be
/// silently read as a non-zero condition).
fn run_guard_command(
    cmd_str: &str,
    shell: ScriptShell,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    timeout: std::time::Duration,
) -> Result<bool> {
    let cfgd_env_path = cfgd_env_path_for(shell);
    // Guard commands are never interactive — always run in their own process
    // group so a timeout kill can reach the whole subtree.
    let mut cmd = build_inline_command(shell, cmd_str, working_dir, cfgd_env_path.as_deref(), true);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }
    let outcome = crate::command_output_with_timeout_outcome(&mut cmd, timeout)?;
    if outcome.timed_out {
        return Err(CfgdError::Config(ConfigError::Invalid {
            message: format!("guard command timed out after {timeout:?}: {cmd_str}"),
        }));
    }
    Ok(outcome.output.status.success())
}

/// Kill a script's process group.
///
/// `graceful=true` sends SIGTERM then waits 5 s before SIGKILL (for timeout/idle
/// kill paths). `graceful=false` sends SIGKILL immediately — used on the
/// abort path where cfgd itself received a signal and must exit quickly.
pub(super) fn kill_script_child(child: &mut std::process::Child, graceful: bool) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let signal = if graceful {
            Signal::SIGTERM
        } else {
            Signal::SIGKILL
        };
        // Negative PID targets the entire process group
        let _ = kill(Pid::from_raw(-(child.id() as i32)), signal);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    if graceful {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Terminate an interactive script's child directly by PID (SIGTERM, then,
/// after a grace period, SIGKILL).
///
/// Deliberately NOT `kill_script_child`: that helper targets the child's
/// process GROUP (`kill(-pid, …)`), which only reaches the intended process
/// when the child is its own group leader. The interactive `Run` arm
/// intentionally skips `process_group(0)` (see its own doc comment in
/// `execute_script_inner`) so the child shares cfgd's own foreground group —
/// `child.id()` is therefore not a process-group leader, and a negative-PID
/// kill would silently miss it.
#[cfg(unix)]
fn kill_interactive_timeout_child(child: &mut std::process::Child) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
    std::thread::sleep(std::time::Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_interactive_timeout_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Poll an interactive script's child for exit, force-killing it once
/// `timeout` elapses. Only reached when the author declared an explicit
/// `timeout:` on an `interactive: true` entry — see the `Run` arm's doc
/// comment in `execute_script_inner` for why the default is unbounded.
fn wait_interactive_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
    run_label: &str,
) -> Result<std::process::ExitStatus> {
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if start.elapsed() > timeout {
            kill_interactive_timeout_child(child);
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' timed out after {}s (interactive)",
                    run_label,
                    timeout.as_secs()
                ),
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Default `continue_on_error` behavior per script phase.
/// Pre-hooks abort on failure; post-hooks, onChange, onDrift continue.
pub(super) fn default_continue_on_error(phase: &ScriptPhase) -> bool {
    match phase {
        // A failed `patch` filter leaves the target unwritten; continuing past
        // it would apply a half-configured machine.
        // A failed `preBackup` leaves the source in whatever state the hook was
        // meant to prepare (typically a still-running service), so the snapshot
        // would be inconsistent — the backup unit aborts instead.
        ScriptPhase::PreApply
        | ScriptPhase::PreReconcile
        | ScriptPhase::Patch
        | ScriptPhase::PreBackup => false,
        ScriptPhase::PostApply
        | ScriptPhase::PostReconcile
        | ScriptPhase::OnChange
        | ScriptPhase::OnDrift
        | ScriptPhase::PostBackup => true,
    }
}

/// Resolve the effective `continue_on_error` for a script entry in a given phase.
pub(crate) fn effective_continue_on_error(entry: &ScriptEntry, phase: &ScriptPhase) -> bool {
    match entry {
        ScriptEntry::Full(ScriptCommand {
            continue_on_error: Some(v),
            ..
        }) => *v,
        _ => default_continue_on_error(phase),
    }
}

/// Combine stdout and stderr into a single captured output string.
/// Returns `None` if both are empty.
pub(super) fn combine_script_output(stdout: &str, stderr: &str) -> Option<String> {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    if stdout.is_empty() && stderr.is_empty() {
        return None;
    }
    let mut out = String::new();
    if !stdout.is_empty() {
        out.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !out.is_empty() {
            out.push_str("\n--- stderr ---\n");
        }
        out.push_str(stderr);
    }
    Some(out)
}

fn spawn_pipe_reader<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
    buf: std::sync::Arc<std::sync::Mutex<String>>,
    ts: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
    tx: std::sync::mpsc::Sender<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Some(pipe) = pipe else { return };
        let reader = std::io::BufReader::new(pipe);
        for line in std::io::BufRead::lines(reader) {
            match line {
                Ok(l) => {
                    *ts.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
                    let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                    if !b.is_empty() {
                        b.push('\n');
                    }
                    b.push_str(&l);
                    let _ = tx.send(l);
                }
                Err(_) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests;
