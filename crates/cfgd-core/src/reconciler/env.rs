use std::collections::HashSet;

use crate::config::{EnvScope, MergedProfile};
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::state::StateStore;

use super::env_engine::{EnvContent, EnvHostProbe, EnvPlatform, EnvTarget, env_targets};
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

/// What one env plan decided, beyond the actions themselves.
///
/// `primary_write` is present exactly when the primary managed env file's
/// rewrite survived elision, carrying both sides of that comparison so the
/// env-gated hook fold can ask a narrower question than "was anything
/// planned": whether one MODULE's own entries are what moved.
pub(super) struct EnvPlanOutcome {
    pub(super) actions: Vec<Action>,
    pub(super) warnings: Vec<String>,
    pub(super) primary_write: Option<PrimaryEnvWrite>,
}

/// Both sides of the planned primary managed env file rewrite: the deployed
/// baseline (`None` when unreadable or absent) and the content the plan
/// writes.
///
/// The primary file is the comparison surface for per-module attribution for
/// the same reason `verify_env_items` reads only it: every declared env var
/// and alias renders into it as one line per entry
/// (`primary_env_var_line` / `primary_alias_line`), so it witnesses any
/// declared entry changing, while the secondary files (fish, environment.d,
/// the LaunchAgent plist) are projections of the same declared entries in
/// dialects those helpers cannot render.
pub(super) struct PrimaryEnvWrite {
    baseline: Option<String>,
    desired: String,
    platform: EnvPlatform,
    /// The deployed baseline holds a managed line the desired rendering no
    /// longer contains AND that no current layer's declared entries account
    /// for — the shape a DELETED declaration leaves behind. The deleted
    /// entry is absent from every current declaration, so no per-module
    /// comparison can see it or say whose it was: attribution is
    /// unanswerable, and the gate fails OPEN for every deferred module.
    unclaimed_deletion: bool,
    /// The same provenance map the desired content was rendered with, so the
    /// per-entry lines this comparison re-renders are the lines that content
    /// actually holds.
    origins: super::env_engine::EnvOrigins,
}

impl PrimaryEnvWrite {
    /// Whether a module's OWN env/alias entries differ between the deployed
    /// rendering and the desired one.
    ///
    /// Presence semantics per declared entry: an entry's rendered line being
    /// in the deployed file but not the desired content (or the reverse) is
    /// the module's contribution moving — a value change shows up as the new
    /// line missing from the baseline, and a merge-shadowing change as the
    /// line appearing or disappearing. An unanswerable baseline fails OPEN:
    /// hooks revive rather than silently skip on uncertainty. An entry whose
    /// name the generator refuses renders in neither, so it cannot move.
    pub(super) fn module_env_moved(
        &self,
        env: &[crate::config::EnvVar],
        aliases: &[crate::config::ShellAlias],
    ) -> bool {
        // A deleted declaration is invisible to the per-entry comparison
        // below (it iterates only CURRENT entries), so an unclaimed
        // disappearing line makes the whole attribution unanswerable and
        // every asking module revives — same fail-open as the unreadable
        // baseline.
        if self.unclaimed_deletion {
            return true;
        }
        let Some(baseline) = &self.baseline else {
            return true;
        };
        let deployed: HashSet<&str> = baseline.lines().collect();
        let desired: HashSet<&str> = self.desired.lines().collect();
        env.iter()
            .filter_map(|ev| {
                super::env_files::primary_env_var_line(ev, self.platform, &self.origins)
            })
            .chain(aliases.iter().filter_map(|a| {
                super::env_files::primary_alias_line(a, self.platform, &self.origins)
            }))
            .any(|line| deployed.contains(line.as_str()) != desired.contains(line.as_str()))
    }
}

