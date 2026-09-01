use std::path::{Path, PathBuf};

use crate::providers::ProviderRegistry;
use crate::state::StateStore;

mod adopt;
mod apply;
mod env;
mod env_engine;
mod env_files;
mod file_action;
mod files;
mod format;
mod lanes;
mod live_tree;
mod managers;
mod modules;
mod packages;
mod patch;
mod pending;
mod plan;
mod restore;
mod rollback;
mod run;
mod scripts;
mod scripts_apply;
mod secrets;
mod sidecar;
mod system;
mod types;
mod verify;

#[cfg(test)]
mod tests;

pub use adopt::{
    ConflictResolver, ResolvedConflict, UNMANAGED_DRIFT_CAUSE, UNMANAGED_SKIP_REASON,
    apply_conflict_policy, is_unmanaged_file, mark_unmanaged_drift, module_file_desired_hash,
    sweep_label, sweep_unmanaged_file_targets, unmanaged_conflict_error,
};
pub use apply::{
    action_matches_phase_filter, action_produced_detail, render_caveats, widest_produced_detail,
};
pub use env::recorded_manager_path_dirs;
pub use env_engine::{
    ENV_VERB_INJECT, ENV_VERB_WRITE, ManagerPathDir, launchd_env_plist, recorded_env_method,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use env_engine::{
    EnvHostProbeOverride, EnvHostProbeOverrideGuard, with_env_host_probe_override_guard,
};
pub use files::{LinkDeployedDigest, RefreshedHashes, link_deployed_digest};
pub(crate) use format::debug_assert_system_key_undoubled;
pub use format::{
    DisplaySubject, action_display_subject, action_display_subject_within, bare_script_subject,
    condense_action_desc_for_display, format_action_description, format_plan_item,
    format_plan_items, hook_script_subject, module_script_subject, module_script_subject_within,
    script_run_subject, script_run_subject_within, system_key_doubling_error, system_resource_key,
};
pub use managers::plan_managers;
pub use packages::stale_tracked_packages;
pub use patch::{PatchBinding, PatchContext, PatchOutcome, evaluate_patch, patch_failure_detail};
pub use pending::{
    ActualPackages, AutoAccepted, DECISION_ACTION_INSTALL, DecisionContents, DecisionExclusions,
    DecisionMint, DecisionScope, DecisionTargets, DecisionsTitleScope, DeliveredItems,
    MSG_ANSWER_DECISIONS, MSG_INCLUDE_DECLINED_DECISIONS, SourcePolicyReview, Subscriptions,
    TIER_LOCKED, TIER_OPTIONAL, TIER_RECOMMENDED, UndecidableBatch, WithheldDecisions,
    WithheldFromPlan, answer_decisions_hint, configured_auto_apply, decision_resource_content,
    decision_row_annotation, decisions_by_source, declared_decision_fingerprints,
    declared_decision_paths, declined_decisions_title, hash_resources, local_profile,
    merged_entry_owners, mint_decisions, owns_decision_store, pending_decisions_title,
    review_source_policies, review_source_policy, source_delivered_layers,
    source_delivered_profile, title_cased_tier, undecidable_source_batches, withhold_from_plan,
};
pub use restore::{RestoreOutcome, restore_file_from_backup};
pub use run::{
    ApplyRun, BACKUPS_PHASE_LABEL, ComposedSource, Confirm, HOOKS_PHASE_LABEL, MSG_NOTHING_TO_DO,
    PhaseCoverage, PseudoPhase, RunContext, RunDisposition, RunExecutor, RunTally, RunTitle,
    ScopedGroup, ScopedPhase, align_width_of, in_scope_tree, nothing_to_do_verdict, outcome_counts,
    pseudo_phase, render_apply_result, render_plan_tree, render_run_rollup, report_align_width,
    report_subject_budget, run_next_step, sole_phase,
};
pub(crate) use sidecar::is_stamped_sidecar_name;
pub use sidecar::{CFGD_BACKUP_SUFFIX, SidecarOutcome, backup_file, cfgd_backup_path};
pub use types::{
    Action, ActionResult, ApplyResult, CFGD_GROUP_ORDER, DeclaredProvision, ENV_GROUP,
    ENV_RESOURCE_TYPE, EnvAction, MANAGERS_GROUP, ManagerAction, ModuleAction, ModuleActionKind,
    Owner, OwnerGroup, OwnerKind, Phase, PhaseFilter, PhaseName, Plan, ReconcileContext,
    RollbackResult, SESSION_GROUP, ScriptAction, ScriptPhase, SystemAction, Tier, attempted_count,
    package_drift_resource_id,
};
pub use verify::{
    EnvItemCheck, MergedEnvItems, SystemCheckError, VerifyReport, VerifyResult,
    env_item_verify_results, env_verify_results, verify,
};

pub(crate) use env::all_recorded_path_dirs;
/// Widened past this crate for `cfgd::cli::plan_ops::filter_plan`, the one
/// caller outside `cfgd-core` (the other two — the daemon's per-module tick
/// and `pending::withhold_from_plan` — are intra-crate and would need only
/// `pub(crate)`). Safe to call on any already-planned [`Plan`]: it mutates in
/// place, dropping a manager node no surviving package/module install still
/// consumes and no surviving manager node still depends on. Callers own one
/// invariant this function does not enforce itself: an EXPLICITLY selected
/// node (a `--only` match) must never reach here still present alongside an
/// empty consumer set, since "nothing consumes it" and "the user asked for it
/// by name" are different questions and this function only answers the
/// first — `filter_plan` satisfies that by calling it only after a
/// `--skip`-only pass, never after `--only` narrowed the plan.
pub use managers::{
    prerequisite_selectors, prune_to_surviving_consumers, restrict_provision_batches,
};
pub(crate) use scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, ScriptReport, ScriptSubject, build_module_script_env,
    build_script_env, effective_continue_on_error, execute_script, script_default_workdir,
};
pub(crate) use types::action_resource_info;

