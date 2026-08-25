//! Shared env-target engine: given the merged env vars, aliases, and an
//! [`EnvScope`], compute the ordered set of targets the planner writes and the
//! verifier re-derives. Keeping both paths on one function is what guarantees
//! `cfgd apply` and `cfgd status`/`verify` agree on the target set (otherwise a
//! newly-written file reports as permanent false drift).
//!
//! Target computation is pure — `$SHELL`, fish presence, and which login
//! dotfiles already exist are captured once into an [`EnvHostProbe`] at the
//! caller boundary, so the matrix is unit-testable without mutating
//! process-global state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{EnvScope, EnvVar, ShellAlias};

use super::env_files::{
    ENV_FILE_HEADER, fish_in_use, generate_env_file_content, generate_fish_env_content,
    generate_powershell_env_content,
};

/// Source line shells evaluate to load the cfgd-managed env file. Uses the
/// POSIX `.` builtin, not the `source` alias: `.profile` is read by `/bin/sh`
/// (dash on Debian, the base `sh` on FreeBSD), which has no `source` — the alias
/// exists only in bash/zsh/csh. `.` is equivalent in bash and zsh, so one line
/// loads correctly across every shell cfgd injects into.
const UNIX_SOURCE_LINE: &str = "[ -f ~/.cfgd.env ] && . ~/.cfgd.env";
const PS_SOURCE_LINE: &str = ". ~/.cfgd-env.ps1";

/// LaunchAgent label for the *user-scope* (`spec.env`) plist. Deliberately
/// distinct from the system configurator's `com.cfgd.environment` so the two
/// never collide.
const MACOS_USER_PLIST_LABEL: &str = "com.cfgd.user-environment";
const MACOS_USER_PLIST_NAME: &str = "com.cfgd.user-environment.plist";

/// Where a managed env value ends up. The planner turns these into
/// [`super::types::EnvAction`]; the verifier checks the file variants exist
/// with the expected content.
pub(super) enum EnvTarget {
    /// A standalone cfgd-owned file — safe to overwrite wholesale.
    ///
    /// `rendered` counts what went into THIS file, not what the run merged:
    /// the systemd `environment.d` and launchd renderings carry env vars
    /// only, so a write line quoting the merged alias count would name
    /// aliases the file does not hold.
    ManagedFile {
        path: PathBuf,
        content: String,
        rendered: RenderedCounts,
    },
    /// An idempotent source-line appended into a user-owned dotfile.
    SourceLine { rc_path: PathBuf, line: String },
    /// A live-session refresh (no file; not a verified-drift surface).
    LiveSession { vars: Vec<(String, String)> },
}

/// Target operating-system family for env target selection. Injected so tests
/// exercise every platform's matrix regardless of the host running the suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnvPlatform {
    Linux,
    MacOs,
    FreeBsd,
    Windows,
}

impl EnvPlatform {
    pub(super) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "freebsd") {
            Self::FreeBsd
        } else {
            Self::Linux
        }
    }
}

/// Host facts that affect env target selection, captured once at the caller
/// boundary so [`env_targets`] stays pure.
pub(super) struct EnvHostProbe {
    /// The user's login shell (`$SHELL`), used to pick the interactive rc file.
    pub shell: String,
    /// Whether a managed fish env file should be written (fish in use *and* its
    /// `conf.d` directory exists).
    pub fish_present: bool,
    /// Whether `~/.bash_profile` already exists (we never create it — doing so
    /// would shadow a user's `~/.profile` in bash's first-match login chain).
    pub bash_profile_exists: bool,
    /// Whether `~/.bash_login` already exists.
    pub bash_login_exists: bool,
    /// Whether a POSIX `sh` (Git Bash) is on PATH — Windows-only relevance.
    pub git_bash_present: bool,
    /// Whether zsh is actually in use on this host — the login shell is zsh, a
    /// `zsh` binary is on PATH, or the user already keeps a `~/.zshrc`. Gates
    /// `~/.zshenv`: a bash-only host must not gain an inert cfgd-owned login file.
    pub zsh_present: bool,
}