/// Whether `baseline` holds a managed line that `desired` no longer contains
/// and that no CURRENT declared entry (env var, alias, or the generator's own
/// PATH scaffolding) accounts for.
///
/// Claiming is by name-derived line PREFIX (`env_var_line_prefix` and
/// friends), not whole-line equality, so a line that merely CHANGED — its
/// name still declared, its value different — is claimed by its declaration
/// and left to the per-module presence comparison that already detects it.
/// Only a line whose name no current layer declares at all goes unclaimed.
/// Confined to the primary MANAGED file's content, which cfgd authors in
/// full; user-authored rc files never pass through here.
fn has_unclaimed_disappearing_line(
    baseline: &str,
    desired: &str,
    merged_env: &[crate::config::EnvVar],
    merged_aliases: &[crate::config::ShellAlias],
    platform: EnvPlatform,
) -> bool {
    let desired_lines: HashSet<&str> = desired.lines().collect();
    // Blank and comment lines are the generator's fixed scaffolding (header,
    // trailing newline), never a declaration's rendering.
    let disappeared: Vec<&str> = baseline
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !desired_lines.contains(l))
        .collect();
    if disappeared.is_empty() {
        return false;
    }
    let mut claimed: Vec<String> = merged_env
        .iter()
        .filter_map(|ev| super::env_files::env_var_line_prefix(&ev.name, platform))
        .collect();
    for alias in merged_aliases {
        claimed.extend(super::env_files::alias_line_prefixes(&alias.name, platform));
    }
    claimed.extend(super::env_files::path_dirs_line_prefix(platform));
    disappeared.iter().any(|line| {
        !claimed
            .iter()
            .any(|prefix| line.starts_with(prefix.as_str()))
    })
}

