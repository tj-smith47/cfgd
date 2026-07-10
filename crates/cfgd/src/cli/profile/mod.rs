use std::path::Path;

use super::*;

// --- Submodule declarations ---

mod backups;
mod create;
mod delete;
mod edit;
pub mod list;
mod migrate;
mod parsers;
pub mod show;
mod switch;
mod update;

#[cfg(test)]
mod tests;

// --- Re-export handlers so cli::mod can dispatch to them ---

pub use create::cmd_profile_create;
pub use delete::cmd_profile_delete;
pub use edit::cmd_profile_edit;
pub use list::cmd_profile_list;
pub use migrate::cmd_profile_migrate;
pub use show::cmd_profile_show;
pub use switch::cmd_profile_switch;
pub use update::cmd_profile_update;

// --- Cross-submodule helpers (private to cli::profile) ---

pub(super) use backups::{
    collect_module_file_targets, prompt_restore_backups, restore_or_remove_deployed_files,
};
pub(super) use parsers::{parse_manager_package, parse_secret_spec, update_script_list};

pub(super) fn profiles_inheriting(profiles_dir: &Path, name: &str) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    for entry in cfgd_core::config::scan_profiles(profiles_dir)
        .map_err(cfgd_core::errors::CfgdError::Config)?
    {
        if let Ok(doc) = config::load_profile(&entry.path)
            && doc.spec.inherits.contains(&name.to_string())
        {
            result.push(doc.metadata.name.clone());
        }
    }
    Ok(result)
}

/// Best-effort sorted profile names for "Available profiles:" hints. Ambiguous
/// or unreadable directories yield an empty list — the hint is supplementary,
/// never a failure path.
pub(super) fn available_profile_names(profiles_dir: &Path) -> Vec<String> {
    cfgd_core::config::scan_profiles(profiles_dir)
        .map(|entries| entries.into_iter().map(|e| e.name).collect())
        .unwrap_or_default()
}

/// Verify an inherited parent profile exists (either layout form). Not-found
/// maps to the stable `parent_not_found` CLI error; ambiguity fails closed.
pub(super) fn ensure_parent_profile_exists(
    profiles_dir: &Path,
    child: &str,
    parent: &str,
) -> anyhow::Result<()> {
    match cfgd_core::config::find_profile_path(profiles_dir, parent) {
        Ok(_) => Ok(()),
        Err(cfgd_core::errors::ConfigError::ProfileNotFound { .. }) => Err(crate::cli::cli_error(
            child,
            "parent_not_found",
            format!("Parent profile '{}' not found", parent),
            serde_json::json!({ "parent": parent }),
        )),
        Err(e) => Err(cfgd_core::errors::CfgdError::Config(e).into()),
    }
}