impl EnvHostProbe {
    pub(super) fn detect(home: &Path) -> Self {
        #[cfg(any(test, feature = "test-helpers"))]
        if let Some(o) = TEST_HOST_PROBE_OVERRIDE.with(|cell| cell.borrow().clone()) {
            return Self {
                shell: o.shell,
                fish_present: o.fish_present,
                bash_profile_exists: o.bash_profile_exists,
                bash_login_exists: o.bash_login_exists,
                git_bash_present: o.git_bash_present,
                zsh_present: o.zsh_present,
            };
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let fish_conf_d = home.join(".config/fish/conf.d");
        let zsh_present = shell.contains("zsh")
            || crate::command_available("zsh")
            || home.join(".zshrc").exists();
        Self {
            shell,
            fish_present: fish_in_use() && fish_conf_d.exists(),
            bash_profile_exists: home.join(".bash_profile").exists(),
            bash_login_exists: home.join(".bash_login").exists(),
            git_bash_present: cfg!(windows) && crate::command_available("sh"),
            zsh_present,
        }
    }
}

// `EnvHostProbe::detect` reads `$SHELL`, `command_available("zsh"/"fish")`,
// and PATH — none of which `with_test_home_guard` isolates, so an
// integration-style test driving a real `cmd_apply`/`cmd_verify` gets
// whatever shell shape the CI runner happens to have. This is the same
// thread-local override idiom as `with_test_home`
// (`cfgd-core/src/util/paths.rs`): a test pins a declared host shape for the
// duration of a guard instead of asserting against ambient reality.
#[cfg(any(test, feature = "test-helpers"))]
thread_local! {
    static TEST_HOST_PROBE_OVERRIDE: std::cell::RefCell<Option<EnvHostProbeOverride>> =
        const { std::cell::RefCell::new(None) };
}

/// A declared host shape for [`EnvHostProbe::detect`] to return verbatim
/// instead of reading `$SHELL`/PATH/`~`. Field-for-field mirror of
/// [`EnvHostProbe`] rather than a re-export of it: `EnvHostProbe` stays
/// `pub(super)` (internal to the reconciler), while this override is the
/// crate's public test seam.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone)]
pub struct EnvHostProbeOverride {
    pub shell: String,
    pub fish_present: bool,
    pub bash_profile_exists: bool,
    pub bash_login_exists: bool,
    pub git_bash_present: bool,
    pub zsh_present: bool,
}

/// RAII guard restoring the prior override on drop, mirroring
/// [`crate::TestHomeGuard`].
#[cfg(any(test, feature = "test-helpers"))]
#[must_use = "dropping the guard immediately restores the previous override"]
pub struct EnvHostProbeOverrideGuard {
    prev: Option<EnvHostProbeOverride>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl Drop for EnvHostProbeOverrideGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        TEST_HOST_PROBE_OVERRIDE.with(|cell| *cell.borrow_mut() = prev);
    }
}

/// Install an `EnvHostProbe::detect` override for the current thread and
/// return a guard that restores the prior value (including `None`) on drop.
#[cfg(any(test, feature = "test-helpers"))]
pub fn with_env_host_probe_override_guard(
    probe: EnvHostProbeOverride,
) -> EnvHostProbeOverrideGuard {
    let prev = TEST_HOST_PROBE_OVERRIDE.with(|cell| cell.replace(Some(probe)));
    EnvHostProbeOverrideGuard { prev }
}

fn reaches_login(scope: EnvScope) -> bool {
    matches!(scope, EnvScope::Login | EnvScope::All)
}

fn reaches_all(scope: EnvScope) -> bool {
    matches!(scope, EnvScope::All)
}