impl<'a> super::Reconciler<'a> {
    /// Plan env file generation from merged profile + module env vars and aliases.
    /// The outcome's warnings are shell rc conflicts.
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
    ) -> EnvPlanOutcome {
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
    ) -> EnvPlanOutcome {
        let (mut merged, merged_aliases, origins) =
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
            return EnvPlanOutcome {
                actions: Self::neutralize_managed_env_files(
                    scope,
                    home,
                    &probe,
                    platform,
                    managed_env_ids,
                ),
                warnings: Vec::new(),
                primary_write: None,
            };
        }

        let targets = env_targets(
            EnvContent::new(&merged, &merged_aliases, path_dirs, &origins),
            scope,
            home,
            &probe,
            platform,
        );

        // Converged surfaces are elided HERE, with the same reads
        // `apply_env_action` makes, so a plan over an unchanged machine
        // carries no env actions at all: the plan, `-o json` and the
        // env-gated module hooks that key off their presence all read
        // "converged" instead of re-planning writes the apply would only
        // report back as unchanged.
        let mut actions = Vec::new();
        let mut warnings = Vec::new();
        let mut live_session = None;
        // `env_targets` pushes the primary managed file (bash/zsh's
        // `.cfgd.env`, PowerShell's `.cfgd-env.ps1`) first whenever there is
        // anything to write, so the first `ManagedFile` this loop sees is the
        // one whose per-entry dialect the hook fold can attribute.
        let mut primary_write = None;
        let mut primary_seen = false;
        for target in targets {
            match target {
                EnvTarget::ManagedFile { path, content } => {
                    let baseline = super::env_files::read_managed_baseline(&path);
                    let converged = baseline.as_deref() == Some(content.as_str());
                    if !primary_seen {
                        primary_seen = true;
                        if !converged {
                            let unclaimed_deletion = baseline.as_deref().is_some_and(|b| {
                                has_unclaimed_disappearing_line(
                                    b,
                                    &content,
                                    &merged,
                                    &merged_aliases,
                                    platform,
                                )
                            });
                            primary_write = Some(PrimaryEnvWrite {
                                baseline,
                                desired: content.clone(),
                                platform,
                                unclaimed_deletion,
                                origins: origins.clone(),
                            });
                        }
                    }
                    if converged {
                        continue;
                    }
                    actions.push(Action::Env(EnvAction::WriteEnvFile {
                        path,
                        content,
                        vars: merged.len(),
                        aliases: merged_aliases.len(),
                    }));
                }
                EnvTarget::SourceLine { rc_path, line } => {
                    // Warn when a user-owned shell rc defines a cfgd-managed name
                    // *before* our source line (their value would win). Bash/zsh
                    // syntax only — skip on Windows PowerShell profiles. The
                    // warning describes the live rc, so it stands whether or
                    // not the source line still needs injecting.
                    if platform != EnvPlatform::Windows {
                        warnings.extend(detect_rc_env_conflicts(
                            &rc_path,
                            &merged,
                            &merged_aliases,
                        ));
                    }
                    // Already present as the exact desired line: nothing to
                    // plan. An unreadable rc fails OPEN — the action is kept,
                    // so the apply arm reports the real error instead of the
                    // plan silently claiming converged.
                    if super::env_files::read_rc_baseline(&rc_path).is_ok_and(|existing| {
                        super::env_files::merge_source_line(&existing, &line).is_none()
                    }) {
                        continue;
                    }
                    actions.push(Action::Env(EnvAction::InjectSourceLine { rc_path, line }));
                }
                EnvTarget::LiveSession { vars } => {
                    live_session = Some(vars);
                }
            }
        }
        // The broadcast follows a change: a converged file set means past
        // applies already refreshed the session with these same values, and
        // planning the refresh anyway is what held the env-gated hook fold
        // open on every converged run.
        if !actions.is_empty()
            && let Some(vars) = live_session
        {
            actions.push(Action::Env(EnvAction::RefreshLiveSession { vars }));
        }

        EnvPlanOutcome {
            actions,
            warnings,
            primary_write,
        }
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
        let targets = env_targets(
            EnvContent::new(&placeholder, &[], &[], &Default::default()),
            scope,
            home,
            probe,
            platform,
        );
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
                    vars: 0,
                    aliases: 0,
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
            EnvAction::WriteEnvFile { path, content, .. } => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EnvVar, ShellAlias};

    fn ev(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    fn ps(env: &[EnvVar], aliases: &[ShellAlias], path_dirs: &[String]) -> String {
        super::super::env_files::generate_powershell_env_content(
            env,
            aliases,
            path_dirs,
            &Default::default(),
        )
    }

    // The reconciler-level gate tests run under the host's own
    // `EnvPlatform::current()` (Unix on CI), so the PowerShell dialect of the
    // unclaimed-deletion scan is driven here explicitly against
    // `.cfgd-env.ps1`-shaped content — pure string logic, no Windows host in
    // the loop.

    #[test]
    fn a_deleted_entry_in_a_powershell_baseline_is_unclaimed() {
        let foo = ev("FOO", "bar");
        let baseline = ps(&[foo.clone(), ev("BAR", "baz")], &[], &[]);
        let desired = ps(std::slice::from_ref(&foo), &[], &[]);
        assert!(
            has_unclaimed_disappearing_line(&baseline, &desired, &[foo], &[], EnvPlatform::Windows,),
            "BAR is declared nowhere, so its deployed line is unclaimed"
        );
    }

    #[test]
    fn another_layers_addition_to_a_powershell_baseline_stays_claimed() {
        let foo = ev("FOO", "bar");
        let catn = ShellAlias {
            name: "catn".to_string(),
            command: "cat -n".to_string(),
        };
        let baseline = ps(std::slice::from_ref(&foo), &[], &[]);
        let desired = ps(std::slice::from_ref(&foo), std::slice::from_ref(&catn), &[]);
        assert!(
            !has_unclaimed_disappearing_line(
                &baseline,
                &desired,
                &[foo],
                &[catn],
                EnvPlatform::Windows,
            ),
            "an addition leaves no orphaned line behind"
        );
    }

    #[test]
    fn a_changed_powershell_value_is_claimed_across_its_quote_flip() {
        // `$env:BASE;x` renders double-quoted while a plain value renders
        // single-quoted, so a value change across that flip leaves a
        // disappearing line whose quote differs from the current
        // declaration's — only the quote-stripped name prefix ties it back.
        // Both directions, because the disappearing line is always the OLD
        // rendering and either quote can be the old one.
        let plain = ev("FOO", "bar");
        let referencing = ev("FOO", "$env:BASE;x");
        for (old, new) in [(&plain, &referencing), (&referencing, &plain)] {
            let baseline = ps(std::slice::from_ref(old), &[], &[]);
            let desired = ps(std::slice::from_ref(new), &[], &[]);
            assert!(
                !has_unclaimed_disappearing_line(
                    &baseline,
                    &desired,
                    std::slice::from_ref(new),
                    &[],
                    EnvPlatform::Windows,
                ),
                "a value change ({:?} -> {:?}) is claimed by its still-declared name, not read as a deletion",
                old.value,
                new.value
            );
        }
    }

    #[test]
    fn a_powershell_path_dirs_change_is_claimed_as_scaffolding() {
        let foo = ev("FOO", "bar");
        let baseline = ps(std::slice::from_ref(&foo), &[], &["C:/old/bin".to_string()]);
        let desired = ps(std::slice::from_ref(&foo), &[], &["C:/new/bin".to_string()]);
        assert!(
            !has_unclaimed_disappearing_line(
                &baseline,
                &desired,
                &[foo],
                &[],
                EnvPlatform::Windows,
            ),
            "the old PATH line is cfgd's own scaffolding, never a layer's deleted entry"
        );
    }
}
