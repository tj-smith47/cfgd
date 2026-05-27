use crate::PathDisplayExt;
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::{Printer, Role};

use super::env_files::{
    detect_rc_env_conflicts, fish_in_use, generate_env_file_content, generate_fish_env_content,
    generate_powershell_env_content,
};
use super::types::{Action, EnvAction};
use super::verify::merge_module_env_aliases;

impl<'a> super::Reconciler<'a> {
    /// Plan env file generation from merged profile + module env vars and aliases.
    /// Returns (actions, warnings) — warnings for shell rc conflicts.
    pub(super) fn plan_env(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
    ) -> (Vec<Action>, Vec<String>) {
        let home = crate::expand_tilde(std::path::Path::new("~"));
        Self::plan_env_with_home(profile_env, profile_aliases, modules, secret_envs, &home)
    }

    pub(super) fn plan_env_with_home(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
        home: &std::path::Path,
    ) -> (Vec<Action>, Vec<String>) {
        let (mut merged, merged_aliases) =
            merge_module_env_aliases(profile_env, profile_aliases, modules);

        // Append secret-backed env vars after regular envs.
        // These are resolved secret values injected into the env file.
        for (name, value) in secret_envs {
            merged.push(crate::config::EnvVar {
                name: name.clone(),
                value: value.clone(),
            });
        }

        if merged.is_empty() && merged_aliases.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let mut actions = Vec::new();

        let warnings = if cfg!(windows) {
            // PowerShell env file — always generated on Windows
            let ps_path = home.join(".cfgd-env.ps1");
            let ps_content = generate_powershell_env_content(&merged, &merged_aliases);
            actions.push(Action::Env(EnvAction::WriteEnvFile {
                path: ps_path,
                content: ps_content,
            }));

            // Inject dot-source line into PowerShell profiles
            let ps_profile_dirs = [
                home.join("Documents/PowerShell"),
                home.join("Documents/WindowsPowerShell"),
            ];
            for profile_dir in &ps_profile_dirs {
                let profile_path = profile_dir.join("Microsoft.PowerShell_profile.ps1");
                actions.push(Action::Env(EnvAction::InjectSourceLine {
                    rc_path: profile_path,
                    line: ". ~/.cfgd-env.ps1".to_string(),
                }));
            }

            // If Git Bash is available, also generate bash env file
            if crate::command_available("sh") {
                let bash_path = home.join(".cfgd.env");
                let bash_content = generate_env_file_content(&merged, &merged_aliases);
                actions.push(Action::Env(EnvAction::WriteEnvFile {
                    path: bash_path,
                    content: bash_content,
                }));
                let bashrc = home.join(".bashrc");
                actions.push(Action::Env(EnvAction::InjectSourceLine {
                    rc_path: bashrc,
                    line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
                }));
            }

            // No rc conflict detection on Windows
            Vec::new()
        } else {
            // Unix: bash/zsh env file + source line
            let env_path = home.join(".cfgd.env");
            let content = generate_env_file_content(&merged, &merged_aliases);
            actions.push(Action::Env(EnvAction::WriteEnvFile {
                path: env_path.clone(),
                content,
            }));

            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let rc_path = if shell.contains("zsh") {
                home.join(".zshrc")
            } else {
                home.join(".bashrc")
            };
            actions.push(Action::Env(EnvAction::InjectSourceLine {
                rc_path: rc_path.clone(),
                line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
            }));

            // Check for conflicts with existing definitions in the shell rc file
            detect_rc_env_conflicts(&rc_path, &merged, &merged_aliases)
        };

        // Fish shell: only generate fish env if fish is the user's shell.
        // Windows fish lives outside $SHELL conventions — see fish_in_use().
        let fish_conf_d = home.join(".config/fish/conf.d");
        if fish_in_use() && fish_conf_d.exists() {
            let fish_path = fish_conf_d.join("cfgd-env.fish");
            let fish_content = generate_fish_env_content(&merged, &merged_aliases);
            let existing_fish = std::fs::read_to_string(&fish_path).unwrap_or_default(); // OK: file may not exist yet
            if existing_fish != fish_content {
                actions.push(Action::Env(EnvAction::WriteEnvFile {
                    path: fish_path,
                    content: fish_content,
                }));
            }
        }

        (actions, warnings)
    }

    pub(super) fn apply_env_action(action: &EnvAction, printer: &Printer) -> Result<String> {
        match action {
            EnvAction::WriteEnvFile { path, content } => {
                let existing = match std::fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => {
                        tracing::warn!("cannot read {}: {e}", path.posix());
                        String::new()
                    }
                };
                if existing == *content {
                    return Ok(format!("env:write:{}:skipped", path.display()));
                }
                crate::atomic_write_str(path, content)?;
                printer.status_simple(Role::Ok, format!("Wrote {}", path.posix()));
                Ok(format!("env:write:{}", path.display()))
            }
            EnvAction::InjectSourceLine { rc_path, line } => {
                let existing = match std::fs::read_to_string(rc_path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => {
                        tracing::warn!("cannot read {}: {e}", rc_path.posix());
                        String::new()
                    }
                };
                if existing.contains(line) {
                    // Already injected
                    return Ok(format!("env:inject:{}:skipped", rc_path.display()));
                }
                let mut content = existing;
                if !content.ends_with('\n') && !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(line);
                content.push('\n');
                crate::atomic_write_str(rc_path, &content)?;
                printer.status_simple(
                    Role::Ok,
                    format!("Injected source line into {}", rc_path.posix()),
                );
                Ok(format!("env:inject:{}", rc_path.display()))
            }
        }
    }
}