/// Compute the ordered list of env targets for a scope. Empty input yields no
/// targets.
///
/// `path_dirs` carries the PATH entries of the package managers the desired
/// state names, so a profile that only bootstraps a manager still gets a
/// managed file *and* the rc source lines that make it reachable.
pub(super) fn env_targets(
    content: EnvContent<'_>,
    scope: EnvScope,
    home: &Path,
    probe: &EnvHostProbe,
    platform: EnvPlatform,
) -> Vec<EnvTarget> {
    let mut targets = Vec::new();
    if content.env.is_empty() && content.aliases.is_empty() && content.path_dirs.is_empty() {
        return targets;
    }

    match platform {
        EnvPlatform::Windows => windows_targets(content, home, probe, &mut targets),
        EnvPlatform::Linux | EnvPlatform::MacOs | EnvPlatform::FreeBsd => {
            unix_targets(content, scope, home, probe, platform, &mut targets)
        }
    }

    // Live-session refresh runs last, after the durable files are written.
    if reaches_all(scope) {
        let vars = valid_export_pairs(content.env);
        if !vars.is_empty() {
            targets.push(EnvTarget::LiveSession { vars });
        }
    }

    targets
}

/// The desired-state inputs every generated shell file is derived from,
/// bundled so the per-platform target builders take one parameter instead of
/// parallel slices that must stay in the same order at each call site.
#[derive(Clone, Copy)]
pub(super) struct EnvContent<'a> {
    env: &'a [EnvVar],
    aliases: &'a [ShellAlias],
    path_dirs: &'a [ManagerPathDir],
    origins: &'a EnvOrigins,
}

impl<'a> EnvContent<'a> {
    pub(super) fn new(
        env: &'a [EnvVar],
        aliases: &'a [ShellAlias],
        path_dirs: &'a [ManagerPathDir],
        origins: &'a EnvOrigins,
    ) -> Self {
        Self {
            env,
            aliases,
            path_dirs,
            origins,
        }
    }
}

/// Which layer each merged env var and alias came from, for the provenance
/// comment the generated shell files carry beside the line it explains.
///
/// Names only — a value is whatever survived the merge, and the comment says
/// who put it there. EVERY entry names its layer: the file is the merge of N
/// layers (profile chain, subscribed sources, modules) and so has no default
/// owner a reader could assume, which is what an unannotated line would ask
/// them to do.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct EnvOrigins(crate::config::EntryOwners);

impl EnvOrigins {
    /// Seed from the profile-layer merge's own record, so the layer that
    /// declared a surviving value owns it here too.
    pub(super) fn from_owners(owners: &crate::config::EntryOwners) -> Self {
        Self(owners.clone())
    }

    /// Record `module` as the owner of every entry it declares, overwriting an
    /// earlier claim exactly as the merge overwrites its value.
    pub(super) fn claim_module_entries(&mut self, module: &crate::modules::ResolvedModule) {
        self.0.claim(
            &crate::reconciler::Owner::module(&module.name).token(),
            &module.env,
            &module.aliases,
        );
    }

    /// The trailing ` # <kind>:<name>` an env-var line carries, or an empty
    /// string when nothing owns it.
    pub(super) fn env_comment(&self, name: &str) -> String {
        comment(self.0.env.get(name).map(String::as_str))
    }

    /// The same for an alias line.
    pub(super) fn alias_comment(&self, name: &str) -> String {
        comment(self.0.aliases.get(name).map(String::as_str))
    }
}

/// The trailing comment cfgd's own bootstrapped-PATH line carries: the managers
/// whose directories it publishes, in dir order and deduped
/// (` # manager:brew,cargo`). One comment for the whole line, because the line
/// is one `export`; empty when nothing named a manager.
///
/// This vocabulary is deliberately WIDER than [`super::OwnerKind`]'s: the name
/// half is a comma list, which no `Owner` name may hold, so reading it back
/// through `OwnerKind::from_token("manager")` stays `None` on purpose rather
/// than minting an owner that names two things at once.
pub(super) fn path_dirs_comment(dirs: &[ManagerPathDir]) -> String {
    let mut managers: Vec<&str> = Vec::new();
    for dir in dirs {
        if !dir.manager.is_empty() && !managers.contains(&dir.manager.as_str()) {
            managers.push(&dir.manager);
        }
    }
    if managers.is_empty() {
        return String::new();
    }
    comment(Some(&format!("manager:{}", managers.join(","))))
}