// Re-export sibling submodule items at the parent level so the externalized
// tests submodule can reach them via `super::*`. The `#[cfg(test)]` guard
// keeps these at module-private scope and only compiles them when tests run.
#[cfg(test)]
use {
    crate::errors::Result,
    crate::output::Printer,
    crate::providers::{FileAction, PackageAction, SecretAction},
    crate::state::ApplyStatus,
    env_engine::*,
    env_files::*,
    format::*,
    restore::*,
    scripts::*,
    std::collections::HashMap,
    verify::*,
};

/// The unified reconciler. Generates plans and applies them.
pub struct Reconciler<'a> {
    registry: &'a ProviderRegistry,
    state: &'a StateStore,
    /// The home directory every env-surface path is derived from.
    ///
    /// Resolved once, here, and passed as data from then on. The env plan names
    /// `~/.cfgd.env` and the shell rc files, so any code path that resolved `~`
    /// for itself would be a second, unobservable way to reach a real home
    /// directory — including from an apply that only meant to exercise
    /// something else.
    home: PathBuf,
    /// Whether the env surface is withheld for this run.
    ///
    /// The env surface is generated as a unit — one file naming every declared
    /// variable, the rc source lines that load it, the live-session mirror — so
    /// a caller withholding any part of it withholds all of it, and says so
    /// once, here. Apply consults this for the post-phase regeneration, which
    /// rebuilds that surface from the DECLARED set rather than from the plan;
    /// without the flag a caller that pruned every env action out of its plan
    /// still gets the surface written behind its back the moment a secret
    /// resolves an env var or a package manager is bootstrapped mid-run.
    withhold_env_surface: bool,
    /// Secrets resolved during THIS run, so one reference costs one spawn of
    /// its backend however many actions name it.
    ///
    /// A single declared reference becomes a `Resolve` action for the file it
    /// writes and a `ResolveEnv` action for the variables it exports, and both
    /// used to spawn `op read` / `sops -d` for the same value. The cache lives
    /// on the reconciler because a reconciler IS one run — a CLI apply builds
    /// one and drops it, a daemon builds one per tick — so plaintext never
    /// outlives the work that needed it.
    secrets: crate::providers::SecretCache,
    /// The config directory patch scripts are anchored under, when the caller
    /// has one.
    ///
    /// `None` leaves a `Patch` entry unanswerable at plan time, so it is
    /// planned on every run (a fixture, a validation pass). Every command that
    /// plans against a real host sets it through
    /// [`Reconciler::with_config_dir`], which is what lets a `Patch` module
    /// converge instead of re-running its hooks on every daemon tick.
    config_dir: Option<PathBuf>,
    /// The run's installed-state reader, used to drop a module package the
    /// machine already carries.
    ///
    /// `None` plans a module's whole declared list, which is what a caller with
    /// no machine to ask about wants (a fixture, a validation pass). Every
    /// command that plans against a real host sets it through
    /// [`Reconciler::diffing_installed`] and hands over the SAME context its
    /// profile-package planner uses, so one enumeration per manager answers
    /// both halves of the run.
    installed: Option<&'a crate::providers::PackageContext<'a>>,
    /// Targets an unmanaged-file conflict settled as `Backup`, to be copied
    /// aside as the action that displaces each one executes.
    ///
    /// The decision is made while the plan is built — that is where the policy,
    /// the prompt and the plan mutation live — but the COPY is a disk mutation,
    /// and a plan is a preview until the operator confirms it. Carrying the
    /// decision here defers the write to the phase whose work it is part of, so
    /// `backed up to …` rides as a DETAIL on the row of the write it protects —
    /// under `Phase: Files`, on the success row and the failure row alike —
    /// instead of standing as its own line above the run's header.
    sidecar_backups: std::collections::HashSet<PathBuf>,
    /// Managers a node of THIS run already failed to put on the machine.
    ///
    /// A provision that failed is the run's own verdict that the manager is not
    /// here, and it outranks any later probe: `is_available()` bottoms out in a
    /// path lookup whose memo the intervening installs moved and whose last arm
    /// is a bare `exists()`, so a manager cfgd just reported it could not
    /// provision can answer "available" one phase later and be spawned into an
    /// `ENOENT`. Within one phase the lane dispatcher's `fail_dependents`
    /// already withholds the downstream work; this is the same withholding
    /// carried ACROSS phases, where no DAG edge reaches.
    unprovisioned: std::cell::RefCell<Vec<String>>,
    /// Managers a node of THIS run has already PUT on the machine — the
    /// mirror of [`Self::unprovisioned`], and the answer to "did this run's
    /// own `Prerequisites` phase already deliver this tool".
    ///
    /// A module entry naming a tool cfgd bootstraps (`- name: npm`) with no
    /// `prefer` and no `aliases` is not a route
    /// (`ResolvedPackage::manager_declared`), so the manager's own cascade
    /// provisions it — brew for npm — and the entry then still stands as an
    /// apt install of the same toolchain, which apt's installed listing has
    /// no way to elide because apt is not what landed it. Two copies of one
    /// toolchain with `PATH` order picking the winner is exactly what the
    /// route feature exists to prevent, so the elision keys on the tool the
    /// provision DELIVERED rather than on the manager that delivered it.
    ///
    /// Recorded from the node's own success, never re-probed: `is_available()`
    /// cannot tell a tool this run installed from one that was here all along,
    /// and only the former makes the entry beside it a duplicate.
    provisioned: std::cell::RefCell<Vec<String>>,
    /// The `(installer, package)` pairs those provisions INSTALLED — `("brew",
    /// "node")` for `provision npm via brew` — recorded from the node's own
    /// success beside [`Self::provisioned`], so an install row can tell an
    /// entry this run delivered from one the machine arrived with
    /// ([`Self::delivered_by_this_run`]).
    provisioned_packages: std::cell::RefCell<Vec<(String, String)>>,
    /// What this run is scoped to, for the `applies` row it records.
    ///
    /// `None` falls back to the resolved profile's own name, which is what
    /// every profile-scoped caller wants. A `--module` run has no profile to
    /// name and says so here instead, so the recorded row states the truth
    /// rather than a placeholder every reader then has to special-case.
    recorded_scope: Option<String>,
}

