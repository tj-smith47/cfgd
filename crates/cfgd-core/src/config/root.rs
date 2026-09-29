use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use cfgd_schema::case_insensitive_enum;

use super::ai::AiConfig;
use super::compliance::ComplianceConfig;
use super::daemon::DaemonConfig;
use super::origin::OriginSpec;
use super::output::OutputConfig;
use super::security::{ModulesConfig, SecurityConfig};
use super::source::SourceSpec;
use super::sync_secrets::SecretsConfig;
use super::theme::ThemeConfig;
use crate::errors::Result;
use cfgd_schema::{FileStrategy, MigrationPolicy};

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
    /// The pre-`spec.output` flat keys this document still spells, as
    /// `config::LEGACY_OUTPUT_KEYS` names them. Not part of the schema: never
    /// serialized, never compared. `cfgd doctor` reports one as a row telling
    /// the reader which nested key replaced it.
    #[serde(skip)]
    pub legacy_output_keys: Vec<String>,
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
                    message: "no profile configured — run: `cfgd profile create <name>`"
                        .to_string(),
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
    /// Omitted, a daemon started with `cfgd daemon` runs every default: a
    /// `5m` reconcile that reports drift and applies nothing, with no pull or
    /// push.
    #[serde(default)]
    pub daemon: Option<DaemonConfig>,

    /// Which backend resolves a `${secret:…}` reference, and how it is
    /// reached. Omitted, the `sops` backend resolves it through sops's own
    /// key search.
    #[serde(default)]
    pub secrets: Option<SecretsConfig>,

    /// Additional config sources this machine subscribes to.
    #[serde(default)]
    pub sources: Vec<SourceSpec>,

    /// How cfgd renders what it reports: theme, usage hints, and which
    /// declared env values are masked. Omitted, every default applies.
    #[serde(default)]
    pub output: Option<OutputConfig>,

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

    /// CLI aliases: map of alias name → command string. `cfgd init` scaffolds
    /// `add` and `remove`; cfgd defines no alias of its own.
    #[serde(default, deserialize_with = "crate::config::null_as_default")]
    #[schemars(with = "Option<std::collections::HashMap<String, String>>")]
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

    /// What cfgd does when this document is behind the schema the running
    /// binary reads. `Prompt` asks once on an interactive run; `Warn` only
    /// reports; `Update` writes the alignment; `Ignore` says nothing.
    #[serde(default)]
    pub migration_policy: MigrationPolicy,
}

/// One accessor per `spec` section whose omission production reads as a
/// value: each returns the declared block, or the block the build uses where
/// the document omits it. A block whose omission turns its settings off
/// (`secrets.sops`, where sops then runs its own key search) has no accessor,
/// and its readers handle `None` themselves.
impl ConfigSpec {
    /// The daemon settings: the declared block, or `daemon: {}` where the
    /// document omits it.
    #[must_use]
    pub fn daemon_effective(&self) -> &DaemonConfig {
        static OMITTED: LazyLock<DaemonConfig> = LazyLock::new(DaemonConfig::default);
        self.daemon.as_ref().unwrap_or(&OMITTED)
    }

    /// The output settings: the declared block, or `output: {}` where the
    /// document omits it.
    #[must_use]
    pub fn output_effective(&self) -> &OutputConfig {
        static OMITTED: LazyLock<OutputConfig> = LazyLock::new(OutputConfig::default);
        self.output.as_ref().unwrap_or(&OMITTED)
    }

    /// The module settings: the declared block, or no registries and no
    /// signature requirement where the document omits it.
    #[must_use]
    pub fn modules_effective(&self) -> &ModulesConfig {
        static OMITTED: LazyLock<ModulesConfig> = LazyLock::new(ModulesConfig::default);
        self.modules.as_ref().unwrap_or(&OMITTED)
    }

    /// The source signature settings: the declared block, or unsigned content
    /// refused where the document omits it.
    #[must_use]
    pub fn security_effective(&self) -> &SecurityConfig {
        static OMITTED: LazyLock<SecurityConfig> = LazyLock::new(SecurityConfig::default);
        self.security.as_ref().unwrap_or(&OMITTED)
    }