/// Render an owner token as a trailing shell comment.
///
/// A comment is appended OUTSIDE the quoted token every dialect's quoting
/// helper produced, so it annotates the assignment rather than joining the
/// value. An owner carrying anything but the token's own alphabet is dropped
/// rather than escaped: a `\n` would end the assignment and stand the
/// remainder up as further shell, and there is nothing a comment is worth
/// risking that for. `,` is in the alphabet because the PATH line's comment
/// names every manager that contributed to it, and a `,` cannot end a shell
/// line.
fn comment(owner: Option<&str>) -> String {
    match owner {
        Some(owner)
            if owner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | ',')) =>
        {
            format!(" # {owner}")
        }
        _ => String::new(),
    }
}

/// A generated line with its trailing owner comment removed, if it has one.
///
/// The exact inverse of [`comment`] and kept beside it so the two cannot
/// drift. Any comparison of a DEPLOYED line against a freshly rendered one
/// folds both sides through this: a file written before owner comments
/// existed carries none, and comparing raw would read every line in such a
/// file as having moved. Applied symmetrically, so a value that itself ends
/// in something comment-shaped folds the same way on both sides and still
/// compares equal to itself.
pub(super) fn without_owner_comment(line: &str) -> &str {
    match line.rsplit_once(" # ") {
        Some((head, tail)) if !tail.is_empty() && comment(Some(tail)) == format!(" # {tail}") => {
            head
        }
        _ => line,
    }
}

/// One PATH directory cfgd published, remembering the manager that created it.
///
/// The manager travels WITH the directory because the generated line names it:
/// a bare `Vec<String>` had already lost which manager each entry belonged to
/// by the time the env file was rendered, so the one line in the file that is
/// cfgd's own bookkeeping was the only line that could not say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerPathDir {
    pub manager: String,
    pub dir: String,
}

impl ManagerPathDir {
    pub fn new(manager: impl Into<String>, dir: impl Into<String>) -> Self {
        Self {
            manager: manager.into(),
            dir: dir.into(),
        }
    }

    /// A directory no manager claims. The sentinel render behind
    /// `super::env_files::path_dirs_line_prefix` takes this: the prefix is
    /// the text BEFORE the trailing comment, so naming a manager there would
    /// put a comment in the very string that has to match a real line's head.
    pub fn unowned(dir: impl Into<String>) -> Self {
        Self {
            manager: String::new(),
            dir: dir.into(),
        }
    }
}

/// The primary managed env file for `platform` — the first `ManagedFile`
/// [`env_targets`] pushes, and the only one the per-item checks read. Named
/// once here because a display surface reads that file back to show what a
/// drifted item ACTUALLY holds, and a second spelling of the path would let
/// the reader and the writer disagree about which file that is.
pub(super) fn primary_env_file_path(home: &Path, platform: EnvPlatform) -> PathBuf {
    match platform {
        EnvPlatform::Windows => home.join(".cfgd-env.ps1"),
        EnvPlatform::Linux | EnvPlatform::MacOs | EnvPlatform::FreeBsd => home.join(".cfgd.env"),
    }
}