impl<'a> Reconciler<'a> {
    /// The file manager the registry this reconciler was built over holds —
    /// what the CLI's post-apply hash refresh hands back to
    /// [`Self::refresh_link_deployed_hashes`], so a caller holding only the
    /// reconciler (`init --apply`) refreshes exactly what `apply` does.
    pub fn file_manager(&self) -> Option<&dyn crate::providers::FileManager> {
        self.registry.file_manager.as_deref()
    }

    pub fn new(registry: &'a ProviderRegistry, state: &'a StateStore) -> Self {
        Self {
            registry,
            state,
            home: resolved_home(),
            withhold_env_surface: false,
            secrets: crate::providers::SecretCache::new(),
            config_dir: None,
            installed: None,
            sidecar_backups: std::collections::HashSet::new(),
            unprovisioned: std::cell::RefCell::new(Vec::new()),
            provisioned: std::cell::RefCell::new(Vec::new()),
            provisioned_packages: std::cell::RefCell::new(Vec::new()),
            recorded_scope: None,
        }
    }

    /// Copy each of `targets` aside as the action that displaces it executes,
    /// reporting where the copy landed on that action's own row.
    ///
    /// Set from the unmanaged-file conflict pass, which decides the policy but
    /// no longer carries it out: see `Self::sidecar_backups`.
    #[must_use]
    pub fn backing_up(mut self, targets: std::collections::HashSet<PathBuf>) -> Self {
        self.sidecar_backups = targets;
        self
    }

