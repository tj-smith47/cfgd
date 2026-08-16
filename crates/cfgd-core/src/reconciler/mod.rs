use std::path::PathBuf;

use crate::providers::ProviderRegistry;
use crate::state::StateStore;

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
mod system;
mod types;
mod verify;

#[cfg(test)]
mod tests;

pub use apply::{action_matches_phase_filter, emit_action_notes};
pub use env_engine::launchd_env_plist;
#[cfg(any(test, feature = "test-helpers"))]
pub use env_engine::{
    EnvHostProbeOverride, EnvHostProbeOverrideGuard, with_env_host_probe_override_guard,
};
pub(crate) use format::debug_assert_system_key_undoubled;
pub use format::{
    DisplaySubject, action_display_subject, bare_script_subject, condense_action_desc_for_display,
    format_action_description, format_plan_item, format_plan_items, hook_script_subject,
    module_script_subject, script_run_subject, system_key_doubling_error, system_resource_key,
};
pub use managers::plan_managers;
pub use packages::stale_tracked_packages;
pub use patch::{PatchBinding, PatchContext, PatchOutcome, evaluate_patch, patch_failure_detail};
pub use pending::{
    ActualPackages, AutoAccepted, DECISION_ACTION_INSTALL, DecisionExclusions, DecisionMint,
    DecisionScope, DecisionTargets, DeliveredItems, SourcePolicyReview, Subscriptions, TIER_LOCKED,
    TIER_OPTIONAL, TIER_RECOMMENDED, UndecidableBatch, WithheldDecisions, configured_auto_apply,
    declared_decision_paths, hash_resources, local_profile, mint_decisions, owns_decision_store,
    review_source_policies, review_source_policy, source_delivered_layers,
    source_delivered_profile, undecidable_source_batches, withhold_from_plan,
};
pub use restore::{RestoreOutcome, restore_file_from_backup};
pub use run::{
    ApplyRun, BACKUPS_PHASE_LABEL, Confirm, HOOKS_PHASE_LABEL, MSG_NOTHING_TO_DO, PhaseCoverage,
    PseudoPhase, RunContext, RunDisposition, RunExecutor, RunTally, RunTitle, ScopedGroup,
    ScopedPhase, align_width, align_width_of, in_scope_tree, pseudo_phase, render_apply_result,
    render_plan_tree, render_run_rollup,
};
pub use types::{
    Action, ActionResult, ApplyResult, CFGD_GROUP_ORDER, EnvAction, MANAGERS_GROUP, ManagerAction,
    ModuleAction, ModuleActionKind, Owner, OwnerGroup, OwnerKind, Phase, PhaseFilter, PhaseName,
    Plan, ReconcileContext, RollbackResult, ScriptAction, ScriptPhase, SystemAction, Tier,
};
pub use verify::{VerifyResult, verify};

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
}

impl<'a> Reconciler<'a> {
    pub fn new(registry: &'a ProviderRegistry, state: &'a StateStore) -> Self {
        Self {
            registry,
            state,
            home: resolved_home(),
            withhold_env_surface: false,
        }
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
        }
    }
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