fn unix_targets(
    content: EnvContent<'_>,
    scope: EnvScope,
    home: &Path,
    probe: &EnvHostProbe,
    platform: EnvPlatform,
    out: &mut Vec<EnvTarget>,
) {
    let EnvContent {
        env,
        aliases,
        path_dirs,
        origins,
    } = content;
    // Interactive (all scopes): the cfgd-owned env file + a source line in the
    // user's interactive rc, plus fish when it's in use.
    out.push(EnvTarget::ManagedFile {
        path: primary_env_file_path(home, platform),
        content: generate_env_file_content(env, aliases, path_dirs, origins),
        rendered: RenderedCounts::of(env, aliases),
    });
    let interactive_rc = if probe.shell.contains("zsh") {
        home.join(".zshrc")
    } else {
        home.join(".bashrc")
    };
    out.push(EnvTarget::SourceLine {
        rc_path: interactive_rc,
        line: UNIX_SOURCE_LINE.to_string(),
    });
    if probe.fish_present {
        out.push(EnvTarget::ManagedFile {
            path: home.join(".config/fish/conf.d/cfgd-env.fish"),
            content: generate_fish_env_content(env, aliases, path_dirs, origins),
            rendered: RenderedCounts::of(env, aliases),
        });
    }

    // Login (Login + All): login shells via source lines into user-owned files.
    if reaches_login(scope) {
        // zsh reads ~/.zshenv in every context, but only write it when zsh is
        // actually in use — a bash-only host would otherwise gain an inert file
        // (and a spurious write line on-camera) for a shell it never runs.
        if probe.zsh_present {
            out.push(EnvTarget::SourceLine {
                rc_path: home.join(".zshenv"),
                line: UNIX_SOURCE_LINE.to_string(),
            });
        }
        // ~/.profile is the safe sh/bash login fallback. Never create
        // ~/.bash_profile — bash reads the first existing of .bash_profile,
        // .bash_login, .profile and stops, so creating one shadows .profile.
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(".profile"),
            line: UNIX_SOURCE_LINE.to_string(),
        });
        if probe.bash_profile_exists {
            out.push(EnvTarget::SourceLine {
                rc_path: home.join(".bash_profile"),
                line: UNIX_SOURCE_LINE.to_string(),
            });
        } else if probe.bash_login_exists {
            out.push(EnvTarget::SourceLine {
                rc_path: home.join(".bash_login"),
                line: UNIX_SOURCE_LINE.to_string(),
            });
        }
    }

    // All: session-manager surfaces. FreeBSD deliberately matches neither arm —
    // it has no systemd (environment.d) and no launchd (LaunchAgent), so the
    // `.cfgd.env` + rc source lines above are its entire env surface. Emitting a
    // systemd environment.d file there would write inert state no consumer reads.
    if reaches_all(scope) {
        if platform == EnvPlatform::Linux {
            // systemd --user + Wayland GUI sessions read environment.d (KEY=VALUE).
            out.push(EnvTarget::ManagedFile {
                path: home.join(".config/environment.d/cfgd.conf"),
                content: generate_environment_d_content(env),
                rendered: RenderedCounts::of(env, &[]),
            });
        }
        if platform == EnvPlatform::MacOs {
            // A LaunchAgent that runs `launchctl setenv` at load publishes the vars into the
            // GUI session's launchd domain, so launchd-spawned GUI apps inherit them.
            let vars: BTreeMap<String, String> = valid_export_pairs(env).into_iter().collect();
            // No publishable vars ⇒ no agent: an empty `launchctl setenv` script would be an inert
            // `/bin/sh -c ""` job with nothing to set.
            if !vars.is_empty() {
                out.push(EnvTarget::ManagedFile {
                    path: home
                        .join("Library/LaunchAgents")
                        .join(MACOS_USER_PLIST_NAME),
                    content: launchd_env_plist(MACOS_USER_PLIST_LABEL, &vars),
                    rendered: RenderedCounts::vars_only(vars.len()),
                });
            }
        }
    }
}

