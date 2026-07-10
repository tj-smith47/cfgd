use std::path::{Path, PathBuf};

use cfgd_core::PathDisplayExt;
use serde::Serialize;

use super::*;

const NO_REGISTRIES_MSG: &str = "No module registries configured";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleListEntry {
    pub name: String,
    pub active: bool,
    pub source: String,
    pub status: String,
    pub packages: usize,
    pub files: usize,
    pub depends: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleShowOutput {
    pub name: String,
    pub directory: String,
    pub source: String,
    pub depends: Vec<String>,
    pub state: Option<cfgd_core::state::ModuleStateRecord>,
    pub spec: cfgd_core::config::ModuleSpec,
}

/// Failure modes of [`load_module_document`], distinguished so callers can emit
/// the correct structured-error code: a genuinely absent module is `not_found`,
/// while a present-but-malformed `module.yaml` is `parse_failed`.
///
/// The `Parse` variant carries the underlying [`cfgd_core::errors::CfgdError`]
/// so that converting to `anyhow::Error` keeps it downcastable — `main.rs` maps
/// exit codes by downcasting the top-level anyhow error to `CfgdError`, so a
/// parse failure must surface its `ConfigError` to earn exit code 4.
#[derive(Debug)]
pub(super) enum ModuleLoadError {
    /// The module's `module.yaml` does not exist.
    NotFound(String),
    /// The module's `module.yaml` exists but could not be read or parsed.
    Parse(cfgd_core::errors::CfgdError),
}

impl ModuleLoadError {
    /// Structured-error code for the `-o json` `error` field. Matches the
    /// spellings used elsewhere (`config_cmd.rs` parse path → `parse_failed`).
    pub(super) fn error_code(&self) -> &'static str {
        match self {
            ModuleLoadError::NotFound(_) => "not_found",
            ModuleLoadError::Parse(_) => "parse_failed",
        }
    }
}

impl std::fmt::Display for ModuleLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModuleLoadError::NotFound(msg) => f.write_str(msg),
            ModuleLoadError::Parse(err) => write!(f, "{err}"),
        }
    }
}

impl From<ModuleLoadError> for anyhow::Error {
    fn from(err: ModuleLoadError) -> Self {
        match err {
            // Preserve the typed CfgdError as the top-level anyhow error so
            // main.rs::exit_code_for_anyhow downcasts it to the parse exit code.
            ModuleLoadError::Parse(inner) => inner.into(),
            ModuleLoadError::NotFound(msg) => anyhow::anyhow!(msg),
        }
    }
}

pub(super) fn load_module_document(
    config_dir: &Path,
    module_name: &str,
) -> Result<(config::ModuleDocument, PathBuf), ModuleLoadError> {
    let module_dir = config_dir.join("modules").join(module_name);
    let module_yaml = module_dir.join("module.yaml");
    if !module_yaml.exists() {
        return Err(ModuleLoadError::NotFound(format!(
            "Module '{}' not found at {}",
            module_name,
            module_yaml.posix()
        )));
    }
    let contents = std::fs::read_to_string(&module_yaml)
        .map_err(|e| ModuleLoadError::Parse(cfgd_core::errors::CfgdError::Io(e)))?;
    let doc = config::parse_module(&contents).map_err(ModuleLoadError::Parse)?;
    Ok((doc, module_yaml))
}

pub(super) fn profiles_using_module(
    profiles_dir: &Path,
    module_name: &str,
) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    for prof in cfgd_core::config::scan_profile_manifests(profiles_dir)
        .map_err(cfgd_core::errors::CfgdError::Config)?
    {
        // For an ambiguous name the winning form is unknowable; the impact
        // list must never under-report, so a reference in ANY candidate
        // manifest marks the profile as affected.
        let uses = prof.paths.iter().any(|path| {
            config::load_profile(path)
                .map(|doc| doc.spec.modules.iter().any(|m| m == module_name))
                .unwrap_or(false)
        });
        if uses {
            result.push(prof.name);
        }
    }
    Ok(result)
}

/// Parse helm-style `--set` overrides and apply them to a ModuleDocument.
/// Supported paths:
///   package.<name>.minVersion=<value>
///   package.<name>.prefer=<a>,<b>,<c>
///   package.<name>.alias.<manager>=<alias>
///   package.<name>.platforms=<a>,<b>
///   package.<name>.script=<value>
pub(super) fn apply_module_sets(
    sets: &[String],
    doc: &mut config::ModuleDocument,
) -> anyhow::Result<()> {
    for set_str in sets {
        let (path, value) = set_str.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("Invalid --set format '{}' — expected key=value", set_str)
        })?;

        let parts: Vec<&str> = path.split('.').collect();
        if parts.len() < 3 || parts[0] != "package" || parts[1].is_empty() || parts[2].is_empty() {
            anyhow::bail!(
                "Invalid --set path '{}' — expected package.<name>.<field>[.<subfield>]",
                path
            );
        }

        let pkg_name = parts[1];
        let field = parts[2];

        let pkg = doc
            .spec
            .packages
            .iter_mut()
            .find(|p| p.name == pkg_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Package '{}' not found in module — add it with --package first",
                    pkg_name
                )
            })?;

        match field {
            "minVersion" => {
                pkg.min_version = Some(value.to_string());
            }
            "prefer" => {
                pkg.prefer = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "platforms" => {
                pkg.platforms = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "deny" => {
                pkg.deny = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "script" => {
                pkg.script = Some(value.to_string());
            }
            "alias" => {
                if parts.len() < 4 {
                    anyhow::bail!(
                        "Invalid alias path '{}' — expected package.<name>.alias.<manager>=<alias>",
                        path
                    );
                }
                let manager = parts[3];
                pkg.aliases.insert(manager.to_string(), value.to_string());
            }
            _ => {
                anyhow::bail!(
                    "Unknown package field '{}' — valid fields: minVersion, prefer, deny, platforms, script, alias",
                    field
                );
            }
        }
    }
    Ok(())
}
// --- Submodule declarations ---

mod build;
mod crud;
mod export;
mod io;
mod keys;
pub mod list_show;
mod push_pull;
mod registry;
mod signature;

#[cfg(test)]
mod tests;

// --- Re-export pub(super) handlers so cli::mod can dispatch to them ---

pub use build::cmd_module_build;
pub use crud::{cmd_module_create, cmd_module_delete, cmd_module_edit, cmd_module_update_local};
pub use export::cmd_module_export;
pub use keys::{cmd_module_keys_generate, cmd_module_keys_list, cmd_module_keys_rotate};
pub(super) use list_show::{cmd_module_list, cmd_module_show};
pub use push_pull::{PushOptions, cmd_module_pull, cmd_module_push};
#[cfg(test)]
pub(super) use registry::build_registry_module_url;
pub use registry::{
    cmd_module_add_from_registry, cmd_module_add_remote, cmd_module_registry_add,
    cmd_module_registry_list, cmd_module_registry_remove, cmd_module_registry_rename,
    cmd_module_search, cmd_module_upgrade,
};

// --- Cross-submodule helpers (private to cli::module) ---

#[cfg(test)]
use export::export_devcontainer;
use io::{save_module_document, scaffold_module_document};
use keys::mask_value;
use signature::enforce_signature_policy;
