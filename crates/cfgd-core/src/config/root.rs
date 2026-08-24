use std::collections::HashMap;

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

/// The root `cfgd.yaml` document: a KRM-style envelope (`apiVersion`/`kind`/
/// `metadata`/`spec`) around a machine's declared configuration.
///
/// ```yaml
/// apiVersion: cfgd.io/v1alpha1
/// kind: CfgdConfig
/// metadata:
///   name: my-machine
/// spec:
///   profile: work
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CfgdConfig {
    /// API group/version, e.g. `cfgd.io/v1alpha1`. See `API_VERSION`.
    pub api_version: String,
    /// Document kind. Always `CfgdConfig` for this file.
    pub kind: String,
    /// Identifying metadata for this config document.
    pub metadata: ConfigMetadata,
    /// The body of the document: everything cfgd reads to decide what this
    /// machine should look like.
    pub spec: ConfigSpec,
    /// Deprecation messages collected while parsing (e.g. legacy `theme.overrides.*`
    /// keys). Not part of the schema: never serialized, never compared. A command
    /// boundary that owns a terminal drains these through `printer.deprecation()`.
    #[serde(skip)]
    pub deprecations: Vec<String>,
}

impl CfgdConfig {
    /// Drain [`Self::deprecations`] through `printer.deprecation()`, the
    /// always-visible stderr channel, THEN CLEAR IT — a second drain of the
    /// same `CfgdConfig` is a no-op rather than a repeat print. `&mut self`
    /// is deliberate: a command that loads config once and drains once
    /// through the normal path, then falls through a secondary code path
    /// that drains the SAME already-loaded value again (a `--module`
    /// fallback that shares its `cfg` binding with the primary load, for
    /// instance), must not surface one legacy-key notice twice.
    ///
    /// The one shared implementation behind every command-boundary drain
    /// (`crate::cli::helpers::drain_config_deprecations` in the binary
    /// crate) and the daemon's own startup / SIGHUP-reload sites, which
    /// parse config directly in cfgd-core and so cannot reach the
    /// binary-crate helper.
    pub fn drain_deprecations(&mut self, printer: &crate::output::Printer) {
        for msg in &self.deprecations {
            printer.deprecation(msg);
        }
        self.deprecations.clear();
    }

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

/// `metadata`: identifying information for a `cfgd.yaml` document.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigMetadata {
    /// A human-chosen name for this machine's config, shown in status output.
    pub name: String,
}

/// `spec`: the body of a `cfgd.yaml` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSpec {
    /// Name of the active `ProfileSpec` to reconcile against.
    #[serde(default)]
    pub profile: Option<String>,

    /// Git origins this config's changes may be pushed to / pulled from.
    #[serde(default)]
    pub origin: Vec<OriginSpec>,

    /// The background daemon that watches for drift between reconciles.
    /// Omitted, no daemon runs and every reconcile is an explicit
    /// `cfgd apply`.
    #[serde(default)]
    pub daemon: Option<DaemonConfig>,

    /// Which backend resolves a `${secret:…}` reference, and how it is
    /// reached. Omitted, no backend is configured and a declared secret
    /// reference fails to resolve.
    #[serde(default)]
    pub secrets: Option<SecretsConfig>,

    /// Additional config sources this machine subscribes to.
    #[serde(default)]
    pub sources: Vec<SourceSpec>,

    /// Colours and glyphs cfgd renders with: a named preset (`default`,
    /// `dracula`, `solarized-dark`, `solarized-light`, `minimal`) plus
    /// per-slot overrides. Omitted, the `default` preset applies.
    #[serde(default)]
    pub theme: Option<ThemeConfig>,

    /// Module configuration: registries and security.
    #[serde(default)]
    pub modules: Option<ModulesConfig>,

    /// Global default file deployment strategy. Per-file overrides take precedence.
    ///
    /// `Patch` is rejected here: it is defined by a per-file `patch:` block,
    /// which a file inheriting the global default cannot have.
    #[serde(default)]
    #[schemars(schema_with = "global_file_strategy_schema")]
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

    /// Periodic snapshots of machine state, for drift history and audit.
    /// Omitted, no snapshots are taken and `cfgd compliance` reports only
    /// what it collects on the spot.
    #[serde(default)]
    pub compliance: Option<ComplianceConfig>,

    /// Update policy for the cfgd binary and authored skills.
    #[serde(default)]
    pub update: Option<UpdateConfig>,
}

