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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_has_correct_shape() {
        let c = minimal_config();
        assert_eq!(c.api_version, crate::API_VERSION);
        assert_eq!(c.kind, "Config");
        assert_eq!(c.metadata.name, "default");
        assert!(c.spec.profile.is_none());
        assert!(c.spec.origin.is_empty());
    }

    #[test]
    fn active_profile_returns_error_when_none() {
        let c = minimal_config();
        assert!(c.active_profile().is_err());
    }

    #[test]
    fn active_profile_returns_error_when_empty_string() {
        let mut c = minimal_config();
        c.spec.profile = Some(String::new());
        assert!(c.active_profile().is_err());
    }

    #[test]
    fn active_profile_returns_name_when_set() {
        let mut c = minimal_config();
        c.spec.profile = Some("work".to_string());
        assert_eq!(c.active_profile().unwrap(), "work");
    }

    #[test]
    fn primary_origin_none_when_empty() {
        let spec = ConfigSpec::default();
        assert!(spec.primary_origin().is_none());
    }

    #[test]
    fn primary_origin_returns_first() {
        let mut spec = ConfigSpec::default();
        spec.origin.push(OriginSpec {
            origin_type: crate::config::OriginType::Git,
            url: "https://example.com/dotfiles.git".to_string(),
            branch: "main".to_string(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        });
        assert_eq!(
            spec.primary_origin().unwrap().url,
            "https://example.com/dotfiles.git"
        );
    }

    #[test]
    fn is_yaml_ext_accepts_yaml_and_yml() {
        assert!(is_yaml_ext(Path::new("foo.yaml")));
        assert!(is_yaml_ext(Path::new("bar.yml")));
        assert!(!is_yaml_ext(Path::new("baz.toml")));
        assert!(!is_yaml_ext(Path::new("noext")));
    }

    #[test]
    fn for_each_yaml_file_nonexistent_dir_is_ok() {
        let r = for_each_yaml_file(Path::new("/nonexistent/path/xyz"), |_| Ok(()));
        assert!(r.is_ok());
    }

    #[test]
    fn cfgd_config_rejects_unknown_top_level_fields() {
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nbogusField: nope\nmetadata:\n  name: t\nspec: {}\n";
        let err = serde_yaml::from_str::<CfgdConfig>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogusField");
        let msg = format!("{}", err);
        assert!(
            msg.contains("unknown field"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn config_spec_rejects_unknown_field_typo() {
        // Real-world scenario: a typo at the spec level should be caught (e.g.
        // `securty:` instead of `security:`). Surfaces drift-style typos.
        let yaml = "profile: default\nsecurty: {}\n";
        let err = serde_yaml::from_str::<ConfigSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject securty typo");
        let msg = format!("{}", err);
        assert!(
            msg.contains("unknown field") && msg.contains("securty"),
            "expected unknown-field error mentioning securty, got: {msg}"
        );
    }

    #[test]
    fn for_each_yaml_file_visits_yaml_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.yaml"), "").unwrap();
        std::fs::write(dir.path().join("b.yml"), "").unwrap();
        std::fs::write(dir.path().join("c.toml"), "").unwrap();
        let mut visited = Vec::new();
        for_each_yaml_file(dir.path(), |p| {
            visited.push(p.file_name().unwrap().to_string_lossy().to_string());
            Ok(())
        })
        .unwrap();
        visited.sort();
        assert_eq!(visited, vec!["a.yaml", "b.yml"]);
    }
}