    /// Copy `target` aside if this run settled it as an adoption.
    ///
    /// Called from both file-writing paths — a profile `spec.files` action and
    /// a module's own deploy loop — immediately before the write that displaces
    /// the target.
    fn back_up_adopted_target(
        &self,
        target: &Path,
    ) -> crate::errors::Result<Option<sidecar::SidecarOutcome>> {
        if self.sidecar_backups.contains(target) {
            return sidecar::backup_file(target).map(Some);
        }
        Ok(None)
    }

    /// Record `scope` as what this run was scoped to, in place of the resolved
    /// profile's name: see `Self::recorded_scope`.
    #[must_use]
    pub fn recording_scope(mut self, scope: impl Into<String>) -> Self {
        self.recorded_scope = Some(scope.into());
        self
    }

    /// Anchor plan-time `Patch` evaluation under `config_dir`.
    ///
    /// A patch merge is a function of the live target plus a script anchored at
    /// the module's directory with the standard `CFGD_*` metadata — metadata
    /// only the config directory can complete. Without it a `Patch` entry can
    /// never read as converged and its module re-plans (and re-hooks) forever.
    #[must_use]
    pub fn with_config_dir(mut self, config_dir: &Path) -> Self {
        self.config_dir = Some(config_dir.to_path_buf());
        self
    }

    /// Diff a module's declared packages against what its manager reports
    /// installed, the way the profile-level planner already does.
    ///
    /// Without it a module re-lists its entire package set on every plan and
    /// re-shells to the manager on every apply, so a converged machine never
    /// reads as converged: `cfgd plan` prints the same block forever and the
    /// only thing making `cfgd apply` a no-op is the manager binary's own
    /// idempotency. Pass the context the command already built for its profile
    /// packages — a second one would re-enumerate every manager.
    #[must_use]
    pub fn diffing_installed(mut self, cx: &'a crate::providers::PackageContext<'a>) -> Self {
        self.installed = Some(cx);
        self
    }

