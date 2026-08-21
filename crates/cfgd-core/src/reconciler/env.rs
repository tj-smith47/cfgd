use std::collections::HashSet;

use crate::config::{EnvScope, MergedProfile};
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::state::StateStore;

use super::env_engine::{EnvHostProbe, EnvPlatform, EnvTarget, env_targets};
use super::env_files::detect_rc_env_conflicts;
use super::format::LIVE_SESSION_RESOURCE_ID;
use super::types::{Action, EnvAction};
use super::verify::merge_module_env_aliases;

/// PATH directories cfgd recorded as its own for a package manager, narrowed
/// to the managers the desired state still names and deduped.
///
/// A manager cfgd provisioned (Homebrew, an npm global prefix) installs
/// binaries into a directory no login shell has on PATH yet. Feeding those
/// directories into the generated env file — rather than appending to it out of
/// band right after `bootstrap()` — gives the file a single writer, so the
/// wholesale rewrite on the second apply still holds them.
///
/// The directories come from the state store and never from a live
/// `PackageManager::path_dirs()` call. That call can touch the machine: npm's
/// spawns npm to ask where its global prefix is, which every reconcile tick
/// would then pay for. Reading a recorded value keeps planning and verification
/// free of it.
///
/// A record exists for a manager cfgd bootstrapped AND for one whose install
/// had to create a prefix of its own (`PackageManager::created_path_dirs`) —
/// the discriminator is who created the directory, not who installed the
/// manager. So cfgd's own `~/.npm-global` reaches the env file under a
/// user-installed npm, while a brew the user installed contributes nothing and
/// earns no rc-file write, because nothing under it is cfgd's to publish.
///
/// Filtering to the still-named managers lets a manager dropped from the config
/// age out of the generated file instead of lingering forever.
pub fn recorded_manager_path_dirs(
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

/// The env-surface resource ids cfgd's own state records it manages.
///
/// The gate on emptying a generated env file: a file carrying cfgd's header is
/// evidence some cfgd wrote it, but not evidence THIS installation did. Only a
/// resource this state store recorded applying may be stripped, so a home
/// directory reached from a machine (or container) with a fresh state store is
/// left exactly as found instead of blanked.
///
/// The granularity is the resource TYPE, not the verb: a recorded env id keeps
/// the path and drops the `write`/`inject` distinction, so the paths of
/// user-owned rc files are in this list too. Anything that treats a member as a
/// file cfgd may rewrite in full has to exclude the rc paths itself.
pub(super) fn recorded_managed_env_files(state: &StateStore) -> Vec<String> {
    match state.managed_resources() {
        Ok(rows) => rows
            .into_iter()
            .filter(|r| r.resource_type == "env")
            .map(|r| r.resource_id)
            .collect(),
        Err(e) => {
            tracing::warn!("cannot read managed env resources: {e}");
            Vec::new()
        }
    }
}

/// Every PATH directory cfgd recorded as its own for a package manager, with
/// no narrowing to the desired state.
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
    /// Plan env file generation from merged profile + module env vars and aliases.
    /// Returns (actions, warnings) — warnings for shell rc conflicts.
    ///
    /// A method, not a free function, because the home directory the env
    /// surfaces hang off is the reconciler's — resolved once at construction.
    /// The moment a planning entry point resolves `~` for itself, a caller that
    /// pinned a home no longer controls where the plan writes.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan_env(
        &self,
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        scope: EnvScope,
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
        path_dirs: &[String],
        managed_env_ids: &[String],
    ) -> (Vec<Action>, Vec<String>) {
        Self::plan_env_with_home(
            profile_env,
            profile_aliases,
            scope,
            modules,
            secret_envs,
            path_dirs,
            managed_env_ids,
            &self.home,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan_env_with_home(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        scope: EnvScope,
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
        path_dirs: &[String],
        managed_env_ids: &[String],
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

        let platform = EnvPlatform::current();
        let probe = EnvHostProbe::detect(home);

        // `path_dirs` alone is enough to warrant the file *and* its source
        // lines: a profile whose only work is bootstrapping a package manager
        // has no env vars, and without the source line no shell would ever read
        // the PATH entry that makes the manager's binaries reachable.
        if merged.is_empty() && merged_aliases.is_empty() && path_dirs.is_empty() {
            return (
                Self::neutralize_managed_env_files(scope, home, &probe, platform, managed_env_ids),
                Vec::new(),
            );
        }

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

    /// Reduce every cfgd-generated env file that still exists to its header,
    /// for a desired state that now declares no env vars, aliases, or PATH
    /// directories.
    ///
    /// Emptying `spec.env` otherwise leaves the last generated file on disk,
    /// and every login shell keeps exporting the values the user just deleted
    /// from their config — the deletion never takes effect. Stripping the body
    /// makes it take effect. The `-f`-guarded source line in the user's rc is
    /// left alone: it now loads a file that sets nothing, and removing a line
    /// from a user-owned dotfile is a separate, destructive-by-nature action.
    ///
    /// Confined to the paths in `managed_env_ids` — the env resources this
    /// state store recorded applying — so the one action that deletes a user's
    /// settings needs cfgd's own record that it wrote them, not merely a header
    /// line that says some cfgd once did.
    ///
    /// `managed_env_ids` holds every env resource, injections included, because
    /// the recorded id keeps the path and drops the verb. A user-owned rc file
    /// is therefore in that set, and the only thing standing between it and a
    /// header-only rewrite would be the fact that no generator emits an rc path
    /// as a managed file. That is true, and it is decided in another module, so
    /// the rc paths of this very target set are subtracted here instead: the
    /// exclusion is then local, and holds even if the two path families ever
    /// overlap.
    fn neutralize_managed_env_files(
        scope: EnvScope,
        home: &std::path::Path,
        probe: &EnvHostProbe,
        platform: EnvPlatform,
        managed_env_ids: &[String],
    ) -> Vec<Action> {
        // The generated bodies are discarded — this call is for the target
        // PATHS, which is why one placeholder variable is enough to get past
        // the "nothing to write" gate inside `env_targets`. Every generator
        // opens with the same header, so the emptied form of all of them is
        // that header alone.
        let placeholder = [crate::config::EnvVar {
            name: "CFGD_MANAGED_ENV".to_string(),
            value: String::new(),
        }];
        let neutral = format!("{}\n", super::env_files::ENV_FILE_HEADER);
        let targets = env_targets(&placeholder, &[], &[], scope, home, probe, platform);
        let rc_paths: HashSet<String> = targets
            .iter()
            .filter_map(|target| match target {
                EnvTarget::SourceLine { rc_path, .. } => Some(crate::to_posix_string(rc_path)),
                _ => None,
            })
            .collect();
        targets
            .into_iter()
            .filter_map(|target| match target {
                EnvTarget::ManagedFile { path, .. } => Some(path),
                _ => None,
            })
            .filter(|path| {
                let key = crate::to_posix_string(path);
                managed_env_ids.contains(&key) && !rc_paths.contains(&key)
            })
            .filter(|path| {
                // Only a file cfgd's own generator wrote, and only while it
                // still carries a body. The header check also excludes the
                // macOS LaunchAgent plist, which the placeholder above puts in
                // the target set and whose XML a header line would corrupt.
                std::fs::read_to_string(path).is_ok_and(|body| {
                    body.starts_with(super::env_files::ENV_FILE_HEADER) && body != neutral
                })
            })
            .map(|path| {
                Action::Env(EnvAction::WriteEnvFile {
                    path,
                    content: neutral.clone(),
                })
            })
            .collect()
    }

    pub(super) fn apply_env_action(
        action: &EnvAction,
        printer: &Printer,
        notes: &crate::providers::NoteSink,
    ) -> Result<String> {
        match action {
            EnvAction::WriteEnvFile { path, content } => {
                if super::env_files::read_managed_baseline(path).as_ref() == Some(content) {
                    return Ok(format!(
                        "env:write:{}{}",
                        crate::to_posix_string(path),
                        super::apply::ENV_SKIPPED_SUFFIX
                    ));
                }
                crate::ensure_parent_dir(path)?;
                crate::atomic_write_resolved_str(path, content)?;
                // Resource-id key, not display: `to_posix_string` folds on every
                // host (unlike `posix()`, a no-op on unix), so this matches the
                // id `format_action_description` derives for the same path.
                Ok(format!("env:write:{}", crate::to_posix_string(path)))
            }
            EnvAction::InjectSourceLine { rc_path, line } => {
                let existing = super::env_files::read_rc_baseline(rc_path)?;
                let Some(content) = super::env_files::merge_source_line(&existing, line) else {
                    // Already present as the exact desired line — nothing to write.
                    return Ok(format!(
                        "env:inject:{}{}",
                        crate::to_posix_string(rc_path),
                        super::apply::ENV_SKIPPED_SUFFIX
                    ));
                };
                super::env_files::guard_rc_write(rc_path, &existing)?;
                crate::ensure_parent_dir(rc_path)?;
                crate::atomic_write_resolved_str(rc_path, &content)?;
                Ok(format!("env:inject:{}", crate::to_posix_string(rc_path)))
            }
            EnvAction::RefreshLiveSession { vars } => {
                let refresh = crate::refresh_session_env(vars);
                for failure in refresh.failures {
                    notes.report(printer, crate::output::Role::Warn, failure);
                }
                if refresh.changed == 0 {
                    // `unavailable > 0` means no session manager answered for
                    // at least one variable — distinct from every variable
                    // already holding its desired value, so the apply tree
                    // does not report "unchanged" for a surface that was
                    // never reachable in the first place.
                    let suffix = if refresh.unavailable > 0 {
                        super::apply::ENV_NO_SESSION_MANAGER_SUFFIX
                    } else {
                        super::apply::ENV_SKIPPED_SUFFIX
                    };
                    return Ok(format!("{LIVE_SESSION_RESOURCE_ID}{suffix}"));
                }
                // The changed count is not the id: the same surface reached
                // twice in one apply (Env phase, then the late regeneration)
                // would otherwise return two different ids and be recorded as
                // two separate results.
                Ok(LIVE_SESSION_RESOURCE_ID.to_string())
            }
        }
    }
}
