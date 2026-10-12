use std::collections::HashSet;

use crate::config::{EnvScope, MergedProfile, ScriptEntry, ScriptSpec};
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::state::StateStore;

use super::env_engine::{
    EnvContent, EnvHostProbe, EnvPlatform, EnvTarget, ManagerPathDir, env_targets,
};
use super::env_files::detect_rc_env_conflicts;
use super::format::LIVE_SESSION_RESOURCE_ID;
use super::types::{Action, EnvAction};
use super::verify::LayeredEnv;

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
) -> Vec<ManagerPathDir> {
    let named: HashSet<String> =
        crate::effective::effective_desired_packages(profile, modules, None)
            .into_iter()
            .map(|ep| ep.manager)
            .collect();
    if named.is_empty() {
        return Vec::new();
    }
    collect_recorded_path_dirs(state, Some(&named))
}

/// What one module asks of the home directory: the file targets it declares,
/// whether it declares any env var or alias, the script entries an apply runs
/// for it, the `creates:` guards of its script installs and its system
/// configurator settings.
pub struct ModuleHomeUse<'a> {
    pub targets: Vec<&'a std::path::Path>,
    pub declares_env: bool,
    /// Its `preApply`, `postApply` and `onChange` entries, the hooks an apply runs.
    pub scripts: Vec<&'a ScriptEntry>,
    /// The `creates:` guard of each package it installs with its own script.
    pub install_creates: Vec<&'a str>,
    pub system: Option<&'a crate::config::SystemSettings>,
}

impl<'a> From<&'a ResolvedModule> for ModuleHomeUse<'a> {
    fn from(module: &'a ResolvedModule) -> Self {
        Self {
            targets: module.files.iter().map(|f| f.target.as_path()).collect(),
            declares_env: !module.env.is_empty() || !module.aliases.is_empty(),
            scripts: module
                .pre_apply_scripts
                .iter()
                .chain(&module.post_apply_scripts)
                .chain(&module.on_change_scripts)
                .collect(),
            install_creates: module
                .packages
                .iter()
                .filter_map(|p| p.creates.as_deref())
                .collect(),
            system: Some(&module.system),
        }
    }
}

impl<'a> ModuleHomeUse<'a> {
    /// The same view of a module read off disk and not yet resolved, judged
    /// on `platform` the way resolution judges it: a gated-off module asks
    /// nothing, and only the env entries, aliases and packages applicable
    /// here count. A package counts its `creates:` guard when `script` is
    /// anywhere in its `prefer` list: which entry resolution settles on turns
    /// on the managers the host has, and a homeless run refused for a guard
    /// its script never reads loses nothing, where one that runs the script
    /// stops half way through its plan.
    pub fn of_loaded(
        module: &'a crate::modules::LoadedModule,
        platform: &crate::platform::Platform,
    ) -> Self {
        use crate::platform::{PlatformGated, applicable_here};
        if !module.spec.applies_to(platform) {
            return Self {
                targets: Vec::new(),
                declares_env: false,
                scripts: Vec::new(),
                install_creates: Vec::new(),
                system: None,
            };
        }
        Self {
            targets: module
                .spec
                .files
                .iter()
                .map(|f| std::path::Path::new(f.target.as_str()))
                .collect(),
            declares_env: applicable_here(&module.spec.env, platform).next().is_some()
                || applicable_here(&module.spec.aliases, platform)
                    .next()
                    .is_some(),
            scripts: module
                .spec
                .scripts
                .iter()
                .flat_map(apply_hook_entries)
                .collect(),
            install_creates: module
                .spec
                .packages
                .iter()
                .filter(|p| p.applies_to(platform))
                .filter(|p| p.prefer.iter().any(|m| m == crate::SCRIPT_SENTINEL))
                .filter_map(|p| p.creates.as_deref())
                .collect(),
            system: Some(&module.spec.system),
        }
    }
}

/// The entries of the hooks an apply runs: `preApply`, `postApply` and
/// `onChange`.
fn apply_hook_entries(spec: &ScriptSpec) -> impl Iterator<Item = &ScriptEntry> {
    spec.pre_apply
        .iter()
        .chain(&spec.post_apply)
        .chain(&spec.on_change)
}

