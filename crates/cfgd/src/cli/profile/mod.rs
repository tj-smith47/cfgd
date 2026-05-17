use std::path::Path;

use super::*;

// --- Submodule declarations ---

mod backups;
mod create;
mod delete;
mod edit;
mod list;
mod parsers;
pub mod show;
mod switch;
mod update;

#[cfg(test)]
mod tests;

// --- Re-export pub(super) handlers so cli::mod can dispatch to them ---

pub(super) use create::cmd_profile_create;
pub(super) use delete::cmd_profile_delete;
pub(super) use edit::cmd_profile_edit;
pub(super) use list::cmd_profile_list;
pub(super) use show::cmd_profile_show;
pub(super) use switch::cmd_profile_switch;
pub(super) use update::cmd_profile_update;

// --- Cross-submodule helpers (private to cli::profile) ---

pub(super) use backups::{collect_module_file_targets, prompt_restore_backups};
pub(super) use parsers::{parse_manager_package, parse_secret_spec, update_script_list};

pub(super) fn profiles_inheriting(profiles_dir: &Path, name: &str) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    cfgd_core::config::for_each_yaml_file(profiles_dir, |path| {
        if let Ok(doc) = config::load_profile(path)
            && doc.spec.inherits.contains(&name.to_string())
        {
            result.push(doc.metadata.name.clone());
        }
        Ok(())
    })?;
    Ok(result)
}
