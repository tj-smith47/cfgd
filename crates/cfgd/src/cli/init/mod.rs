use super::*;

// --- Submodule declarations ---

pub mod cmd_init;
pub mod enroll;
mod source;

#[cfg(test)]
mod tests;

// --- Re-export pub(crate) entry points so cli::mod can dispatch to them ---

pub(super) use cmd_init::regenerate_workflow;
pub use cmd_init::{InitArgs, cmd_init};
pub(super) use enroll::cmd_enroll;
pub use enroll::{EnrollOutput, build_enroll_error_doc, build_enroll_final_doc};
pub(super) use source::resolve_from;

// --- Cross-submodule helpers (private to cli::init, visible to tests) ---

#[cfg(test)]
use cmd_init::{
    apply_plan, check_prerequisites, is_module_only_apply, pick_profile, scaffold, should_run_apply,
};
#[cfg(test)]
use enroll::{
    build_device_credential, detect_ssh_key, first_existing_ssh_key, next_steps_lines,
    sign_with_gpg, sign_with_ssh,
};
#[cfg(test)]
use source::{clone_into, is_git_source};