/// Which question refuses a [`HOME_REFUSAL_SLOTS`] role up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeQuestioner {
    /// [`unresolved_home_refusal`], which `apply`, `plan` and `init --apply`
    /// ask before they open the state store.
    UnresolvedHomeRefusal,
    /// The check `compliance` and `compliance export` run before they open
    /// the state store.
    Compliance,
}

/// The roles [`expand_tilde`](crate::expand_tilde) names on a path a homeless
/// run refuses before it writes anything, each paired with the slot it fills
/// and the [`HomeQuestioner`] that asks it. The slot is the name of the
/// population pin's fixture that proves the refusal leaves the store as it
/// found it. Every other role carries `// home-refusal-ok: <why>` at its call
/// site, naming why no question asks it up front.
pub const HOME_REFUSAL_SLOTS: &[(&str, &str, HomeQuestioner)] = &{
    use HomeQuestioner::{Compliance, UnresolvedHomeRefusal as Run};
    [
        (crate::FILE_TARGET_ROLE, "file target", Run),
        ("decision path", "file target", Run),
        (crate::SECRET_TARGET_ROLE, "secret target", Run),
        ("backup source", "backup source", Run),
        ("backup destination", "backup destination", Run),
        (crate::SCRIPT_WORKDIR_ROLE, "script workdir", Run),
        (crate::CREATES_GUARD_ROLE, "creates guard", Run),
        ("env value", "env var", Run),
        ("age key", "age key", Run),
        ("SSH key path", "SSH key", Run),
        ("systemd unit file", "systemd unit file", Run),
        ("KDE config file", "KDE config file", Run),
        ("launch agent plist", "launch agent", Run),
        ("watch path", "compliance watch path", Compliance),
        (
            "compliance export directory",
            "compliance export directory",
            Compliance,
        ),
    ]
};

