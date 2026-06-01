use crate::PathDisplayExt;
use crate::config::EnvScope;
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::{Printer, Role};

use super::env_engine::{EnvHostProbe, EnvPlatform, EnvTarget, env_targets};
use super::env_files::detect_rc_env_conflicts;
use super::types::{Action, EnvAction};
use super::verify::merge_module_env_aliases;

impl<'a> super::Reconciler<'a> {
    /// Plan env file generation from merged profile + module env vars and aliases.
    /// Returns (actions, warnings) — warnings for shell rc conflicts.
    pub(super) fn plan_env(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        scope: EnvScope,
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
    ) -> (Vec<Action>, Vec<String>) {
        let home = crate::expand_tilde(std::path::Path::new("~"));
        Self::plan_env_with_home(
            profile_env,
            profile_aliases,
            scope,
            modules,
            secret_envs,
            &home,
        )
    }

    pub(super) fn plan_env_with_home(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        scope: EnvScope,
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

        let platform = EnvPlatform::current();
        let probe = EnvHostProbe::detect(home);
        let targets = env_targets(&merged, &merged_aliases, scope, home, &probe, platform);

        let mut actions = Vec::new();
        let mut warnings = Vec::new();
        for target in targets {
            match target {
                EnvTarget::ManagedFile { path, content } => {
                    actions.push(Action::Env(EnvAction::WriteEnvFile { path, content }));
                }
                EnvTarget::SourceLine { rc_path, line } => {
                    // Warn when a user-owned shell rc defines a cfgd-managed name
                    // *before* our source line (their value would win). Bash/zsh
                    // syntax only — skip on Windows PowerShell profiles.
                    if platform != EnvPlatform::Windows {
                        warnings.extend(detect_rc_env_conflicts(
                            &rc_path,
                            &merged,
                            &merged_aliases,
                        ));
                    }
                    actions.push(Action::Env(EnvAction::InjectSourceLine { rc_path, line }));
                }
                EnvTarget::LiveSession { vars } => {
                    actions.push(Action::Env(EnvAction::RefreshLiveSession { vars }));
                }
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
                if let Some(parent) = path.parent()
                    && !parent.exists()
                {
                    std::fs::create_dir_all(parent)?;
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
                if let Some(parent) = rc_path.parent()
                    && !parent.exists()
                {
                    std::fs::create_dir_all(parent)?;
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
            EnvAction::RefreshLiveSession { vars } => {
                let changed = crate::refresh_session_env(vars, printer);
                if changed == 0 {
                    return Ok("env:session:skipped".to_string());
                }
                printer.status_simple(
                    Role::Ok,
                    format!("Refreshed {changed} live session variable(s)"),
                );
                Ok(format!("env:session:{changed}"))
            }
        }
    }
}
