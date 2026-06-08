use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::origin::OriginSpec;
use super::profile_spec::{EncryptionConstraint, ManagedFileSpec, PackagesSpec, SecretSpec};

// --- Multi-source config management ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSpec {
    pub name: String,
    pub origin: OriginSpec,
    #[serde(default)]
    pub subscription: SubscriptionSpec,
    #[serde(default)]
    pub sync: SourceSyncSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default = "default_source_priority")]
    pub priority: u32,
    #[serde(default)]
    pub accept_recommended: bool,
    #[serde(default)]
    pub opt_in: Vec<String>,
    /// Subscriber opt-in to run lifecycle scripts (profile-layer and
    /// source-delivered module bodies) from this source even when the source's
    /// `constraints.no_scripts` would otherwise reject them. Default `false`:
    /// the source's own `no_scripts` constraint governs.
    #[serde(default)]
    pub allow_scripts: bool,
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
            allow_scripts: false,
            overrides: serde_yaml::Value::Null,
            reject: serde_yaml::Value::Null,
        }
    }
}

fn default_source_priority() -> u32 {
    500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ConfigSourceMetadata,
    pub spec: ConfigSourceSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceMetadata {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceSpec {
    #[serde(default)]
    pub provides: ConfigSourceProvides,
    #[serde(default)]
    pub policy: ConfigSourcePolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            name: String,
            value: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        crate::validate_env_var_user_name(&raw.name).map_err(serde::de::Error::custom)?;
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
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_spec_rejects_unknown_field() {
        // `sourcees:`-style typos at the source level should error loudly.
        let yaml = r#"name: team
origin:
  type: Git
  url: https://example.com/x.git
bogusField: 1
"#;
        let err = serde_yaml::from_str::<SourceSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogusField");
        let msg = format!("{}", err);
        assert!(
            msg.contains("unknown field") && msg.contains("bogusField"),
            "expected unknown-field error mentioning bogusField, got: {msg}"
        );
    }

    #[test]
    fn subscription_spec_rejects_unknown_field() {
        let yaml = "priority: 100\nautoApply: true\n";
        let err = serde_yaml::from_str::<SubscriptionSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject autoApply (belongs on sync)");
        assert!(format!("{}", err).contains("unknown field"));
    }

    #[test]
    fn subscription_spec_parses_allow_scripts() {
        let yaml = "priority: 100\nallowScripts: true\n";
        let spec: SubscriptionSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(spec.allow_scripts);
        assert_eq!(spec.priority, 100);
    }

    #[test]
    fn subscription_spec_allow_scripts_defaults_false() {
        let yaml = "priority: 100\n";
        let spec: SubscriptionSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(!spec.allow_scripts);
        assert!(!SubscriptionSpec::default().allow_scripts);
    }

    #[test]
    fn env_var_rejects_cfgd_prefix_at_parse_time() {
        let yaml = r#"
- name: CFGD_FOO
  value: bar
"#;
        let err = serde_yaml::from_str::<Vec<EnvVar>>(yaml)
            .expect_err("CFGD_* env var names must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("reserved"),
            "error should mention 'reserved': {msg}"
        );
        assert!(
            msg.contains("CFGD_FOO"),
            "error should name the offending var: {msg}"
        );
    }

    #[test]
    fn env_var_accepts_normal_names() {
        let yaml = r#"
- name: MY_APP_KEY
  value: hello
- name: PATH
  value: /usr/bin
"#;
        let vars: Vec<EnvVar> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "MY_APP_KEY");
        assert_eq!(vars[1].name, "PATH");
    }
}