fn windows_targets(
    content: EnvContent<'_>,
    home: &Path,
    probe: &EnvHostProbe,
    out: &mut Vec<EnvTarget>,
) {
    let EnvContent {
        env,
        aliases,
        path_dirs,
        origins,
    } = content;
    // PowerShell env file + dot-source into both profile locations.
    out.push(EnvTarget::ManagedFile {
        path: primary_env_file_path(home, EnvPlatform::Windows),
        content: generate_powershell_env_content(env, aliases, path_dirs, origins),
        rendered: RenderedCounts::of(env, aliases),
    });
    for dir in ["Documents/PowerShell", "Documents/WindowsPowerShell"] {
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(dir).join("Microsoft.PowerShell_profile.ps1"),
            line: PS_SOURCE_LINE.to_string(),
        });
    }
    // Git Bash, when present, gets the same bash env file + source line as Unix.
    if probe.git_bash_present {
        out.push(EnvTarget::ManagedFile {
            path: home.join(".cfgd.env"),
            content: generate_env_file_content(env, aliases, path_dirs, origins),
            rendered: RenderedCounts::of(env, aliases),
        });
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(".bashrc"),
            line: UNIX_SOURCE_LINE.to_string(),
        });
    }
}

/// How many entries a generated file actually holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RenderedCounts {
    pub(super) vars: usize,
    pub(super) aliases: usize,
}

impl RenderedCounts {
    /// What a shell generator will render from `env` and `aliases`: the
    /// entries whose names pass the same safety filter every generator
    /// applies before emitting a line, so the count describes the file rather
    /// than the declaration list it was derived from.
    fn of(env: &[EnvVar], aliases: &[ShellAlias]) -> Self {
        Self {
            vars: env
                .iter()
                .filter(|e| crate::validate_env_var_name(&e.name).is_ok())
                .count(),
            aliases: aliases
                .iter()
                .filter(|a| crate::validate_alias_name(&a.name).is_ok())
                .count(),
        }
    }

    /// A rendering with no alias syntax at all (`environment.d`, launchd).
    fn vars_only(vars: usize) -> Self {
        Self { vars, aliases: 0 }
    }
}

/// `(name, value)` pairs whose names pass the shell-safety filter — the same
/// filter the per-shell generators apply, centralized so every target agrees.
fn valid_export_pairs(env: &[EnvVar]) -> Vec<(String, String)> {
    env.iter()
        .filter(|e| crate::validate_env_var_name(&e.name).is_ok())
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect()
}

/// `environment.d(5)` content: `KEY=VALUE`, one per line. **Not shell** — no
/// `export`, no quoting; values are literal (systemd expands `${OTHER}` itself).
pub(super) fn generate_environment_d_content(env: &[EnvVar]) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        // Quoted, not raw: a newline in the value would otherwise end the
        // assignment and let the rest of it stand as further assignments in
        // the user's systemd environment.
        lines.push(format!(
            "{}={}",
            ev.name,
            crate::posix_single_quoted(&ev.value)
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Render a launchd LaunchAgent/Daemon plist that publishes `vars` into its launchd
/// domain by running `launchctl setenv` once per variable at load.
///
/// A plist `EnvironmentVariables` dict applies only to the job's own process, so it
/// cannot make `spec.env` reach GUI apps. `launchctl setenv` instead sets each
/// variable in the launchd domain the job runs in — the user's GUI session for a
/// LaunchAgent (`spec.env`), the system domain for a LaunchDaemon
/// (`spec.system.environment`) — so every later-spawned process inherits it. The
/// two consumers differ only by `label` and install domain.
///
/// Names that are not shell-safe identifiers are skipped (they would otherwise inject
/// into the shell command); values are shell-escaped, and the whole command is
/// XML-escaped for the plist `<string>`.
pub fn launchd_env_plist(label: &str, vars: &BTreeMap<String, String>) -> String {
    let setenv_script = vars
        .iter()
        .filter(|(key, _)| crate::validate_env_var_name(key).is_ok())
        .map(|(key, value)| {
            format!(
                "/bin/launchctl setenv {key} {}",
                crate::shell_escape_value(value)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>-c</string>
        <string>{script}</string>
    </array>
    <key>RunAtLoad</key>
    <true />
</dict>
</plist>
"#,
        label = crate::xml_escape(label),
        script = crate::xml_escape(&setenv_script),
    )
}