    /// The AI settings `cfgd generate` runs with: the declared block, or
    /// `ai: {}` where the document omits it.
    #[must_use]
    pub fn ai_effective(&self) -> &AiConfig {
        static OMITTED: LazyLock<AiConfig> = LazyLock::new(AiConfig::default);
        self.ai.as_ref().unwrap_or(&OMITTED)
    }

    /// The compliance settings: the declared block, or `compliance: {}` (no
    /// snapshots taken) where the document omits it.
    #[must_use]
    pub fn compliance_effective(&self) -> &ComplianceConfig {
        static OMITTED: LazyLock<ComplianceConfig> = LazyLock::new(ComplianceConfig::default);
        self.compliance.as_ref().unwrap_or(&OMITTED)
    }

    /// The secrets settings: the declared block, or `secrets: {}` (the `sops`
    /// backend) where the document omits it.
    #[must_use]
    pub fn secrets_effective(&self) -> &SecretsConfig {
        static OMITTED: LazyLock<SecretsConfig> = LazyLock::new(SecretsConfig::default);
        self.secrets.as_ref().unwrap_or(&OMITTED)
    }

    /// The update settings: the declared block, or `update: {}` where the
    /// document omits it.
    #[must_use]
    pub fn update_effective(&self) -> &UpdateConfig {
        static OMITTED: LazyLock<UpdateConfig> = LazyLock::new(UpdateConfig::default);
        self.update.as_ref().unwrap_or(&OMITTED)
    }

    /// This spec with every section the build reads through an `_effective`
    /// accessor filled in with that accessor's value, nested sections
    /// included. `cfgd config get` answers from it, so a key under an omitted
    /// section reports what the build uses.
    #[must_use]
    pub fn effective(&self) -> ConfigSpec {
        let mut spec = self.clone();
        let mut daemon = self.daemon_effective().clone();
        let mut reconcile = daemon.reconcile_effective().clone();
        reconcile.policy = Some(reconcile.policy_effective().clone());
        daemon.sync = Some(daemon.sync_effective().clone());
        daemon.notify = Some(daemon.notify_effective().clone());
        daemon.reconcile = Some(reconcile);
        spec.daemon = Some(daemon);
        let mut output = self.output_effective().clone();
        output.theme = Some(output.theme_effective().clone());
        output.usage_hints = Some(output.usage_hints_effective());
        output.mask_env_values = Some(output.mask_env_values_effective());
        spec.output = Some(output);
        let mut modules = self.modules_effective().clone();
        modules.security = Some(modules.security_effective().clone());
        spec.modules = Some(modules);
        spec.security = Some(self.security_effective().clone());
        spec.ai = Some(self.ai_effective().clone());
        spec.compliance = Some(self.compliance_effective().clone());
        let mut update = self.update_effective().clone();
        update.channel = Some(update.channel_effective().to_string());
        spec.update = Some(update);
        spec.secrets = Some(self.secrets_effective().clone());
        spec
    }

    /// The theme block `spec.output.theme` declares.
    #[must_use]
    pub fn theme(&self) -> Option<&ThemeConfig> {
        self.output.as_ref().and_then(|o| o.theme.as_ref())
    }
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

    /// Release channel to track: `stable` (releases GitHub marks latest) or
    /// `prerelease` (the newest release, prereleases included). Omitted,
    /// `stable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,

    /// Update policy for authored skills. Defaults to inheriting `policy`.
    #[serde(default, deserialize_with = "crate::config::null_as_default")]
    #[schemars(with = "Option<SkillUpdateConfig>")]
    pub skills: SkillUpdateConfig,
}

/// The release channel an `update` block that names none tracks.
pub const STABLE_UPDATE_CHANNEL: &str = "stable";

impl UpdateConfig {
    /// The release channel to track: the declared one, or
    /// [`STABLE_UPDATE_CHANNEL`] where the document omits it.
    #[must_use]
    pub fn channel_effective(&self) -> &str {
        self.channel.as_deref().unwrap_or(STABLE_UPDATE_CHANNEL)
    }
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
        legacy_output_keys: Vec::new(),
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
