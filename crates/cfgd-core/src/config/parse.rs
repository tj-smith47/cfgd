use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::ai::AiConfig;
use super::compliance::ComplianceConfig;
use super::daemon::DaemonConfig;
use super::origin::OriginSpec;
use super::profile_spec::{FileStrategy, ProfileDocument};
use super::root::{CfgdConfig, ConfigMetadata, ConfigSpec};
use super::security::{ModulesConfig, SecurityConfig};
use super::source::{ConfigSourceDocument, SourceSpec};
use super::sync_secrets::SecretsConfig;
use super::theme::ThemeConfig;
use crate::PathDisplayExt;
use crate::errors::{ConfigError, Result};

/// Maximum number of YAML anchors (`&name`) allowed in a single document.
/// Prevents billion-laughs-style anchor/alias expansion attacks (CVE-2019-11253).
const MAX_YAML_ANCHORS: usize = 256;

/// Pre-parse check for YAML anchor/alias bomb attacks.
/// Counts anchor definitions (`&name`) and rejects documents exceeding the limit.
pub(super) fn check_yaml_anchor_limit(contents: &str, context: &Path) -> Result<()> {
    // Count lines containing YAML anchor definitions (& followed by an identifier).
    // This is a conservative heuristic — it may count `&` in strings, but that's
    // acceptable since legitimate configs rarely have hundreds of anchors.
    let anchor_count = contents
        .as_bytes()
        .windows(2)
        .filter(|w| w[0] == b'&' && (w[1].is_ascii_alphanumeric() || w[1] == b'_'))
        .count();

    if anchor_count > MAX_YAML_ANCHORS {
        return Err(ConfigError::Invalid {
            message: format!(
                "{}: too many YAML anchors ({}, max {}) — possible anchor/alias bomb",
                context.posix(),
                anchor_count,
                MAX_YAML_ANCHORS
            ),
        }
        .into());
    }
    Ok(())
}

/// Emit `tracing::warn!` for any legacy `theme.overrides.*` keys present in the
/// raw YAML. Removed keys (subheader/key/value/iconInfo) and renamed keys
/// (iconSuccess→iconOk, iconWarning→iconWarn, iconError→iconFail) are silently
/// dropped by `ThemeOverrides`'s typed deserialize; this pre-pass surfaces them
/// so users can migrate their `cfgd.yaml` instead of wondering why an override
/// did nothing.
pub(super) fn warn_on_legacy_theme_keys(raw_yaml: &str) {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(raw_yaml) else {
        return;
    };
    let overrides = value
        .get("spec")
        .and_then(|s| s.get("theme"))
        .and_then(|t| t.get("overrides"));
    let Some(serde_yaml::Value::Mapping(m)) = overrides else {
        return;
    };

    let removed_keys = ["subheader", "key", "value", "iconInfo"];
    let renamed_keys = [
        ("iconSuccess", "iconOk"),
        ("iconWarning", "iconWarn"),
        ("iconError", "iconFail"),
    ];

    for k in removed_keys {
        if m.contains_key(serde_yaml::Value::String(k.into())) {
            tracing::warn!(
                "config: theme.overrides.{k} is no longer supported (capability removed in v0.4); \
                 the field will be ignored. Remove it from your cfgd.yaml."
            );
        }
    }
    for (old, new) in renamed_keys {
        if m.contains_key(serde_yaml::Value::String(old.into())) {
            tracing::warn!(
                "config: theme.overrides.{old} is renamed to {new}; \
                 the old name still works for now but will be removed in a future release."
            );
        }
    }
}

/// Parse a ConfigSource manifest from YAML content.
pub fn parse_config_source(contents: &str) -> Result<ConfigSourceDocument> {
    check_yaml_anchor_limit(contents, Path::new("ConfigSource"))?;
    let doc: ConfigSourceDocument = serde_yaml::from_str(contents).map_err(ConfigError::from)?;

    if doc.kind != "ConfigSource" {
        return Err(ConfigError::Invalid {
            message: format!("expected kind 'ConfigSource', got '{}'", doc.kind),
        }
        .into());
    }

    Ok(doc)
}

