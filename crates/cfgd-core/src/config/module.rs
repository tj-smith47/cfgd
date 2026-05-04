use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::parse::check_yaml_anchor_limit;
use super::profile_spec::{EncryptionSpec, FileStrategy, ScriptSpec};
use super::source::{EnvVar, ShellAlias};
use crate::errors::{ConfigError, Result};

// --- Module ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ModuleMetadata,
    pub spec: ModuleSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleMetadata {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<ModulePackageEntry>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ModuleFileEntry>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<ShellAlias>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<ScriptSpec>,

    /// System configurator settings contributed by this module.
    /// Deep-merged into the profile system map; module values override profile values at leaf level.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub system: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModulePackageEntry {
    #[serde(default)]
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer: Vec<String>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub aliases: HashMap<String, String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleFileEntry {
    pub source: String,
    pub target: String,
    /// Per-file deployment strategy override. If None, uses the global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<FileStrategy>,
    /// When true, the source file is local-only: auto-added to .gitignore,
    /// silently skipped on machines where it doesn't exist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Encryption settings for this module file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScriptEntry {
    Simple(String),
    Full {
        run: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
        /// Kill the script if it produces no stdout/stderr output for this duration.
        /// Prevents scripts from silently hanging on unresponsive resources.
        /// Format: "30s", "2m", etc. If unset, no idle timeout is enforced.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "idleTimeout"
        )]
        idle_timeout: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "continueOnError"
        )]
        continue_on_error: Option<bool>,
    },
}

impl ScriptEntry {
    /// Extract the run command string from any variant.
    pub fn run_str(&self) -> &str {
        match self {
            ScriptEntry::Simple(s) => s,
            ScriptEntry::Full { run, .. } => run,
        }
    }
}

impl std::fmt::Display for ScriptEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.run_str())
    }
}

// --- Module Lockfile ---

/// Lockfile recording pinned remote modules with integrity hashes.
/// Stored at `<config_dir>/modules.lock`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleLockfile {
    #[serde(default)]
    pub modules: Vec<ModuleLockEntry>,
}

/// A single locked remote module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleLockEntry {
    /// Module name (matches metadata.name in the module spec).
    pub name: String,
    /// Git URL of the remote module repository.
    pub url: String,
    /// Pinned git ref — tag or commit SHA (branches not allowed for remote modules).
    pub pinned_ref: String,
    /// Resolved commit SHA at the time of locking.
    pub commit: String,
    /// SHA-256 hash of the module directory contents for integrity verification.
    pub integrity: String,
    /// Subdirectory within the repo containing the module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
}

// --- Module Registries ---

/// A module registry — a git repo containing modules in `modules/<name>/module.yaml` structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleRegistryEntry {
    /// Short name / alias for this source (defaults to GitHub org name).
    pub name: String,
    /// Git URL of the source repository.
    pub url: String,
}

/// Parse a Module document from YAML content.
pub fn parse_module(contents: &str) -> Result<ModuleDocument> {
    check_yaml_anchor_limit(contents, Path::new("Module"))?;
    let doc: ModuleDocument = serde_yaml::from_str(contents).map_err(ConfigError::from)?;

    if doc.kind != "Module" {
        return Err(ConfigError::Invalid {
            message: format!("expected kind 'Module', got '{}'", doc.kind),
        }
        .into());
    }

    Ok(doc)
}
