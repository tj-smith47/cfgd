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
mod plan;
mod restore;
mod rollback;
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
    condense_action_desc_for_display, display_action_desc_in_phase, format_action_description,
    format_plan_items,
};
pub use packages::stale_tracked_packages;
pub use restore::{RestoreOutcome, restore_file_from_backup};
pub use types::{
    Action, ActionResult, ApplyResult, EnvAction, ModuleAction, ModuleActionKind, ModuleScope,
    ModuleSection, Phase, PhaseName, Plan, ReconcileContext, RollbackResult, ScriptAction,
    ScriptPhase, SystemAction,
};
pub use verify::{VerifyResult, verify};

pub(crate) use env::all_recorded_path_dirs;
pub(crate) use scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, build_module_script_env, build_script_env,
    execute_script, script_default_workdir,
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

#[cfg(not(test))]
fn resolved_home() -> PathBuf {
    crate::expand_tilde(std::path::Path::new("~"))
}

/// Under `cargo test` an unguarded reconciler gets a sandbox instead of the
/// operator's home directory.
///
/// `expand_tilde` honors the test-home thread-local, so a test that installs
/// `with_test_home_guard` still gets the home it asked for. This covers the
/// test that installs nothing: an apply carrying a secret-backed env var or a
/// bootstrapped package manager regenerates the env surfaces mid-run, which
/// once rewrote this machine's own `~/.cfgd.env`. Test discipline is what
/// failed there, so the fallback removes the real home from reach rather than
/// documenting that it must not be reached. The directory is named, not
/// created — anything appearing in it is a test that should have installed a
/// guard.
#[cfg(test)]
fn resolved_home() -> PathBuf {
    match crate::test_home_override() {
        Some(home) => home,
        None => {
            std::env::temp_dir().join(format!("cfgd-unguarded-test-home-{}", std::process::id()))
        }
    }
}
