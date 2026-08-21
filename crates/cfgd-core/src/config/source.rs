use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::origin::OriginSpec;
use super::profile_spec::{
    EncryptionConstraint, ManagedFileSpec, PackagesSpec, SecretSpec, SystemSettings,
};

// --- Multi-source config management ---

/// One entry of `spec.sources[]`: a remote config source this machine
/// subscribes to.
///
/// ```yaml
/// sources:
///   - name: team-baseline
///     origin:
///       type: Git
///       url: git@github.com:acme/cfgd-baseline.git
///     subscription:
///       acceptRecommended: true
///     sync:
///       interval: 1h
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSpec {
    /// Local name for this source, used in `cfgd source` commands and status output.
    pub name: String,
    /// Where the source's manifest is fetched from.
    pub origin: OriginSpec,
    /// What this machine accepts from the source and how it applies.
    #[serde(default)]
    pub subscription: SubscriptionSpec,
    /// How often and under what conditions the source is refreshed.
    #[serde(default)]
    pub sync: SourceSyncSpec,
}

/// `spec.sources[].subscription`: the subscriber's own policy toward one source.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionSpec {
    /// Which of the source's published profiles to compose against. Omitted
    /// composes every profile the source provides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Merge priority against other sources and the local profile; higher wins
    /// on conflicting leaf values. Default: `500`. Capped at
    /// `MAX_SOURCE_PRIORITY`.
    #[serde(
        default = "default_source_priority",
        deserialize_with = "deserialize_source_priority"
    )]
    pub priority: u32,
    /// Automatically accept the source's `recommended` policy tier without
    /// prompting. Default: `false`.
    #[serde(default)]
    pub accept_recommended: bool,
    /// Names from the source's `optional` policy tier to accept.
    #[serde(default)]
    pub opt_in: Vec<String>,
    /// Subscriber opt-in to run lifecycle scripts (profile-layer and
    /// source-delivered module bodies) from this source even when the source's
    /// `constraints.noScripts` would otherwise reject them. Default `false`:
    /// the source's own `noScripts` constraint governs.
    #[serde(default)]
    pub allow_scripts: bool,
    /// Subscriber-side demand that this source's HEAD commit carry a valid GPG
    /// or SSH signature.
    ///
    /// The trust anchor for the check. `constraints.requireSignedCommits` says
    /// the same thing, but it is read from the source's manifest INSIDE the
    /// cached clone, so whoever can write the cache can also clear it. This
    /// flag is read from the subscriber's own config, which the cache cannot
    /// reach.
    ///
    /// ORed with the manifest's flag, so it only ever ADDS strictness: a
    /// manifest `true` is never weakened by a subscriber `false`. Default
    /// `false`. `spec.security.allowUnsigned` still bypasses both.
    #[serde(default)]
    pub require_signed_commits: bool,
    /// Local values to deep-merge on top of what the source delivers, applied
    /// after composition.
    #[serde(default)]
    #[schemars(with = "serde_json::Value")]
    pub overrides: serde_yaml::Value,
    /// Items from the source's `recommended` tier to drop entirely rather than
    /// accept. A mapping under `packages`, `env`, `aliases`, and/or `modules`;
    /// any other top-level key is rejected as a typo.
    #[serde(default)]
    #[schemars(with = "serde_json::Value")]
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
            require_signed_commits: false,
            overrides: serde_yaml::Value::Null,
            reject: serde_yaml::Value::Null,
        }
    }
}

impl SourceSpec {
    /// Whether this source's HEAD signature must be verified, given what its
    /// manifest asks for.
    ///
    /// The ONE derivation of the effective flag, and what every enforcing site
    /// reads: `SourceManager::verify_commit_signature` on the load paths and
    /// `build_sync_tasks` for the daemon's per-source sync. Two sites deciding
    /// this separately is how one of them ends up trusting only the manifest,
    /// which is the file inside the cache an attacker who planted that cache
    /// wrote. Strictness only accumulates: either side asking for signatures is
    /// enough, and neither can turn the other off.
    pub fn requires_signed_commits(&self, manifest_requires: bool) -> bool {
        self.subscription.require_signed_commits || manifest_requires
    }
}

