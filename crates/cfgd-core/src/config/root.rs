use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::ai::AiConfig;
use super::compliance::ComplianceConfig;
use super::daemon::DaemonConfig;
use super::origin::OriginSpec;
use super::profile_spec::FileStrategy;
use super::security::{ModulesConfig, SecurityConfig};
use super::source::SourceSpec;
use super::sync_secrets::SecretsConfig;
use super::theme::ThemeConfig;
use crate::errors::Result;

// --- Root Config (cfgd.yaml) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgdConfig {
    pub api_version: String,
    pub kind: String,
    pub metadata: ConfigMetadata,
    pub spec: ConfigSpec,
}

impl CfgdConfig {
    /// Returns the active profile name, or an error if no profile is configured.
    pub fn active_profile(&self) -> Result<&str> {
        self.spec
            .profile
            .as_deref()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                crate::errors::CfgdError::Config(crate::errors::ConfigError::Invalid {
                    message: "no profile configured — run: cfgd profile create <name>".to_string(),
                })
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSpec {
    #[serde(default)]
    pub profile: Option<String>,

    #[serde(default)]
    pub origin: Vec<OriginSpec>,

    #[serde(default)]
    pub daemon: Option<DaemonConfig>,

    #[serde(default)]
    pub secrets: Option<SecretsConfig>,

    #[serde(default)]
    pub sources: Vec<SourceSpec>,

    #[serde(default)]
    pub theme: Option<ThemeConfig>,

    /// Module configuration: registries and security.
    #[serde(default)]
    pub modules: Option<ModulesConfig>,

    /// Global default file deployment strategy. Per-file overrides take precedence.
    #[serde(default)]
    pub file_strategy: FileStrategy,

    /// Security settings for source signature verification.
    #[serde(default)]
    pub security: Option<SecurityConfig>,

    /// CLI aliases: map of alias name → command string.
    /// Built-in defaults (add, remove) can be overridden or extended.
    #[serde(default)]
    pub aliases: HashMap<String, String>,

    /// AI assistant configuration: provider, model, and API key env var.
    #[serde(default)]
    pub ai: Option<AiConfig>,

    /// Compliance snapshot configuration.
    #[serde(default)]
    pub compliance: Option<ComplianceConfig>,
}

/// Returns `true` if `path` has a YAML extension (`.yaml` or `.yml`,
/// case-sensitive to match the rest of cfgd).
///
/// Use this instead of inlining `ext == "yaml" || ext == "yml"` checks when
/// iterating module / profile directories — keeps the "what counts as a YAML
/// file" decision in one place.
pub fn is_yaml_ext(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "yaml" || e == "yml")
}

/// Iterate over every `.yaml` / `.yml` file in `dir`, invoking `f(path)` for each.
///
/// - Non-existent `dir` is **not** an error — yields nothing.
/// - Non-YAML entries, subdirectories, and unreadable entries are silently skipped.
/// - `f`'s error short-circuits the walk (first error wins).
///
/// Use this instead of open-coding `std::fs::read_dir` + `is_yaml_ext` checks
/// when scanning `<config_dir>/profiles` / `<config_dir>/modules` trees.
pub fn for_each_yaml_file<F>(dir: &Path, mut f: F) -> std::io::Result<()>
where
    F: FnMut(&Path) -> std::io::Result<()>,
{
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if is_yaml_ext(&path) {
            f(&path)?;
        }
    }
    Ok(())
}

/// Build a minimal CfgdConfig for module-only operations that don't have cfgd.yaml.
pub fn minimal_config() -> CfgdConfig {
    CfgdConfig {
        api_version: crate::API_VERSION.to_string(),
        kind: "Config".to_string(),
        metadata: ConfigMetadata {
            name: "default".to_string(),
        },
        spec: ConfigSpec::default(),
    }
}

// Custom deserialization: origin can be a single object or an array
// Internally always Vec<OriginSpec> with primary at index 0
impl ConfigSpec {
    pub fn primary_origin(&self) -> Option<&OriginSpec> {
        self.origin.first()
    }
}
