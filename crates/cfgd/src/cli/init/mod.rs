use super::*;

// --- Submodule declarations ---

mod cmd_init;
mod enroll;
mod source;

#[cfg(test)]
mod tests;

// --- Re-export pub(crate) entry points so cli::mod can dispatch to them ---

pub(super) use cmd_init::{InitArgs, cmd_init, regenerate_workflow};
pub(super) use enroll::cmd_enroll;
pub(super) use source::resolve_from;

// --- Cross-submodule helpers (private to cli::init, visible to tests) ---

#[cfg(test)]
use cmd_init::{
    apply_plan, check_prerequisites, is_module_only_apply, pick_profile, scaffold, should_run_apply,
};
#[cfg(test)]
use enroll::{detect_ssh_key, sign_with_gpg, sign_with_ssh};
#[cfg(test)]
use source::{clone_into, is_git_source};
