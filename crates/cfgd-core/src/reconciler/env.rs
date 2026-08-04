use std::collections::HashSet;

use crate::PathDisplayExt;
use crate::config::{EnvScope, MergedProfile};
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::{Printer, Role};
use crate::providers::PackageManager;
use crate::state::StateStore;

use super::env_engine::{EnvHostProbe, EnvPlatform, EnvTarget, env_targets};
use super::env_files::detect_rc_env_conflicts;
use super::format::LIVE_SESSION_RESOURCE_ID;
use super::types::{Action, EnvAction};
use super::verify::merge_module_env_aliases;

/// PATH directories cfgd recorded when it bootstrapped a package manager,
/// narrowed to the managers the desired state still names and deduped.
///
/// A bootstrapped manager (Homebrew, an npm global prefix) installs binaries
/// into a directory no login shell has on PATH yet. Feeding those directories
/// into the generated env file — rather than appending to it out of band right
/// after `bootstrap()` — gives the file a single writer, so the wholesale
/// rewrite on the second apply still holds them.
///
/// The directories come from the state store and never from a live
/// `PackageManager::path_dirs()` probe. That probe touches the machine: npm's
/// creates the global prefix directory, and brew's answer depends on which
/// prefix already exists, so before bootstrap it names the wrong one on Apple
/// Silicon. Reading a value recorded at bootstrap keeps planning and
/// verification free of both.
///
/// Only a manager cfgd itself bootstrapped has a record, so a brew the user
/// installed contributes nothing and earns no rc-file write.
///
/// Filtering to the still-named managers lets a manager dropped from the config
/// age out of the generated file instead of lingering forever.
pub(super) fn recorded_manager_path_dirs(
    state: &StateStore,
    profile: &MergedProfile,
    modules: &[ResolvedModule],
) -> Vec<String> {
    let named: HashSet<String> = crate::effective::effective_desired_packages(profile, modules)
        .into_iter()
        .map(|ep| ep.manager)
        .collect();
    if named.is_empty() {
        return Vec::new();
    }
    collect_recorded_path_dirs(state, Some(&named))
}

/// Every PATH directory cfgd recorded when it bootstrapped a package manager,
/// with no narrowing to the desired state.
///
/// Lifecycle scripts take this unfiltered view where the generated env file
/// takes the narrowed one. The env file is a durable artifact a login shell
/// reads forever, so a manager dropped from the config has to age out of it; a
/// child process's PATH lives for the length of one script, where a directory
/// belonging to a manager this profile no longer names is inert. Buying the
/// filter would mean threading the merged profile and the resolved module list
/// into the drift and on-change paths, which hold neither.
pub(crate) fn all_recorded_path_dirs(state: &StateStore) -> Vec<String> {
    collect_recorded_path_dirs(state, None)
}

