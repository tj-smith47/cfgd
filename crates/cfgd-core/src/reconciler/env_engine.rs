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
    ManagedFile { path: PathBuf, content: String },
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
    Windows,
}

impl EnvPlatform {
    pub(super) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
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
}

impl EnvHostProbe {
    pub(super) fn detect(home: &Path) -> Self {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let fish_conf_d = home.join(".config/fish/conf.d");
        Self {
            shell,
            fish_present: fish_in_use() && fish_conf_d.exists(),
            bash_profile_exists: home.join(".bash_profile").exists(),
            bash_login_exists: home.join(".bash_login").exists(),
            git_bash_present: cfg!(windows) && crate::command_available("sh"),
        }
    }
}

fn reaches_login(scope: EnvScope) -> bool {
    matches!(scope, EnvScope::Login | EnvScope::All)
}

fn reaches_all(scope: EnvScope) -> bool {
    matches!(scope, EnvScope::All)
}

/// Compute the ordered list of env targets for a scope. Empty input yields no
/// targets.
pub(super) fn env_targets(
    merged_env: &[EnvVar],
    merged_aliases: &[ShellAlias],
    scope: EnvScope,
    home: &Path,
    probe: &EnvHostProbe,
    platform: EnvPlatform,
) -> Vec<EnvTarget> {
    let mut targets = Vec::new();
    if merged_env.is_empty() && merged_aliases.is_empty() {
        return targets;
    }

    match platform {
        EnvPlatform::Windows => {
            windows_targets(merged_env, merged_aliases, scope, home, probe, &mut targets)
        }
        EnvPlatform::Linux | EnvPlatform::MacOs => unix_targets(
            merged_env,
            merged_aliases,
            scope,
            home,
            probe,
            platform,
            &mut targets,
        ),
    }

    // Live-session refresh runs last, after the durable files are written.
    if reaches_all(scope) {
        let vars = valid_export_pairs(merged_env);
        if !vars.is_empty() {
            targets.push(EnvTarget::LiveSession { vars });
        }
    }

    targets
}

fn unix_targets(
    env: &[EnvVar],
    aliases: &[ShellAlias],
    scope: EnvScope,
    home: &Path,
    probe: &EnvHostProbe,
    platform: EnvPlatform,
    out: &mut Vec<EnvTarget>,
) {
    // Interactive (all scopes): the cfgd-owned env file + a source line in the
    // user's interactive rc, plus fish when it's in use.
    out.push(EnvTarget::ManagedFile {
        path: home.join(".cfgd.env"),
        content: generate_env_file_content(env, aliases),
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
            content: generate_fish_env_content(env, aliases),
        });
    }

    // Login (Login + All): login shells via source lines into user-owned files.
    if reaches_login(scope) {
        // zsh reads ~/.zshenv in every context; safe to create when absent.
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(".zshenv"),
            line: UNIX_SOURCE_LINE.to_string(),
        });
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

    // All: session-manager surfaces.
    if reaches_all(scope) {
        if platform == EnvPlatform::Linux {
            // systemd --user + Wayland GUI sessions read environment.d (KEY=VALUE).
            out.push(EnvTarget::ManagedFile {
                path: home.join(".config/environment.d/cfgd.conf"),
                content: generate_environment_d_content(env),
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
                });
            }
        }
    }
}

fn windows_targets(
    env: &[EnvVar],
    aliases: &[ShellAlias],
    _scope: EnvScope,
    home: &Path,
    probe: &EnvHostProbe,
    out: &mut Vec<EnvTarget>,
) {
    // PowerShell env file + dot-source into both profile locations.
    out.push(EnvTarget::ManagedFile {
        path: home.join(".cfgd-env.ps1"),
        content: generate_powershell_env_content(env, aliases),
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
            content: generate_env_file_content(env, aliases),
        });
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(".bashrc"),
            line: UNIX_SOURCE_LINE.to_string(),
        });
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
        lines.push(format!("{}={}", ev.name, ev.value));
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