    /// Withhold the env surface from the post-phase regeneration when `yes`.
    ///
    /// For a caller that pruned the env actions out of its own plan and needs
    /// apply to honour that decision for the surface apply would otherwise
    /// rebuild from the declared set. Withholding is all-or-nothing by
    /// construction: there is no per-variable action to suppress.
    #[must_use]
    pub fn withholding_env_surface(mut self, yes: bool) -> Self {
        self.withhold_env_surface = yes;
        self
    }

    /// A reconciler whose env surfaces resolve against `home` instead of the
    /// invoking user's. For a caller that manages a home directory other than
    /// its own — and for tests, which must never name a real one.
    pub fn with_home(
        registry: &'a ProviderRegistry,
        state: &'a StateStore,
        home: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registry,
            state,
            home: home.into(),
            withhold_env_surface: false,
            secrets: crate::providers::SecretCache::new(),
            config_dir: None,
            installed: None,
            sidecar_backups: std::collections::HashSet::new(),
            unprovisioned: std::cell::RefCell::new(Vec::new()),
            provisioned: std::cell::RefCell::new(Vec::new()),
            provisioned_packages: std::cell::RefCell::new(Vec::new()),
            recorded_scope: None,
        }
    }
}

/// The primary managed env file under `home` on THIS host — the one file the
/// per-item env checks read back, and the one the env engine always writes
/// first. Named once for every caller outside this module, so a consumer
/// cannot mint a second platform split and end up pointing at a file the
/// verifier does not read.
#[must_use]
pub fn primary_env_file(home: &Path) -> PathBuf {
    env_engine::primary_env_file_path(home, env_engine::EnvPlatform::current())
}

#[cfg(not(any(test, feature = "test-helpers")))]
fn resolved_home() -> PathBuf {
    crate::expand_tilde(std::path::Path::new("~"))
}

/// Under any test build an unguarded reconciler gets a sandbox instead of the
/// operator's home directory.
///
/// `expand_tilde` honors the test-home thread-local, so a test that installs
/// `with_test_home_guard` still gets the home it asked for. This covers the
/// test that installs nothing: an apply carrying `spec.env`, a secret-backed
/// env var, or a bootstrapped package manager writes the env surfaces mid-run,
/// which is how this machine's own `~/.cfgd.env`, `environment.d/cfgd.conf`
/// and shell rc files came to be rewritten by a test run. Test discipline is
/// what failed there, so the fallback removes the real home from reach rather
/// than documenting that it must not be reached.
///
/// The gate is the `test-helpers` feature and not `cfg(test)` alone, because
/// `cfg(test)` is set only while compiling THIS crate's own test binary. Every
/// dependent crate links a plain release-shaped `cfgd-core`, so a `cfgd` CLI
/// test driving a real apply resolved the operator's home through the arm
/// above. The feature is declared in each consumer's `[dev-dependencies]`
/// only, so under resolver 2 no shipped binary can compile this arm.
#[cfg(any(test, feature = "test-helpers"))]
fn resolved_home() -> PathBuf {
    crate::test_home_override().unwrap_or_else(unguarded_test_home)
}

/// A throwaway home directory unique to the calling thread.
///
/// The test harness gives each test its own thread, so a per-thread directory
/// means two unguarded tests running in parallel cannot read each other's env
/// surfaces — a single process-wide sandbox would trade a real-home escape for
/// a cross-test one. It is named, not created: anything appearing under
/// `cfgd-unguarded-test-home-*` is a test that should have installed a guard.
#[cfg(any(test, feature = "test-helpers"))]
fn unguarded_test_home() -> PathBuf {
    use std::cell::OnceCell;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static HOME: OnceCell<PathBuf> = const { OnceCell::new() };
    }

    HOME.with(|home| {
        home.get_or_init(|| {
            std::env::temp_dir().join(format!(
                "cfgd-unguarded-test-home-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
        })
        .clone()
    })
}