/// Load and parse the root cfgd.yaml config file
pub fn load_config(path: &Path) -> Result<CfgdConfig> {
    if !path.exists() {
        // A leading `~` survived expansion only because no home directory could
        // be resolved (HOME unset on Unix, USERPROFILE/HOME unset on Windows).
        // Reporting the literal `~` leaves the user no path to act on, so name
        // the cause instead.
        if path.starts_with("~") {
            return Err(ConfigError::HomeUnresolved {
                path: path.to_path_buf(),
            }
            .into());
        }
        return Err(ConfigError::NotFound {
            path: path.to_path_buf(),
        }
        .into());
    }

    // Reject excessively large config files to prevent memory exhaustion (YAML bomb defense)
    const MAX_CONFIG_SIZE: u64 = 50 * 1024 * 1024; // 50 MB
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_CONFIG_SIZE
    {
        return Err(ConfigError::Invalid {
            message: format!(
                "{} is too large ({} bytes, max {})",
                path.posix(),
                meta.len(),
                MAX_CONFIG_SIZE
            ),
        }
        .into());
    }

    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Invalid {
        message: format!("failed to read {}: {}", path.posix(), e),
    })?;

    parse_config(&contents, path)
}

/// Parse config from string, supporting both YAML and TOML based on file extension
pub fn parse_config(contents: &str, path: &Path) -> Result<CfgdConfig> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("yaml");

    if ext != "toml" {
        check_yaml_anchor_limit(contents, path)?;
        warn_on_legacy_theme_keys(contents);
    }

    let raw: RawCfgdConfig = match ext {
        "toml" => toml::from_str(contents).map_err(ConfigError::from)?,
        _ => serde_yaml::from_str(contents).map_err(ConfigError::from)?,
    };

    // Normalize origin to Vec
    let origin = match raw.spec.origin {
        Some(RawOrigin::Single(o)) => vec![o],
        Some(RawOrigin::Multiple(v)) => v,
        None => vec![],
    };

    Ok(CfgdConfig {
        api_version: raw.api_version,
        kind: raw.kind,
        metadata: raw.metadata,
        spec: ConfigSpec {
            profile: raw.spec.profile,
            origin,
            daemon: raw.spec.daemon,
            secrets: raw.spec.secrets,
            sources: raw.spec.sources,
            theme: raw.spec.theme,
            modules: raw.spec.modules,
            file_strategy: raw.spec.file_strategy,
            security: raw.spec.security,
            aliases: raw.spec.aliases,
            ai: raw.spec.ai,
            compliance: raw.spec.compliance,
        },
    })
}

// Internal raw types for flexible origin deserialization
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawCfgdConfig {
    api_version: String,
    kind: String,
    metadata: ConfigMetadata,
    spec: RawConfigSpec,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawConfigSpec {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    origin: Option<RawOrigin>,
    #[serde(default)]
    daemon: Option<DaemonConfig>,
    #[serde(default)]
    secrets: Option<SecretsConfig>,
    #[serde(default)]
    sources: Vec<SourceSpec>,
    #[serde(default)]
    theme: Option<ThemeConfig>,
    #[serde(default)]
    modules: Option<ModulesConfig>,
    #[serde(default)]
    file_strategy: FileStrategy,
    #[serde(default)]
    security: Option<SecurityConfig>,
    #[serde(default)]
    aliases: HashMap<String, String>,
    #[serde(default)]
    ai: Option<AiConfig>,
    #[serde(default)]
    compliance: Option<ComplianceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawOrigin {
    Single(OriginSpec),
    Multiple(Vec<OriginSpec>),
}

/// Load a profile document from a YAML file
pub fn load_profile(path: &Path) -> Result<ProfileDocument> {
    if !path.exists() {
        return Err(ConfigError::NotFound {
            path: path.to_path_buf(),
        }
        .into());
    }

    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Invalid {
        message: format!("failed to read {}: {}", path.posix(), e),
    })?;

    check_yaml_anchor_limit(&contents, path)?;
    let doc: ProfileDocument = serde_yaml::from_str(&contents).map_err(ConfigError::from)?;
    Ok(doc)
}

/// Find a profile file by name, checking `.yaml` then `.yml` extensions.
pub(super) fn find_profile_path(profiles_dir: &Path, name: &str) -> PathBuf {
    let yaml_path = profiles_dir.join(format!("{}.yaml", name));
    if yaml_path.exists() {
        return yaml_path;
    }
    let yml_path = profiles_dir.join(format!("{}.yml", name));
    if yml_path.exists() {
        return yml_path;
    }
    // Fall back to .yaml so load_profile produces the expected error
    yaml_path
}
