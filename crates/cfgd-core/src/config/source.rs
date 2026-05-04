use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::origin::OriginSpec;
use super::profile_spec::{EncryptionConstraint, ManagedFileSpec, PackagesSpec, SecretSpec};

// --- Multi-source config management ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpec {
    pub name: String,
    pub origin: OriginSpec,
    #[serde(default)]
    pub subscription: SubscriptionSpec,
    #[serde(default)]
    pub sync: SourceSyncSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default = "default_source_priority")]
    pub priority: u32,
    #[serde(default)]
    pub accept_recommended: bool,
    #[serde(default)]
    pub opt_in: Vec<String>,
    #[serde(default)]
    pub overrides: serde_yaml::Value,
    #[serde(default)]
    pub reject: serde_yaml::Value,
}

impl Default for SubscriptionSpec {
    fn default() -> Self {
        Self {
            profile: None,
            priority: default_source_priority(),
            accept_recommended: false,
            opt_in: Vec::new(),
            overrides: serde_yaml::Value::Null,
            reject: serde_yaml::Value::Null,
        }
    }
}

fn default_source_priority() -> u32 {
    500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSyncSpec {
    #[serde(default = "default_sync_interval")]
    pub interval: String,
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_version: Option<String>,
}

impl Default for SourceSyncSpec {
    fn default() -> Self {
        Self {
            interval: default_sync_interval(),
            auto_apply: false,
            pin_version: None,
        }
    }
}

pub(super) fn default_sync_interval() -> String {
    "1h".to_string()
}

// --- ConfigSource manifest (published by team, lives in source repo as cfgd-source.yaml) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSourceDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ConfigSourceMetadata,
    pub spec: ConfigSourceSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSourceMetadata {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSourceSpec {
    #[serde(default)]
    pub provides: ConfigSourceProvides,
    #[serde(default)]
    pub policy: ConfigSourcePolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSourceProvides {
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub profile_details: Vec<ConfigSourceProfileEntry>,
    #[serde(default)]
    pub platform_profiles: HashMap<String, String>,
    #[serde(default)]
    pub modules: Vec<String>,
}

/// Detailed profile entry in a ConfigSource manifest.
/// When present, provides richer info than the flat `profiles` list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSourceProfileEntry {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub inherits: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSourcePolicy {
    #[serde(default)]
    pub required: PolicyItems,
    #[serde(default)]
    pub recommended: PolicyItems,
    #[serde(default)]
    pub optional: PolicyItems,
    #[serde(default)]
    pub locked: PolicyItems,
    #[serde(default)]
    pub constraints: SourceConstraints,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

impl<'de> Deserialize<'de> for EnvVar {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            name: String,
            value: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        crate::validate_env_var_name(&raw.name).map_err(serde::de::Error::custom)?;
        Ok(EnvVar {
            name: raw.name,
            value: raw.value,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShellAlias {
    pub name: String,
    pub command: String,
}

impl<'de> Deserialize<'de> for ShellAlias {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            name: String,
            command: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        crate::validate_alias_name(&raw.name).map_err(serde::de::Error::custom)?;
        Ok(ShellAlias {
            name: raw.name,
            command: raw.command,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyItems {
    #[serde(default)]
    pub packages: Option<PackagesSpec>,
    #[serde(default)]
    pub files: Vec<ManagedFileSpec>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub aliases: Vec<ShellAlias>,
    #[serde(default)]
    pub system: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub modules: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<SecretSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceConstraints {
    #[serde(default = "default_true")]
    pub no_scripts: bool,
    #[serde(default = "default_true")]
    pub no_secrets_read: bool,
    #[serde(default)]
    pub allowed_target_paths: Vec<String>,
    #[serde(default)]
    pub allow_system_changes: bool,
    /// Require that the HEAD commit in this source's git repo has a valid
    /// GPG or SSH signature. Subscribers can bypass with `security.allow-unsigned`.
    #[serde(default)]
    pub require_signed_commits: bool,
    /// Encryption requirements imposed on files delivered by this source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionConstraint>,
}

impl Default for SourceConstraints {
    fn default() -> Self {
        Self {
            no_scripts: true,
            no_secrets_read: true,
            allowed_target_paths: Vec::new(),
            allow_system_changes: false,
            require_signed_commits: false,
            encryption: None,
        }
    }
}

pub(super) fn default_true() -> bool {
    true
}
