use std::path::Path;

use cfgd_core::output_v2::{Doc, Role};

use super::*;

// --- Submodule declarations ---

mod backups;
pub mod create;
pub mod delete;
pub mod edit;
pub mod list;
mod parsers;
pub mod show;
pub mod switch;
pub mod update;

#[cfg(test)]
mod tests;

// --- Re-export handlers so cli::mod can dispatch to them ---

pub use create::cmd_profile_create;
pub use delete::cmd_profile_delete;
pub use edit::cmd_profile_edit;
pub use list::cmd_profile_list;
pub use show::cmd_profile_show;
pub use switch::cmd_profile_switch;
pub use update::cmd_profile_update;

// --- Cross-submodule helpers (private to cli::profile) ---

pub(super) use backups::{collect_module_file_targets, prompt_restore_backups};
pub(super) use parsers::{parse_manager_package, parse_secret_spec, update_script_list};

/// Doc emitted on every profile-command error path before the `anyhow::bail!`
/// fires. Carries an `error` category key + `name` so structured consumers
/// (`-o json`) see a stable shape on failure.
pub(super) fn build_profile_error_doc(
    name: &str,
    error_kind: &str,
    message: impl Into<String>,
    extras: serde_json::Value,
) -> Doc {
    let mut payload = serde_json::json!({
        "error": error_kind,
        "name": name,
    });
    if let serde_json::Value::Object(extra_map) = extras
        && let serde_json::Value::Object(payload_map) = &mut payload
    {
        for (k, v) in extra_map {
            payload_map.insert(k, v);
        }
    }
    Doc::new().status(Role::Fail, message).with_data(payload)
}

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