/// The refusal a run over this desired state meets when no home directory
/// resolves, or `None` when nothing it declares writes under one.
///
/// A declared file or secret target leading with `~` refuses as
/// [`StateError::HomeUnresolved`]. So do a `~` source or destination of a
/// backup unit `apply` snapshots, a `~` `workdir:` or `creates:` guard on a
/// script the apply runs (a profile or module `preApply`, `postApply` or
/// `onChange` entry, or a module package's install script), a path a system
/// configurator the run drives reads under `~`
/// ([`SystemConfigurator::home_refusal`]), and a secret the run decrypts with
/// a secret backend whose key needs the missing home
/// ([`SecretBackend::home_refusal`]). The configurators asked are those this
/// host can run or whose tool the run can install first. Env vars, aliases,
/// secret env exports and a recorded manager `PATH` entry refuse as
/// [`StateError::EnvHomeUnresolved`], because they live in the managed env
/// file. The plan, `apply` and `plan` before they open the state store, and
/// `doctor` all ask it, so `doctor` fails its verdict exactly when a run
/// refuses. [`HOME_REFUSAL_SLOTS`] names each of its questions by the role
/// the path's expansion carries.
///
/// [`SecretBackend::home_refusal`]: crate::providers::SecretBackend::home_refusal
/// [`SystemConfigurator::home_refusal`]: crate::providers::SystemConfigurator::home_refusal
///
/// [`StateError::HomeUnresolved`]: crate::errors::StateError::HomeUnresolved
/// [`StateError::EnvHomeUnresolved`]: crate::errors::StateError::EnvHomeUnresolved
pub fn unresolved_home_refusal<'a>(
    state: Option<&StateStore>,
    profile: &MergedProfile,
    modules: impl IntoIterator<Item = ModuleHomeUse<'a>>,
    registry: Option<&crate::providers::ProviderRegistry>,
) -> Option<crate::errors::StateError> {
    let modules: Vec<ModuleHomeUse<'a>> = modules.into_iter().collect();
    let declared_target = profile
        .files
        .managed
        .iter()
        .map(|f| f.target.as_path())
        .chain(profile.secrets.iter().filter_map(|s| s.target.as_deref()))
        .chain(modules.iter().flat_map(|m| m.targets.iter().copied()))
        .find_map(|t| crate::home_refusal_of(crate::expand_tilde(t, crate::FILE_TARGET_ROLE)));
    if declared_target.is_some() {
        return declared_target;
    }
    // A unit `apply` snapshots expands its source and destination when it is
    // bound, after the store has opened.
    let backup_path = profile
        .backups
        .iter()
        .filter(|spec| crate::backup::runs_with_apply(spec))
        .find_map(|spec| crate::home_refusal_of(crate::backup::BackupUnit::declared_paths(spec)));
    if backup_path.is_some() {
        return backup_path;
    }
    // A script resolves its `workdir:` and `creates:` when it runs, in the
    // middle of the apply.
    let script_path = apply_hook_entries(&profile.scripts)
        .chain(modules.iter().flat_map(|m| m.scripts.iter().copied()))
        .find_map(|entry| {
            let ScriptEntry::Full(command) = entry else {
                return None;
            };
            let workdir = command.workdir.as_deref().and_then(|raw| {
                crate::home_refusal_of(crate::expand_tilde(
                    std::path::Path::new(raw),
                    crate::SCRIPT_WORKDIR_ROLE,
                ))
            });
            workdir.or_else(|| {
                command.creates.as_deref().and_then(|raw| {
                    crate::home_refusal_of(crate::expand_tilde(
                        std::path::Path::new(raw),
                        crate::CREATES_GUARD_ROLE,
                    ))
                })
            })
        })
        .or_else(|| {
            modules
                .iter()
                .flat_map(|m| m.install_creates.iter())
                .find_map(|raw| {
                    crate::home_refusal_of(crate::expand_tilde(
                        std::path::Path::new(raw),
                        crate::CREATES_GUARD_ROLE,
                    ))
                })
        });
    if script_path.is_some() {
        return script_path;
    }
    if let Some(registry) = registry {
        let mut system = profile.system.clone();
        for module_system in modules.iter().filter_map(|m| m.system) {
            for (key, value) in module_system {
                crate::deep_merge_yaml(
                    system.entry(key.clone()).or_insert(serde_yaml::Value::Null),
                    value,
                );
            }
        }
        // The planner configures an unavailable configurator in this run when
        // the bootstrap installs its tool first.
        let configured = registry
            .system_configurators()
            .iter()
            .filter(|c| c.required_tool().is_some() || c.is_available())
            .find_map(|c| c.home_refusal(system.get(c.name())?));
        if configured.is_some() {
            return configured;
        }
    }
    // The planner's own split: a source naming no provider is a file the
    // backend decrypts, and only one with a target is planned at all.
    let decrypts = profile.secrets.iter().any(|s| {
        s.target.is_some() && crate::providers::parse_secret_reference(&s.source).is_none()
    });
    if decrypts
        && let Some(refusal) = registry
            .and_then(|r| r.secret_backend.as_deref())
            .and_then(|b| b.home_refusal())
    {
        return Some(refusal);
    }
    let writes_env_file = !profile.env.is_empty()
        || !profile.aliases.is_empty()
        || modules.iter().any(|m| m.declares_env)
        || profile
            .secrets
            .iter()
            .any(|s| s.envs.as_ref().is_some_and(|e| !e.is_empty()))
        || state.is_some_and(|state| !all_recorded_path_dirs(state).is_empty());
    writes_env_file.then(|| crate::errors::StateError::EnvHomeUnresolved {
        path: super::primary_env_file(std::path::Path::new("~")),
    })
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
            // tracing-ok: a state read that degrades to an empty set; nothing printed asks about it
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
        .into_iter()
        .map(|d| d.dir)
        .collect()
}