fn default_source_priority() -> u32 {
    500
}

/// Maximum user-settable source priority. The `required` tier ranks a source at
/// `priority + 1000` and the locked-tier sentinel is `u32::MAX`; capping here keeps
/// the required rank strictly below the locked sentinel and the addition overflow-free.
pub const MAX_SOURCE_PRIORITY: u32 = u32::MAX - 1001;

/// Validate a user-supplied source priority against [`MAX_SOURCE_PRIORITY`].
///
/// Returns the priority unchanged when in range, or the canonical
/// over-ceiling message so every entry point (YAML deserialization, the
/// interactive prompt, `source add --priority`, and `source priority`)
/// reports identical wording.
pub fn validate_source_priority(n: u32) -> std::result::Result<u32, String> {
    if n > MAX_SOURCE_PRIORITY {
        return Err(format!(
            "source priority {n} exceeds maximum {MAX_SOURCE_PRIORITY}"
        ));
    }
    Ok(n)
}

fn deserialize_source_priority<'de, D>(deserializer: D) -> std::result::Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let n = u32::deserialize(deserializer)?;
    validate_source_priority(n).map_err(serde::de::Error::custom)
}

/// `spec.sources[].sync`: how a source is refreshed and pinned.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSyncSpec {
    /// How often to fetch and re-resolve the source, as a duration string.
    /// Default: `1h`.
    #[serde(default = "default_sync_interval")]
    pub interval: String,
    /// Apply the source's changes automatically on refresh rather than only
    /// recording them. Default: `false`.
    #[serde(default)]
    pub auto_apply: bool,
    /// Pin to a specific git tag/branch/commit instead of tracking the origin's
    /// default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_version: Option<String>,
    /// Fail-closed marker. When `true`, a failure to load this source (fetch,
    /// manifest, signature, or an unresolvable `pinVersion`) is fatal — apply /
    /// plan / compose abort rather than silently dropping the source. Default
    /// `false` keeps the best-effort warn-and-continue behaviour for optional
    /// sources. Use it for security or team baselines that must always be
    /// composed in.
    #[serde(default)]
    pub required: bool,
}

impl Default for SourceSyncSpec {
    fn default() -> Self {
        Self {
            interval: default_sync_interval(),
            auto_apply: false,
            pin_version: None,
            required: false,
        }
    }
}

pub(super) fn default_sync_interval() -> String {
    "1h".to_string()
}

// --- ConfigSource manifest (published by team, lives in source repo as cfgd-source.yaml) ---

/// A `cfgd-source.yaml` manifest: what a config source publishes and the
/// policy tier each item is offered under. Lives in the source's own
/// repository, not in the subscriber's config.
///
/// ```yaml
/// apiVersion: cfgd.io/v1alpha1
/// kind: ConfigSource
/// metadata:
///   name: team-baseline
/// spec:
///   provides:
///     profiles: [base]
///   policy:
///     required:
///       packages:
///         apt: [git]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceDocument {
    /// API group/version, e.g. `cfgd.io/v1alpha1`.
    pub api_version: String,
    /// Document kind. Always `ConfigSource` for this file.
    pub kind: String,
    /// Identifying metadata for this source.
    pub metadata: ConfigSourceMetadata,
    /// What the source publishes and its policy tiers.
    pub spec: ConfigSourceSpec,
}

/// `metadata`: identifying information for a config source.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceMetadata {
    /// The source's published name.
    pub name: String,
    /// The source manifest's own version, shown to subscribers.
    #[serde(default)]
    pub version: Option<String>,
    /// A one-line human summary of what this source provides.
    #[serde(default)]
    pub description: Option<String>,
}

/// `spec`: the body of a config source manifest.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceSpec {
    /// Profiles and modules this source publishes.
    #[serde(default)]
    pub provides: ConfigSourceProvides,
    /// Policy tiers (required/recommended/optional/locked) and constraints
    /// this source enforces on subscribers.
    #[serde(default)]
    pub policy: ConfigSourcePolicy,
}