/// Schema for `spec.fileStrategy`: the [`FileStrategy`] variants minus `Patch`.
///
/// Editors validate `cfgd.yaml` against the published schema, so the value set
/// they offer must match what the parser accepts — `Patch` is rejected as a
/// global default (see `validate_global_file_strategy`).
fn global_file_strategy_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let accepted: Vec<&'static str> = FileStrategy::ALL
        .iter()
        .filter(|s| s.valid_as_global_default())
        .map(|s| s.as_str())
        .collect();
    schemars::json_schema!({
        "type": "string",
        "enum": accepted,
        "default": FileStrategy::default().as_str(),
    })
}

/// Update policy governing how cfgd self-update checks behave.
///
/// `Auto` applies updates without prompting, `Prompt` asks before applying,
/// `Notify` only reports that an update is available, and `Manual` disables
/// automatic checks entirely (the user runs `cfgd upgrade` themselves).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum UpdatePolicy {
    /// Apply updates automatically without prompting.
    Auto,
    /// Ask before applying an available update.
    #[default]
    Prompt,
    /// Report that an update is available, but take no action.
    Notify,
    /// Disable automatic update checks; the user upgrades manually.
    Manual,
}

case_insensitive_enum!(UpdatePolicy {
    "Auto" => UpdatePolicy::Auto,
    "Prompt" => UpdatePolicy::Prompt,
    "Notify" => UpdatePolicy::Notify,
    "Manual" => UpdatePolicy::Manual,
});

/// Per-skill update policy. Mirrors `UpdatePolicy` but adds `Inherit`, which
/// defers to the binary-level `update.policy`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum SkillUpdatePolicy {
    /// Defer to the binary-level update policy (`update.policy`).
    #[default]
    Inherit,
    /// Apply skill updates automatically without prompting.
    Auto,
    /// Ask before applying an available skill update.
    Prompt,
    /// Report that a skill update is available, but take no action.
    Notify,
    /// Disable automatic skill update checks; the user updates manually.
    Manual,
}

case_insensitive_enum!(SkillUpdatePolicy {
    "Inherit" => SkillUpdatePolicy::Inherit,
    "Auto" => SkillUpdatePolicy::Auto,
    "Prompt" => SkillUpdatePolicy::Prompt,
    "Notify" => SkillUpdatePolicy::Notify,
    "Manual" => SkillUpdatePolicy::Manual,
});

/// Configuration for cfgd self-update checks and authored-skill updates.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateConfig {
    /// How update checks for the cfgd binary behave. Defaults to `Prompt`.
    #[serde(default)]
    pub policy: UpdatePolicy,

    /// How often to check for updates, as a duration string (e.g. `24h`, `7d`,
    /// `30m`) or a plain number of seconds. Defaults to `24h`.
    #[serde(default = "default_update_interval")]
    pub interval: String,

    /// Release channel to track (e.g. `stable`, `beta`). When unset, cfgd uses
    /// its built-in default channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,

    /// Update policy for authored skills. Defaults to inheriting `policy`.
    #[serde(default)]
    pub skills: SkillUpdateConfig,
}

impl Default for UpdateConfig {
    /// Mirrors deserializing an empty `update:` block: serde field defaults
    /// fire on deserialize but not on a derived `Default`, so `interval` must
    /// be set to `default_update_interval()` here or `Default::default()`
    /// yields an empty interval that fails to parse as a duration.
    fn default() -> Self {
        Self {
            policy: UpdatePolicy::default(),
            interval: default_update_interval(),
            channel: None,
            skills: SkillUpdateConfig::default(),
        }
    }
}

impl UpdateConfig {
    /// Resolve the effective update policy for authored skills, collapsing
    /// `SkillUpdatePolicy::Inherit` to the binary-level [`UpdateConfig::policy`].
    pub fn effective_skill_policy(&self) -> UpdatePolicy {
        match self.skills.policy {
            SkillUpdatePolicy::Inherit => self.policy,
            SkillUpdatePolicy::Auto => UpdatePolicy::Auto,
            SkillUpdatePolicy::Prompt => UpdatePolicy::Prompt,
            SkillUpdatePolicy::Notify => UpdatePolicy::Notify,
            SkillUpdatePolicy::Manual => UpdatePolicy::Manual,
        }
    }
}

