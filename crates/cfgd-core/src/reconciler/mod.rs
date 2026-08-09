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
mod modules;
mod packages;
mod patch;
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

pub use apply::action_matches_phase_filter;
pub use env_engine::launchd_env_plist;
pub use format::{
    DisplaySubject, action_display_subject, condense_action_desc_for_display,
    format_action_description, format_plan_item, format_plan_items, module_script_subject,
    script_run_subject,
};
pub use packages::stale_tracked_packages;
pub use patch::{PatchBinding, PatchContext, PatchOutcome, evaluate_patch, patch_failure_detail};
pub use restore::{RestoreOutcome, restore_file_from_backup};
pub use run::{
    ApplyRun, BACKUPS_PHASE_LABEL, Confirm, HOOKS_PHASE_LABEL, PseudoPhase, RunContext,
    RunDisposition, RunExecutor, RunTally, RunTitle, align_width, align_width_of, pseudo_phase,
    render_apply_result, render_plan_tree, render_run_rollup,
};
pub use types::{
    Action, ActionResult, ApplyResult, EnvAction, ModuleAction, ModuleActionKind, Owner,
    OwnerGroup, OwnerKind, Phase, PhaseFilter, PhaseName, Plan, ReconcileContext, RollbackResult,
    ScriptAction, ScriptPhase, SystemAction,
};
pub use verify::{VerifyResult, verify};

pub(crate) use env::all_recorded_path_dirs;
pub(crate) use scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, ScriptReport, ScriptSubject, build_module_script_env,
    build_script_env, effective_continue_on_error, execute_script, script_default_workdir,
};

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
}

impl<'a> Reconciler<'a> {
    pub fn new(registry: &'a ProviderRegistry, state: &'a StateStore) -> Self {
        Self {
            registry,
            state,
            home: resolved_home(),
        }
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