/// `spec.provides`: what a config source makes available to subscribers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceProvides {
    /// Flat list of published profile names. Superseded by `profile_details`
    /// when that list is non-empty.
    #[serde(default)]
    pub profiles: Vec<String>,
    /// Published profiles with descriptions, paths, and inheritance — richer
    /// than the flat `profiles` list.
    #[serde(default)]
    pub profile_details: Vec<ConfigSourceProfileEntry>,
    /// Maps a platform/distro tag (`macos`, `debian`, …) to the profile name
    /// to use on that platform.
    #[serde(default)]
    pub platform_profiles: HashMap<String, String>,
    /// Names of modules this source publishes.
    #[serde(default)]
    pub modules: Vec<String>,
}

/// Detailed profile entry in a ConfigSource manifest.
/// When present, provides richer info than the flat `profiles` list.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourceProfileEntry {
    /// Profile name.
    pub name: String,
    /// A one-line human summary of the profile.
    #[serde(default)]
    pub description: Option<String>,
    /// Path to the profile's manifest within the source repository, if not at
    /// the conventional location.
    #[serde(default)]
    pub path: Option<String>,
    /// Names of other published profiles this one inherits from.
    #[serde(default)]
    pub inherits: Vec<String>,
}

/// `spec.policy`: the four acceptance tiers a config source offers items
/// under, plus the constraints it enforces on every subscriber.
///
/// ```yaml
/// policy:
///   required:
///     packages:
///       apt: [git]
///   recommended:
///     modules: [nvim]
///   optional:
///     modules: [tmux]
///   locked:
///     env:
///       - name: COMPANY_PROXY
///         value: http://proxy.internal:3128
///   constraints:
///     noScripts: true
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSourcePolicy {
    /// Items every subscriber receives unconditionally.
    #[serde(default)]
    pub required: PolicyItems,
    /// Items a subscriber receives when `subscription.acceptRecommended` is set.
    #[serde(default)]
    pub recommended: PolicyItems,
    /// Items a subscriber must explicitly name in `subscription.optIn` to receive.
    #[serde(default)]
    pub optional: PolicyItems,
    /// Items every subscriber receives and cannot override locally.
    #[serde(default)]
    pub locked: PolicyItems,
    /// Restrictions this source imposes on how subscribers may compose it.
    #[serde(default)]
    pub constraints: SourceConstraints,
}

/// A single `NAME=VALUE` environment variable entry.
#[derive(Debug, Clone, Serialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    /// Variable name. Must be shell-safe and not a reserved `CFGD_*` name.
    pub name: String,
    /// Variable value.
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

/// A single shell alias entry.
#[derive(Debug, Clone, Serialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShellAlias {
    /// Alias name, as typed at the shell prompt.
    pub name: String,
    /// Command the alias expands to.
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

/// The content a config source offers under one policy tier
/// (`required`/`recommended`/`optional`/`locked`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyItems {
    /// Packages offered at this tier.
    #[serde(default)]
    pub packages: Option<PackagesSpec>,
    /// Files offered at this tier.
    #[serde(default)]
    pub files: Vec<ManagedFileSpec>,
    /// Environment variables offered at this tier.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Shell aliases offered at this tier.
    #[serde(default)]
    pub aliases: Vec<ShellAlias>,
    /// System configurator settings offered at this tier.
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]
    pub system: SystemSettings,
    /// Profile names this tier recommends composing in.
    #[serde(default)]
    pub profiles: Vec<String>,
    /// Module names offered at this tier.
    #[serde(default)]
    pub modules: Vec<String>,
    /// Secrets offered at this tier.
    #[serde(default)]
    pub secrets: Vec<SecretSpec>,
}