fn collect_recorded_path_dirs(
    state: &StateStore,
    keep: Option<&HashSet<String>>,
) -> Vec<ManagerPathDir> {
    let recorded = match state.bootstrapped_managers() {
        Ok(recorded) => recorded,
        // Losing the records degrades to the pre-bootstrap state (no PATH entry)
        // rather than failing the plan outright.
        Err(e) => {
            // tracing-ok: same, degrading to the pre-bootstrap PATH
            tracing::warn!("cannot read bootstrapped PATH directories: {e}");
            return Vec::new();
        }
    };

    let mut dirs: Vec<ManagerPathDir> = Vec::new();
    for (manager, manager_dirs) in recorded {
        if keep.is_some_and(|keep| !keep.contains(&manager)) {
            continue;
        }
        for dir in manager_dirs {
            // First claimant of a directory keeps it: two managers sharing a
            // prefix publish one PATH entry, and the comment beside it names
            // whichever of them the record listed first.
            if !dirs.iter().any(|d| d.dir == dir) {
                dirs.push(ManagerPathDir {
                    manager: manager.clone(),
                    dir,
                });
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
    /// The same folded `PATH` assignment that content was rendered with. A
    /// declared `PATH` does not render a line of its own — it is one producer
    /// of the file's single `PATH` line — so re-rendering it without the fold
    /// would look for a line no file has ever held, and a module whose only
    /// change is its `PATH` entry would never be seen to have moved.
    path: Option<super::env_engine::FoldedPath>,
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
    ) -> crate::errors::Result<bool> {
        // A deleted declaration is invisible to the per-entry comparison
        // below (it iterates only CURRENT entries), so an unclaimed
        // disappearing line makes the whole attribution unanswerable and
        // every asking module revives — same fail-open as the unreadable
        // baseline.
        if self.unclaimed_deletion {
            return Ok(true);
        }
        let Some(baseline) = &self.baseline else {
            return Ok(true);
        };
        // Compared WITHOUT the trailing owner comment on any of the three
        // sides: a baseline written before owner comments existed carries
        // none, and comparing raw would read every entry in such a file as
        // having moved — running every module's `onChange` once for a
        // rewrite that changed no value.
        use super::env_engine::without_owner_comment;
        let deployed: HashSet<&str> = baseline.lines().map(without_owner_comment).collect();
        let desired: HashSet<&str> = self.desired.lines().map(without_owner_comment).collect();
        let env_lines = env
            .iter()
            .map(|ev| super::env_files::primary_env_var_line(ev, self.platform, self.path.as_ref()))
            .collect::<crate::errors::Result<Vec<_>>>()?;
        Ok(env_lines
            .into_iter()
            .flatten()
            .chain(
                aliases
                    .iter()
                    .filter_map(|a| super::env_files::primary_alias_line(a, self.platform)),
            )
            .any(|line| {
                let line = without_owner_comment(&line);
                deployed.contains(line) != desired.contains(line)
            }))
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
///
/// A block header is scaffolding, its `(priority N)` suffix included: it is
/// the generator's own text naming the layer the lines below it came from.
/// No declaration renders it, so it is skipped with the banner and the
/// blank lines. A layer that disappears takes its header with it, and the
/// entries under it are what answer for the change.
fn has_unclaimed_disappearing_line(
    baseline: &str,
    desired: &str,
    merged_env: &[crate::config::EnvVar],
    merged_aliases: &[crate::config::ShellAlias],
    platform: EnvPlatform,
) -> bool {
    let desired_lines: HashSet<&str> = desired.lines().collect();
    // Blank and comment lines are the generator's own scaffolding: the banner,
    // each block header, the trailing newline.
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
    /// Plan env file generation from the layered env view of a profile and its
    /// modules. The outcome's warnings are shell rc conflicts.
    ///
    /// A method, not a free function, because the home directory the env
    /// surfaces hang off is the reconciler's — resolved once at construction.
    /// The moment a planning entry point resolves `~` for itself, a caller that
    /// pinned a home no longer controls where the plan writes.
    pub(super) fn plan_env(
        &self,
        layered: LayeredEnv,
        scope: EnvScope,
        secret_envs: &[(String, String)],
        path_dirs: &[ManagerPathDir],
        managed_env_ids: &[String],
    ) -> crate::errors::Result<EnvPlanOutcome> {
        let Some(home) = self.home.as_deref() else {
            // With no home the managed env files cannot be located, so there
            // is nothing to neutralize either; only a run that would WRITE one
            // has to refuse.
            if layered.merged.is_empty()
                && layered.merged_aliases.is_empty()
                && path_dirs.is_empty()
                && secret_envs.is_empty()
            {
                return Ok(EnvPlanOutcome {
                    actions: Vec::new(),
                    warnings: Vec::new(),
                    primary_write: None,
                });
            }
            return Err(crate::errors::StateError::EnvHomeUnresolved {
                path: super::primary_env_file(std::path::Path::new("~")),
            }
            .into());
        };
        Self::plan_env_with_home(
            layered,
            scope,
            secret_envs,
            path_dirs,
            managed_env_ids,
            home,
        )
    }

    pub(super) fn plan_env_with_home(
        layered: LayeredEnv,
        scope: EnvScope,
        secret_envs: &[(String, String)],
        path_dirs: &[ManagerPathDir],
        managed_env_ids: &[String],
        home: &std::path::Path,
    ) -> crate::errors::Result<EnvPlanOutcome> {
        // The secret exports are a layer of their own AND winners of the merge:
        // their values exist only once a backend has resolved them, so they
        // reach both halves here, outside the layer merge.
        let mut layered = layered.with_secret_envs(secret_envs);
        for (name, value) in secret_envs {
            layered.merged.push(crate::config::EnvVar {
                name: name.clone(),
                value: value.clone(),
                platforms: vec![],
            });
        }

        let platform = EnvPlatform::current();
        let probe = EnvHostProbe::detect(home);

        // `path_dirs` alone is enough to warrant the file *and* its source
        // lines: a profile whose only work is bootstrapping a package manager
        // has no env vars, and without the source line no shell would ever read
        // the PATH entry that makes the manager's binaries reachable.
        if layered.merged.is_empty() && layered.merged_aliases.is_empty() && path_dirs.is_empty() {
            return Ok(EnvPlanOutcome {
                actions: Self::neutralize_managed_env_files(
                    scope,
                    home,
                    &probe,
                    platform,
                    managed_env_ids,
                )?,
                warnings: Vec::new(),
                primary_write: None,
            });
        }

        let targets = env_targets(
            EnvContent::of(&layered, path_dirs),
            scope,
            home,
            &probe,
            platform,
        )?;
        let LayeredEnv {
            merged,
            merged_aliases,
            origins,
            ..
        } = &layered;

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
                EnvTarget::ManagedFile {
                    path,
                    content,
                    rendered,
                } => {
                    let baseline = super::env_files::read_managed_baseline(&path);
                    let converged = baseline.as_deref() == Some(content.as_str());
                    if !primary_seen {
                        primary_seen = true;
                        if !converged {
                            let unclaimed_deletion = baseline.as_deref().is_some_and(|b| {
                                has_unclaimed_disappearing_line(
                                    b,
                                    &content,
                                    merged,
                                    merged_aliases,
                                    platform,
                                )
                            });
                            primary_write = Some(PrimaryEnvWrite {
                                baseline,
                                desired: content.clone(),
                                platform,
                                unclaimed_deletion,
                                path: super::env_engine::primary_folded_path(
                                    merged, path_dirs, origins, home, platform,
                                )?,
                            });
                        }
                    }
                    if converged {
                        continue;
                    }
                    actions.push(Action::Env(EnvAction::WriteEnvFile {
                        path,
                        content,
                        vars: rendered.vars,
                        aliases: rendered.aliases,
                    }));
                }
                EnvTarget::SourceLine { rc_path, line } => {
                    // Warn when a user-owned shell rc defines a cfgd-managed name
                    // *before* our source line (their value would win). Bash/zsh
                    // syntax only — skip on Windows PowerShell profiles. The
                    // warning describes the live rc, so it stands whether or
                    // not the source line still needs injecting.
                    if platform != EnvPlatform::Windows {
                        warnings.extend(detect_rc_env_conflicts(&rc_path, merged, merged_aliases));
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

        Ok(EnvPlanOutcome {
            actions,
            warnings,
            primary_write,
        })
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
    ) -> crate::errors::Result<Vec<Action>> {
        // The generated bodies are discarded — this call is for the target
        // PATHS, which is why one placeholder variable is enough to get past
        // the "nothing to write" gate inside `env_targets`. Every generator
        // opens with the same banner, so the emptied form of all of them is
        // that banner alone: a file with no blocks left takes the banner's
        // block-free form.
        let placeholder = LayeredEnv::from_parts(
            &crate::reconciler::Owner::profile("cfgd").token(),
            &[crate::config::EnvVar {
                name: crate::CFGD_MANAGED_ENV_ENV.to_string(),
                value: String::new(),
                platforms: vec![],
            }],
            &[],
            &[],
        );
        let neutral = format!("{}\n", super::env_files::banner("#", false).join("\n"));
        let targets = env_targets(
            EnvContent::of(&placeholder, &[]),
            scope,
            home,
            probe,
            platform,
        )?;
        let rc_paths: HashSet<String> = targets
            .iter()
            .filter_map(|target| match target {
                EnvTarget::SourceLine { rc_path, .. } => Some(crate::to_posix_string(rc_path)),
                _ => None,
            })
            .collect();
        Ok(targets
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
            .collect())
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
                        "env:{}:{}{}",
                        super::env_engine::ENV_VERB_WRITE,
                        crate::to_posix_string(path),
                        super::apply::ENV_SKIPPED_SUFFIX
                    ));
                }
                crate::ensure_parent_dir(path)?;
                crate::atomic_write_resolved_str(path, content)?;
                // Resource-id key, not display: `to_posix_string` folds on every
                // host (unlike `posix()`, a no-op on unix), so this matches the
                // id `format_action_description` derives for the same path.
                Ok(format!(
                    "env:{}:{}",
                    super::env_engine::ENV_VERB_WRITE,
                    crate::to_posix_string(path)
                ))
            }
            EnvAction::InjectSourceLine { rc_path, line } => {
                if !super::env_files::inject_rc_source_line(rc_path, line)? {
                    // Already present as the exact desired line — nothing to write.
                    return Ok(format!(
                        "env:{}:{}{}",
                        super::env_engine::ENV_VERB_INJECT,
                        crate::to_posix_string(rc_path),
                        super::apply::ENV_SKIPPED_SUFFIX
                    ));
                }
                Ok(format!(
                    "env:{}:{}",
                    super::env_engine::ENV_VERB_INJECT,
                    crate::to_posix_string(rc_path)
                ))
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
    use super::super::env_engine::{EnvOrigins, FoldedPath};
    use super::*;
    use crate::config::{EnvVar, ShellAlias};

    fn ev(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: name.to_string(),
            value: value.to_string(),
            platforms: vec![],
        }
    }

    /// A generated file written before the layered blocks: every line loses
    /// its ` # owner` tail at once. Each old line is still claimed by its own
    /// declaration's prefix, so the rewrite is an ordinary content update. A
    /// file full of deletions would be one cfgd refuses to own.
    #[test]
    fn a_commented_baseline_upgrades_to_blocks_without_an_unclaimed_deletion() {
        let foo = ev("FOO", "bar");
        let catn = ShellAlias {
            name: "catn".to_string(),
            command: "cat -n".to_string(),
            platforms: vec![],
        };
        let dirs = [ManagerPathDir::new("brew", "/opt/homebrew/bin")];
        let owners = {
            let mut o = crate::config::EntryOwners::default();
            o.claim(
                "profile:base",
                std::slice::from_ref(&foo),
                std::slice::from_ref(&catn),
            );
            EnvOrigins::from_owners(&o)
        };

        for platform in [EnvPlatform::Linux, EnvPlatform::Windows] {
            // What an older cfgd wrote: one banner line, then one commented
            // line per entry. Rendered through the same `Dialect` so the two
            // shapes cannot drift apart in anything but the scaffolding.
            let legacy = |origins: &EnvOrigins| {
                let dialect = super::super::env_files::Dialect::of(platform);
                [
                    super::super::env_files::ENV_FILE_HEADER.to_string(),
                    dialect.path_line(&FoldedPath::derived(&dirs)),
                    super::super::env_files::legacy_commented_line(
                        &dialect.env_line(&foo).unwrap().expect("a safe name"),
                        &origins.env_comment(&foo.name),
                    ),
                    super::super::env_files::legacy_commented_line(
                        &dialect.alias_line(&catn).expect("a safe name"),
                        &origins.alias_comment(&catn.name),
                    ),
                    String::new(),
                ]
                .join("\n")
            };
            let blocks = || {
                let layered = crate::reconciler::LayeredEnv::for_test(
                    std::slice::from_ref(&foo),
                    std::slice::from_ref(&catn),
                    &Default::default(),
                );
                if platform == EnvPlatform::Windows {
                    super::super::env_files::generate_powershell_env_content(
                        &layered,
                        Some(&FoldedPath::derived(&dirs)),
                    )
                    .unwrap()
                } else {
                    super::super::env_files::generate_env_file_content(
                        &layered,
                        Some(&FoldedPath::derived(&dirs)),
                    )
                    .unwrap()
                }
            };
            let baseline = legacy(&owners);
            let desired = blocks();
            assert_ne!(baseline, desired, "the upgrade must change the file");
            assert!(
                !has_unclaimed_disappearing_line(
                    &baseline,
                    &desired,
                    std::slice::from_ref(&foo),
                    std::slice::from_ref(&catn),
                    platform,
                ),
                "an owner-less line is claimed by its own declaration: {baseline}"
            );
        }
    }

    /// The one-time move to layered blocks must not read as every module's
    /// contribution moving, or a converged machine runs every `onChange` hook
    /// it has once for a file whose values did not change.
    #[test]
    fn moving_a_converged_file_to_blocks_moves_no_modules_entries() {
        let foo = ev("FOO", "bar");
        let catn = ShellAlias {
            name: "catn".to_string(),
            command: "cat -n".to_string(),
            platforms: vec![],
        };
        let dirs = [ManagerPathDir::new("brew", "/opt/homebrew/bin")];
        let owners = {
            let mut o = crate::config::EntryOwners::default();
            o.claim(
                "module:nvim",
                std::slice::from_ref(&foo),
                std::slice::from_ref(&catn),
            );
            EnvOrigins::from_owners(&o)
        };

        for platform in [EnvPlatform::Linux, EnvPlatform::Windows] {
            // What an older cfgd wrote: one banner line, then one commented
            // line per entry. Rendered through the same `Dialect` so the two
            // shapes cannot drift apart in anything but the scaffolding.
            let legacy = |origins: &EnvOrigins| {
                let dialect = super::super::env_files::Dialect::of(platform);
                [
                    super::super::env_files::ENV_FILE_HEADER.to_string(),
                    dialect.path_line(&FoldedPath::derived(&dirs)),
                    super::super::env_files::legacy_commented_line(
                        &dialect.env_line(&foo).unwrap().expect("a safe name"),
                        &origins.env_comment(&foo.name),
                    ),
                    super::super::env_files::legacy_commented_line(
                        &dialect.alias_line(&catn).expect("a safe name"),
                        &origins.alias_comment(&catn.name),
                    ),
                    String::new(),
                ]
                .join("\n")
            };
            let blocks = || {
                let layered = crate::reconciler::LayeredEnv::for_test(
                    std::slice::from_ref(&foo),
                    std::slice::from_ref(&catn),
                    &Default::default(),
                );
                if platform == EnvPlatform::Windows {
                    super::super::env_files::generate_powershell_env_content(
                        &layered,
                        Some(&FoldedPath::derived(&dirs)),
                    )
                    .unwrap()
                } else {
                    super::super::env_files::generate_env_file_content(
                        &layered,
                        Some(&FoldedPath::derived(&dirs)),
                    )
                    .unwrap()
                }
            };
            let write = PrimaryEnvWrite {
                baseline: Some(legacy(&owners)),
                desired: blocks(),
                platform,
                unclaimed_deletion: false,
                path: Some(FoldedPath::derived(&dirs)),
            };
            assert!(
                !write
                    .module_env_moved(std::slice::from_ref(&foo), std::slice::from_ref(&catn))
                    .unwrap(),
                "a comment-only rewrite moves nothing on {platform:?}"
            );
        }
    }

    fn ps(env: &[EnvVar], aliases: &[ShellAlias], path_dirs: &[ManagerPathDir]) -> String {
        let fold = (!path_dirs.is_empty()).then(|| FoldedPath::derived(path_dirs));
        super::super::env_files::generate_powershell_env_content(
            &crate::reconciler::LayeredEnv::for_test(env, aliases, &Default::default()),
            fold.as_ref(),
        )
        .unwrap()
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
            platforms: vec![],
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
        let baseline = ps(
            std::slice::from_ref(&foo),
            &[],
            &[ManagerPathDir::new("scoop", "C:/old/bin")],
        );
        let desired = ps(
            std::slice::from_ref(&foo),
            &[],
            &[ManagerPathDir::new("scoop", "C:/new/bin")],
        );
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