/// Update configuration specific to authored skills.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillUpdateConfig {
    /// How skill update checks behave. Defaults to `Inherit` (defer to the
    /// binary-level policy).
    #[serde(default)]
    pub policy: SkillUpdatePolicy,
}

fn default_update_interval() -> String {
    "24h".to_string()
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
        deprecations: Vec::new(),
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
    fn update_config_parses_explicit_skill_override() {
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: c\nspec:\n  profile: base\n  update:\n    policy: Notify\n    skills:\n      policy: Manual\n";
        let cfg: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
        let u = cfg.spec.update.unwrap();
        assert!(matches!(u.policy, UpdatePolicy::Notify));
        assert!(matches!(u.skills.policy, SkillUpdatePolicy::Manual));
    }

    #[test]
    fn update_defaults_are_prompt_and_inherit() {
        let u = UpdateConfig::default();
        assert!(matches!(u.policy, UpdatePolicy::Prompt));
        assert!(matches!(u.skills.policy, SkillUpdatePolicy::Inherit));
    }

    #[test]
    fn update_config_default_matches_empty_deserialize() {
        // Default::default() must equal deserialize-of-empty: serde field
        // defaults only fire on deserialize, so a derived Default leaves
        // `interval` empty and the update check warns spuriously on every run.
        let from_empty: UpdateConfig = serde_yaml::from_str("{}").unwrap();
        let from_default = UpdateConfig::default();
        assert_eq!(
            from_default.interval, from_empty.interval,
            "Default must match deserialize-of-empty"
        );
        assert!(
            !from_default.interval.is_empty(),
            "default interval must not be empty"
        );
        assert_eq!(from_default.interval, "24h");
    }

    #[test]
    fn inherit_resolves_to_binary_policy() {
        let u = UpdateConfig {
            policy: UpdatePolicy::Auto,
            ..Default::default()
        }; // skills = Inherit
        assert!(matches!(u.effective_skill_policy(), UpdatePolicy::Auto));
    }

    #[test]
    fn effective_skill_policy_resolves_every_variant_against_two_binary_policies() {
        // Inherit tracks the binary policy; each explicit variant passes through
        // unchanged regardless of the binary policy. Exercising two distinct
        // binary policies proves Inherit's binding is the live `policy`, not a
        // hardcoded value, while the explicit arms stay binary-independent.
        for binary in [UpdatePolicy::Auto, UpdatePolicy::Notify] {
            let cases = [
                (SkillUpdatePolicy::Inherit, binary),
                (SkillUpdatePolicy::Auto, UpdatePolicy::Auto),
                (SkillUpdatePolicy::Prompt, UpdatePolicy::Prompt),
                (SkillUpdatePolicy::Notify, UpdatePolicy::Notify),
                (SkillUpdatePolicy::Manual, UpdatePolicy::Manual),
            ];
            for (skills, expected) in cases {
                let u = UpdateConfig {
                    policy: binary,
                    skills: SkillUpdateConfig { policy: skills },
                    ..Default::default()
                };
                assert_eq!(
                    u.effective_skill_policy(),
                    expected,
                    "binary={binary:?} skills={skills:?} must resolve to {expected:?}"
                );
            }
        }
    }

    #[test]
    fn update_policy_parses_case_insensitively() {
        for (token, expected) in [
            ("auto", UpdatePolicy::Auto),
            ("PROMPT", UpdatePolicy::Prompt),
            ("notify", UpdatePolicy::Notify),
            ("Manual", UpdatePolicy::Manual),
        ] {
            let p: UpdatePolicy = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(p, expected, "token {token}");
        }
        serde_yaml::from_str::<UpdatePolicy>("sometimes").expect_err("garbage must error");
    }

    #[test]
    fn skill_update_policy_parses_case_insensitively() {
        for (token, expected) in [
            ("inherit", SkillUpdatePolicy::Inherit),
            ("Inherit", SkillUpdatePolicy::Inherit),
            ("auto", SkillUpdatePolicy::Auto),
            ("PROMPT", SkillUpdatePolicy::Prompt),
            ("notify", SkillUpdatePolicy::Notify),
            ("Manual", SkillUpdatePolicy::Manual),
        ] {
            let p: SkillUpdatePolicy = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(p, expected, "token {token}");
        }
        serde_yaml::from_str::<SkillUpdatePolicy>("sometimes").expect_err("garbage must error");
    }
}
