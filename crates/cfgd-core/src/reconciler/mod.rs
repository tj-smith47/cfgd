use crate::providers::ProviderRegistry;
use crate::state::StateStore;

mod apply;
mod env;
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
pub use format::{format_action_description, format_plan_items};
pub use types::{
    Action, ActionResult, ApplyResult, EnvAction, ModuleAction, ModuleActionKind, Phase, PhaseName,
    Plan, ReconcileContext, RollbackResult, ScriptAction, ScriptPhase, SystemAction,
};
pub use verify::{VerifyResult, verify};

pub(crate) use scripts::{build_script_env, execute_script};

// Re-export sibling submodule items at the parent level so the externalized
// tests submodule can reach them via `super::*`. The `#[cfg(test)]` guard
// keeps these at module-private scope and only compiles them when tests run.
#[cfg(test)]
use {
    crate::errors::Result,
    crate::output::Printer,
    crate::providers::{FileAction, PackageAction, SecretAction},
    crate::state::ApplyStatus,
    env_files::*,
    format::*,
    restore::*,
    scripts::*,
    std::collections::HashMap,
    std::path::PathBuf,
    verify::*,
};

/// The unified reconciler. Generates plans and applies them.
pub struct Reconciler<'a> {
    registry: &'a ProviderRegistry,
    state: &'a StateStore,
}

impl<'a> Reconciler<'a> {
    pub fn new(registry: &'a ProviderRegistry, state: &'a StateStore) -> Self {
        Self { registry, state }
    }
}