fn collect_recorded_path_dirs(state: &StateStore, keep: Option<&HashSet<String>>) -> Vec<String> {
    let recorded = match state.bootstrapped_managers() {
        Ok(recorded) => recorded,
        // Losing the records degrades to the pre-bootstrap state (no PATH entry)
        // rather than failing the plan outright.
        Err(e) => {
            tracing::warn!("cannot read bootstrapped PATH directories: {e}");
            return Vec::new();
        }
    };

    let mut dirs: Vec<String> = Vec::new();
    for (manager, manager_dirs) in recorded {
        if keep.is_some_and(|keep| !keep.contains(&manager)) {
            continue;
        }
        for dir in manager_dirs {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

impl<'a> super::Reconciler<'a> {
    /// Capture the PATH directories `pm` contributes, immediately after it was
    /// bootstrapped successfully.
    ///
    /// This is the only place the reconciler probes `path_dirs()`, and it is the
    /// one instant the probe is trustworthy: the manager exists as of this call,
    /// so it reports the prefix it actually installed into instead of guessing
    /// from whatever happened to be on disk beforehand.
    ///
    /// A failed record does not fail the apply — the manager is installed either
    /// way — but it does leave the PATH entry unwritten, so it is logged.
    pub(super) fn record_bootstrap_path_dirs(&self, pm: &dyn PackageManager) {
        // The directories land in shell files that a Git-Bash and a PowerShell
        // session on the same Windows host both read, and in a state row those
        // reads are compared against.
        let dirs: Vec<String> = pm
            .path_dirs()
            .iter()
            .map(|dir| crate::to_posix_string(std::path::Path::new(dir)))
            .collect();
        // Registered before the state write, and unconditionally: the very next
        // action in this apply may be an install through a binary that just
        // landed in one of these directories, and a failed state write is no
        // reason to leave the running process unable to find it.
        crate::register_bootstrapped_path_dirs(&dirs);
        if let Err(e) = self.state.record_bootstrapped_path_dirs(pm.name(), &dirs) {
            tracing::warn!(
                "cannot record PATH directories for bootstrapped {}: {e}",
                pm.name()
            );
        }
    }

    /// Plan env file generation from merged profile + module env vars and aliases.
    /// Returns (actions, warnings) — warnings for shell rc conflicts.
    pub(super) fn plan_env(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        scope: EnvScope,
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
        path_dirs: &[String],
    ) -> (Vec<Action>, Vec<String>) {
        let home = crate::expand_tilde(std::path::Path::new("~"));
        Self::plan_env_with_home(
            profile_env,
            profile_aliases,
            scope,
            modules,
            secret_envs,
            path_dirs,
            &home,
        )
    }

    pub(super) fn plan_env_with_home(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        scope: EnvScope,
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
        path_dirs: &[String],
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

        // `path_dirs` alone is enough to warrant the file *and* its source
        // lines: a profile whose only work is bootstrapping a package manager
        // has no env vars, and without the source line no shell would ever read
        // the PATH entry that makes the manager's binaries reachable.
        if merged.is_empty() && merged_aliases.is_empty() && path_dirs.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let platform = EnvPlatform::current();
        let probe = EnvHostProbe::detect(home);
        let targets = env_targets(
            &merged,
            &merged_aliases,
            path_dirs,
            scope,
            home,
            &probe,
            platform,
        );

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
                    return Ok(format!(
                        "env:write:{}:skipped",
                        crate::to_posix_string(path)
                    ));
                }
                if let Some(parent) = path.parent()
                    && !parent.exists()
                {
                    std::fs::create_dir_all(parent)?;
                }
                crate::atomic_write_str(path, content)?;
                printer.status_simple(Role::Ok, format!("Wrote {}", path.posix()));
                // Resource-id key, not display: `to_posix_string` folds on every
                // host (unlike `posix()`, a no-op on unix), so this matches the
                // id `format_action_description` derives for the same path.
                Ok(format!("env:write:{}", crate::to_posix_string(path)))
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
                let Some(content) = super::env_files::merge_source_line(&existing, line) else {
                    // Already present as the exact desired line — nothing to write.
                    return Ok(format!(
                        "env:inject:{}:skipped",
                        crate::to_posix_string(rc_path)
                    ));
                };
                crate::ensure_parent_dir(rc_path)?;
                crate::atomic_write_str(rc_path, &content)?;
                printer.status_simple(
                    Role::Ok,
                    format!("Injected source line into {}", rc_path.posix()),
                );
                Ok(format!("env:inject:{}", crate::to_posix_string(rc_path)))
            }
            EnvAction::RefreshLiveSession { vars } => {
                let changed = crate::refresh_session_env(vars, printer);
                if changed == 0 {
                    return Ok(format!("{LIVE_SESSION_RESOURCE_ID}:skipped"));
                }
                printer.status_simple(
                    Role::Ok,
                    format!("Refreshed {changed} live session variable(s)"),
                );
                // The count belongs in the status line above, never in the id:
                // the same surface reached twice in one apply (Env phase, then
                // the late regeneration) would otherwise return two different
                // ids and be recorded as two separate results.
                Ok(LIVE_SESSION_RESOURCE_ID.to_string())
            }
        }
    }
}