/// `spec.policy.constraints`: restrictions a config source imposes on every
/// subscriber, independent of which tier delivered an item.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceConstraints {
    /// Reject lifecycle scripts (profile-layer and module `run:` bodies) this
    /// source delivers, unless a subscriber opts in via
    /// `subscription.allowScripts`. Default: `true`.
    #[serde(default = "default_true")]
    pub no_scripts: bool,
    /// Reject `${secret:…}` references this source's delivered content
    /// resolves. Default: `true`.
    #[serde(default = "default_true")]
    pub no_secrets_read: bool,
    /// Glob patterns restricting which file targets this source may deploy to.
    /// Empty means no restriction.
    #[serde(default)]
    pub allowed_target_paths: Vec<String>,
    /// Allow this source to deliver `system:` configurator settings. Default: `false`.
    #[serde(default)]
    pub allow_system_changes: bool,
    /// Require that the HEAD commit in this source's git repo has a valid
    /// GPG or SSH signature. ORed with the subscriber's
    /// `subscription.requireSignedCommits`, so either side asking is enough.
    /// Subscribers can bypass both with `spec.security.allowUnsigned`.
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
    fn subscription_spec_parses_require_signed_commits() {
        let yaml = "priority: 100\nrequireSignedCommits: true\n";
        let spec: SubscriptionSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(spec.require_signed_commits);
        assert_eq!(spec.priority, 100);

        let round_tripped: SubscriptionSpec =
            serde_yaml::from_str(&serde_yaml::to_string(&spec).unwrap()).unwrap();
        assert!(round_tripped.require_signed_commits);
    }

    #[test]
    fn subscription_spec_require_signed_commits_defaults_false() {
        let yaml = "priority: 100\n";
        let spec: SubscriptionSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(!spec.require_signed_commits);
        assert!(!SubscriptionSpec::default().require_signed_commits);
    }

    #[test]
    fn requires_signed_commits_ors_the_subscriber_flag_with_the_manifests() {
        let yaml = r#"
name: team
origin:
  type: Git
  url: https://example.com/team.git
  branch: main
subscription:
  requireSignedCommits: true
"#;
        let subscriber_demands: SourceSpec = serde_yaml::from_str(yaml).unwrap();
        // A manifest inside a planted cache cannot clear the subscriber's demand.
        assert!(subscriber_demands.requires_signed_commits(false));
        assert!(subscriber_demands.requires_signed_commits(true));

        let mut silent = subscriber_demands.clone();
        silent.subscription.require_signed_commits = false;
        // A subscriber that asks for nothing still honours the manifest.
        assert!(!silent.requires_signed_commits(false));
        assert!(silent.requires_signed_commits(true));
    }

    #[test]
    fn sync_spec_parses_required_true() {
        let yaml = "interval: 30m\nrequired: true\n";
        let spec: SourceSyncSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(spec.required);
        assert_eq!(spec.interval, "30m");
    }

    #[test]
    fn sync_spec_required_defaults_false() {
        let yaml = "interval: 1h\n";
        let spec: SourceSyncSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(!spec.required);
        assert!(!SourceSyncSpec::default().required);
    }

    #[test]
    fn sync_spec_rejects_unknown_field_alongside_required() {
        let yaml = "required: true\nbogusField: 1\n";
        let err = serde_yaml::from_str::<SourceSyncSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogusField");
        assert!(format!("{err}").contains("unknown field"));
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

    #[test]
    fn subscription_priority_rejects_over_cap() {
        // u32::MAX (4294967295) is above MAX_SOURCE_PRIORITY — must be rejected.
        let yaml = "priority: 4294967295\n";
        let err = serde_yaml::from_str::<SubscriptionSpec>(yaml)
            .expect_err("priority at u32::MAX must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("exceeds maximum"),
            "error should mention 'exceeds maximum': {msg}"
        );
    }

    #[test]
    fn subscription_priority_accepts_at_cap() {
        // MAX_SOURCE_PRIORITY itself must be accepted.
        let yaml = format!("priority: {}\n", MAX_SOURCE_PRIORITY);
        let spec: SubscriptionSpec =
            serde_yaml::from_str(&yaml).expect("priority at MAX_SOURCE_PRIORITY must be accepted");
        assert_eq!(spec.priority, MAX_SOURCE_PRIORITY);
    }

    #[test]
    fn subscription_priority_default_unaffected_by_cap() {
        // Omitting `priority` falls back to the default (500) via `default_source_priority`,
        // not through `deserialize_source_priority`, so the default path must still work.
        let yaml = "profile: dev\n";
        let spec: SubscriptionSpec =
            serde_yaml::from_str(yaml).expect("default priority path must not be broken");
        assert_eq!(spec.priority, 500);
    }
}
