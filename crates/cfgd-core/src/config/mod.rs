// Config types, profile resolution, and multi-source prep

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::deep_merge_yaml;
use crate::errors::{ConfigError, Result};
use crate::union_extend;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfig {
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    #[serde(default = "default_ai_model")]
    pub model: String,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: default_ai_provider(),
            model: default_ai_model(),
            api_key_env: default_api_key_env(),
        }
    }
}

fn default_ai_provider() -> String {
    "claude".into()
}
fn default_ai_model() -> String {
    "claude-sonnet-4-6".into()
}
fn default_api_key_env() -> String {
    "ANTHROPIC_API_KEY".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityConfig {
    /// Allow unsigned source content even when the source requires signed commits.
    /// Intended for development/testing environments.
    #[serde(default)]
    pub allow_unsigned: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModulesConfig {
    /// Module registries — git repos containing modules in a prescribed directory structure.
    #[serde(default)]
    pub registries: Vec<ModuleRegistryEntry>,

    /// Module security settings.
    #[serde(default)]
    pub security: Option<ModuleSecurityConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSecurityConfig {
    /// Require GPG/SSH signatures on all remote module tags.
    /// When true, unsigned modules are rejected unless `--allow-unsigned` is passed.
    #[serde(default)]
    pub require_signatures: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeConfig {
    #[serde(default = "default_theme_name")]
    pub name: String,
    #[serde(default, skip_serializing_if = "ThemeOverrides::is_empty")]
    pub overrides: ThemeOverrides,
}

fn default_theme_name() -> String {
    "default".to_string()
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: default_theme_name(),
            overrides: ThemeOverrides::default(),
        }
    }
}

// Accept both `theme: "dracula"` (string) and `theme: { name: dracula, overrides: ... }` (struct)
impl<'de> serde::Deserialize<'de> for ThemeConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct ThemeVisitor;
        impl<'de> de::Visitor<'de> for ThemeVisitor {
            type Value = ThemeConfig;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a theme name string or a theme config mapping")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<ThemeConfig, E> {
                Ok(ThemeConfig {
                    name: v.to_string(),
                    overrides: ThemeOverrides::default(),
                })
            }
            fn visit_map<M: de::MapAccess<'de>>(
                self,
                map: M,
            ) -> std::result::Result<ThemeConfig, M::Error> {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Inner {
                    #[serde(default = "default_theme_name")]
                    name: String,
                    #[serde(default)]
                    overrides: ThemeOverrides,
                }
                let inner = Inner::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(ThemeConfig {
                    name: inner.name,
                    overrides: inner.overrides,
                })
            }
        }
        deserializer.deserialize_any(ThemeVisitor)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeOverrides {
    pub success: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub info: Option<String>,
    pub muted: Option<String>,
    pub header: Option<String>,
    pub subheader: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub diff_add: Option<String>,
    pub diff_remove: Option<String>,
    pub diff_context: Option<String>,
    pub icon_success: Option<String>,
    pub icon_warning: Option<String>,
    pub icon_error: Option<String>,
    pub icon_info: Option<String>,
    pub icon_pending: Option<String>,
    pub icon_arrow: Option<String>,
}

impl ThemeOverrides {
    pub fn is_empty(&self) -> bool {
        self.success.is_none()
            && self.warning.is_none()
            && self.error.is_none()
            && self.info.is_none()
            && self.muted.is_none()
            && self.header.is_none()
            && self.subheader.is_none()
            && self.key.is_none()
            && self.value.is_none()
            && self.diff_add.is_none()
            && self.diff_remove.is_none()
            && self.diff_context.is_none()
            && self.icon_success.is_none()
            && self.icon_warning.is_none()
            && self.icon_error.is_none()
            && self.icon_info.is_none()
            && self.icon_pending.is_none()
            && self.icon_arrow.is_none()
    }
}

// Custom deserialization: origin can be a single object or an array
// Internally always Vec<OriginSpec> with primary at index 0
impl ConfigSpec {
    pub fn primary_origin(&self) -> Option<&OriginSpec> {
        self.origin.first()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginSpec {
    #[serde(rename = "type")]
    pub origin_type: OriginType,
    pub url: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    pub auth: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OriginType {
    Git,
    Server,
}

fn default_branch() -> String {
    "master".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub reconcile: Option<ReconcileConfig>,
    #[serde(default)]
    pub sync: Option<SyncConfig>,
    #[serde(default)]
    pub notify: Option<NotifyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileConfig {
    #[serde(default = "default_reconcile_interval")]
    pub interval: String,
    #[serde(default)]
    pub on_change: bool,
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default)]
    pub policy: Option<AutoApplyPolicyConfig>,
    /// Policy for daemon auto-reconciliation of detected drift.
    /// `Auto` = silently apply (must opt-in), `NotifyOnly` = notify but don't
    /// apply (safe default), `Prompt` = future interactive approval.
    #[serde(default)]
    pub drift_policy: DriftPolicy,
    /// Per-module or per-profile reconcile overrides (kustomize-style patches).
    /// Each patch targets a specific Module or Profile by name and overrides
    /// individual reconcile fields. Precedence: Module patch > Profile patch > global.
    #[serde(default)]
    pub patches: Vec<ReconcilePatch>,
}

/// A kustomize-style reconcile patch targeting a specific module or profile.
/// When `name` is omitted, the patch applies to all entities of the given kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcilePatch {
    pub kind: ReconcilePatchKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_apply: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_policy: Option<DriftPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconcilePatchKind {
    Module,
    Profile,
}

/// Daemon drift reconciliation policy. PascalCase values match K8s enum conventions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftPolicy {
    /// Apply drift corrections automatically (current behavior, now opt-in).
    Auto,
    /// Notify and record drift, but do not apply. User must run `cfgd apply`.
    #[default]
    NotifyOnly,
    /// Future: notify with actionable prompt.
    Prompt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoApplyPolicyConfig {
    #[serde(default = "default_policy_notify")]
    pub new_recommended: PolicyAction,
    #[serde(default = "default_policy_ignore")]
    pub new_optional: PolicyAction,
    #[serde(default = "default_policy_notify")]
    pub locked_conflict: PolicyAction,
}

impl Default for AutoApplyPolicyConfig {
    fn default() -> Self {
        Self {
            new_recommended: PolicyAction::Notify,
            new_optional: PolicyAction::Ignore,
            locked_conflict: PolicyAction::Notify,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyAction {
    Notify,
    Accept,
    Reject,
    Ignore,
}

fn default_policy_notify() -> PolicyAction {
    PolicyAction::Notify
}

fn default_policy_ignore() -> PolicyAction {
    PolicyAction::Ignore
}

fn default_reconcile_interval() -> String {
    "5m".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    #[serde(default)]
    pub auto_push: bool,
    #[serde(default)]
    pub auto_pull: bool,
    #[serde(default = "default_sync_interval")]
    pub interval: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyConfig {
    #[serde(default)]
    pub drift: bool,
    #[serde(default)]
    pub method: NotifyMethod,
    pub webhook_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum NotifyMethod {
    #[default]
    Desktop,
    Stdout,
    Webhook,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsConfig {
    #[serde(default = "default_secrets_backend")]
    pub backend: String,
    pub sops: Option<SopsConfig>,
    #[serde(default)]
    pub integrations: Vec<SecretIntegration>,
}

fn default_secrets_backend() -> String {
    "sops".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SopsConfig {
    pub age_key: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretIntegration {
    pub name: String,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

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

fn default_sync_interval() -> String {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShellAlias {
    pub name: String,
    pub command: String,
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
}

impl Default for SourceConstraints {
    fn default() -> Self {
        Self {
            no_scripts: true,
            no_secrets_read: true,
            allowed_target_paths: Vec::new(),
            allow_system_changes: false,
            require_signed_commits: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Parse a ConfigSource manifest from YAML content.
pub fn parse_config_source(contents: &str) -> Result<ConfigSourceDocument> {
    let doc: ConfigSourceDocument = serde_yaml::from_str(contents).map_err(ConfigError::from)?;

    if doc.kind != "ConfigSource" {
        return Err(ConfigError::Invalid {
            message: format!("expected kind 'ConfigSource', got '{}'", doc.kind),
        }
        .into());
    }

    Ok(doc)
}

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
    pub scripts: Option<ModuleScriptSpec>,
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleScriptSpec {
    #[serde(default)]
    pub post_apply: Vec<String>,
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
    let doc: ModuleDocument = serde_yaml::from_str(contents).map_err(ConfigError::from)?;

    if doc.kind != "Module" {
        return Err(ConfigError::Invalid {
            message: format!("expected kind 'Module', got '{}'", doc.kind),
        }
        .into());
    }

    Ok(doc)
}

// --- Profile ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ProfileMetadata,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSpec {
    #[serde(default)]
    pub inherits: Vec<String>,

    #[serde(default)]
    pub modules: Vec<String>,

    #[serde(default)]
    pub env: Vec<EnvVar>,

    #[serde(default)]
    pub aliases: Vec<ShellAlias>,

    #[serde(default)]
    pub packages: Option<PackagesSpec>,

    #[serde(default)]
    pub files: Option<FilesSpec>,

    #[serde(default)]
    pub system: HashMap<String, serde_yaml::Value>,

    #[serde(default)]
    pub secrets: Vec<SecretSpec>,

    #[serde(default)]
    pub scripts: Option<ScriptSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagesSpec {
    #[serde(default)]
    pub brew: Option<BrewSpec>,
    #[serde(default)]
    pub apt: Option<AptSpec>,
    #[serde(default)]
    pub cargo: Option<CargoSpec>,
    #[serde(default)]
    pub npm: Option<NpmSpec>,
    #[serde(default)]
    pub pipx: Vec<String>,
    #[serde(default)]
    pub dnf: Vec<String>,
    #[serde(default)]
    pub apk: Vec<String>,
    #[serde(default)]
    pub pacman: Vec<String>,
    #[serde(default)]
    pub zypper: Vec<String>,
    #[serde(default)]
    pub yum: Vec<String>,
    #[serde(default)]
    pub pkg: Vec<String>,
    #[serde(default)]
    pub snap: Option<SnapSpec>,
    #[serde(default)]
    pub flatpak: Option<FlatpakSpec>,
    #[serde(default)]
    pub nix: Vec<String>,
    #[serde(default)]
    pub go: Vec<String>,
    #[serde(default)]
    pub custom: Vec<CustomManagerSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrewSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub taps: Vec<String>,
    #[serde(default)]
    pub formulae: Vec<String>,
    #[serde(default)]
    pub casks: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AptSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpmSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub global: Vec<String>,
}

/// Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`)
/// and object form (`cargo: { file: Cargo.toml, packages: [...] }`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CargoSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

impl<'de> Deserialize<'de> for CargoSpec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CargoSpecFull {
            #[serde(default)]
            file: Option<String>,
            #[serde(default)]
            packages: Vec<String>,
        }

        // Try to deserialize as either a list of strings or a map with file/packages
        struct CargoSpecVisitor;

        impl<'de> de::Visitor<'de> for CargoSpecVisitor {
            type Value = CargoSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a list of package names or a map with file/packages keys")
            }

            fn visit_seq<A>(self, mut seq: A) -> std::result::Result<CargoSpec, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut packages = Vec::new();
                while let Some(item) = seq.next_element::<String>()? {
                    packages.push(item);
                }
                Ok(CargoSpec {
                    file: None,
                    packages,
                })
            }

            fn visit_map<M>(self, map: M) -> std::result::Result<CargoSpec, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let full = CargoSpecFull::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(CargoSpec {
                    file: full.file,
                    packages: full.packages,
                })
            }
        }

        deserializer.deserialize_any(CargoSpecVisitor)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapSpec {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub classic: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatpakSpec {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomManagerSpec {
    pub name: String,
    pub check: String,
    pub list_installed: String,
    pub install: String,
    pub uninstall: String,
    #[serde(default)]
    pub update: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesSpec {
    #[serde(default)]
    pub managed: Vec<ManagedFileSpec>,
    #[serde(default)]
    pub permissions: HashMap<String, String>,
}

/// File deployment strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FileStrategy {
    /// Create a symbolic link from target to source (default).
    #[default]
    Symlink,
    /// Copy source content to target.
    Copy,
    /// Render a Tera template and write the output (auto-selected for .tera files).
    Template,
    /// Create a hard link from target to source.
    Hardlink,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedFileSpec {
    pub source: String,
    pub target: PathBuf,
    /// Per-file deployment strategy override. If None, uses the global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<FileStrategy>,
    /// When true, the source file is local-only: auto-added to .gitignore,
    /// silently skipped on machines where it doesn't exist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Which source this file came from (None = local config).
    /// Used by the template sandbox to restrict variable access.
    #[serde(skip)]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretSpec {
    pub source: String,
    pub target: PathBuf,
    pub template: Option<String>,
    pub backend: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptSpec {
    #[serde(default)]
    pub pre_reconcile: Vec<PathBuf>,
    #[serde(default)]
    pub post_reconcile: Vec<PathBuf>,
}

// --- Profile Resolution ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum LayerPolicy {
    Local,
    Required,
    Recommended,
    Optional,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileLayer {
    pub source: String,
    pub profile_name: String,
    pub priority: u32,
    pub policy: LayerPolicy,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedProfile {
    pub layers: Vec<ProfileLayer>,
    pub merged: MergedProfile,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MergedProfile {
    pub modules: Vec<String>,
    pub env: Vec<EnvVar>,
    pub aliases: Vec<ShellAlias>,
    pub packages: PackagesSpec,
    pub files: FilesSpec,
    pub system: HashMap<String, serde_yaml::Value>,
    pub secrets: Vec<SecretSpec>,
    pub scripts: ScriptSpec,
}

/// Load and parse the root cfgd.yaml config file
pub fn load_config(path: &Path) -> Result<CfgdConfig> {
    if !path.exists() {
        return Err(ConfigError::NotFound {
            path: path.to_path_buf(),
        }
        .into());
    }

    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Invalid {
        message: format!("failed to read {}: {}", path.display(), e),
    })?;

    parse_config(&contents, path)
}

/// Parse config from string, supporting both YAML and TOML based on file extension
pub fn parse_config(contents: &str, path: &Path) -> Result<CfgdConfig> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("yaml");

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
        },
    })
}

// Internal raw types for flexible origin deserialization
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCfgdConfig {
    api_version: String,
    kind: String,
    metadata: ConfigMetadata,
    spec: RawConfigSpec,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
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
        message: format!("failed to read {}: {}", path.display(), e),
    })?;

    let doc: ProfileDocument = serde_yaml::from_str(&contents).map_err(ConfigError::from)?;
    Ok(doc)
}

/// Find a profile file by name, checking `.yaml` then `.yml` extensions.
fn find_profile_path(profiles_dir: &Path, name: &str) -> PathBuf {
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

/// Resolve a profile by loading it and its full inheritance chain, then merging.
pub fn resolve_profile(profile_name: &str, profiles_dir: &Path) -> Result<ResolvedProfile> {
    let resolution_order = resolve_inheritance_order(profile_name, profiles_dir, &mut vec![])?;

    let mut layers = Vec::new();
    for name in &resolution_order {
        let path = find_profile_path(profiles_dir, name);
        let doc = load_profile(&path)?;
        layers.push(ProfileLayer {
            source: "local".to_string(),
            profile_name: name.clone(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: doc.spec,
        });
    }

    let merged = merge_layers(&layers);

    Ok(ResolvedProfile { layers, merged })
}

/// Recursively resolve the inheritance order (depth-first, left-to-right).
/// Returns profiles in resolution order: earliest ancestor first, active profile last.
fn resolve_inheritance_order(
    profile_name: &str,
    profiles_dir: &Path,
    visited: &mut Vec<String>,
) -> Result<Vec<String>> {
    if visited.contains(&profile_name.to_string()) {
        let mut chain = visited.clone();
        chain.push(profile_name.to_string());
        return Err(ConfigError::CircularInheritance { chain }.into());
    }

    visited.push(profile_name.to_string());

    let path = find_profile_path(profiles_dir, profile_name);
    let doc = load_profile(&path)?;

    let mut order = Vec::new();
    for parent in &doc.spec.inherits {
        let parent_order = resolve_inheritance_order(parent, profiles_dir, visited)?;
        for name in parent_order {
            if !order.contains(&name) {
                order.push(name);
            }
        }
    }

    order.push(profile_name.to_string());
    visited.pop();

    Ok(order)
}

/// Merge profile layers according to merge rules:
/// - packages: union
/// - files: overlay (later overrides earlier for same target)
/// - env: override (later replaces earlier for same name)
/// - secrets: append (deduplicated by target)
/// - scripts: append in order
/// - system: deep merge (later overrides at leaf level)
fn merge_layers(layers: &[ProfileLayer]) -> MergedProfile {
    let mut merged = MergedProfile::default();

    for layer in layers {
        let spec = &layer.spec;

        // Modules: union
        union_extend(&mut merged.modules, &spec.modules);

        // Env: later layer overrides earlier by name
        crate::merge_env(&mut merged.env, &spec.env);

        // Aliases: later layer overrides earlier by name
        crate::merge_aliases(&mut merged.aliases, &spec.aliases);

        // Packages: union
        if let Some(ref pkgs) = spec.packages {
            if let Some(ref brew) = pkgs.brew {
                let merged_brew = merged.packages.brew.get_or_insert_with(BrewSpec::default);
                if brew.file.is_some() {
                    merged_brew.file = brew.file.clone();
                }
                union_extend(&mut merged_brew.taps, &brew.taps);
                union_extend(&mut merged_brew.formulae, &brew.formulae);
                union_extend(&mut merged_brew.casks, &brew.casks);
            }
            if let Some(ref apt) = pkgs.apt {
                let apt_spec = merged.packages.apt.get_or_insert_with(AptSpec::default);
                if apt.file.is_some() {
                    apt_spec.file = apt.file.clone();
                }
                union_extend(&mut apt_spec.packages, &apt.packages);
            }
            if let Some(ref cargo) = pkgs.cargo {
                let merged_cargo = merged.packages.cargo.get_or_insert_with(CargoSpec::default);
                // Later file wins
                if cargo.file.is_some() {
                    merged_cargo.file = cargo.file.clone();
                }
                union_extend(&mut merged_cargo.packages, &cargo.packages);
            }
            if let Some(ref npm) = pkgs.npm {
                let npm_spec = merged.packages.npm.get_or_insert_with(NpmSpec::default);
                if npm.file.is_some() {
                    npm_spec.file = npm.file.clone();
                }
                union_extend(&mut npm_spec.global, &npm.global);
            }
            union_extend(&mut merged.packages.pipx, &pkgs.pipx);
            union_extend(&mut merged.packages.dnf, &pkgs.dnf);
            union_extend(&mut merged.packages.apk, &pkgs.apk);
            union_extend(&mut merged.packages.pacman, &pkgs.pacman);
            union_extend(&mut merged.packages.zypper, &pkgs.zypper);
            union_extend(&mut merged.packages.yum, &pkgs.yum);
            union_extend(&mut merged.packages.pkg, &pkgs.pkg);
            if let Some(ref snap) = pkgs.snap {
                let merged_snap = merged.packages.snap.get_or_insert_with(SnapSpec::default);
                union_extend(&mut merged_snap.packages, &snap.packages);
                union_extend(&mut merged_snap.classic, &snap.classic);
            }
            if let Some(ref flatpak) = pkgs.flatpak {
                let merged_flatpak = merged
                    .packages
                    .flatpak
                    .get_or_insert_with(FlatpakSpec::default);
                union_extend(&mut merged_flatpak.packages, &flatpak.packages);
                if flatpak.remote.is_some() {
                    merged_flatpak.remote = flatpak.remote.clone();
                }
            }
            union_extend(&mut merged.packages.nix, &pkgs.nix);
            union_extend(&mut merged.packages.go, &pkgs.go);
            // Custom managers: merge by name, union packages
            for custom in &pkgs.custom {
                if let Some(existing) = merged
                    .packages
                    .custom
                    .iter_mut()
                    .find(|c| c.name == custom.name)
                {
                    // Later layer's commands override, packages merge
                    existing.check = custom.check.clone();
                    existing.list_installed = custom.list_installed.clone();
                    existing.install = custom.install.clone();
                    existing.uninstall = custom.uninstall.clone();
                    if custom.update.is_some() {
                        existing.update = custom.update.clone();
                    }
                    union_extend(&mut existing.packages, &custom.packages);
                } else {
                    merged.packages.custom.push(custom.clone());
                }
            }
        }

        // Files: overlay (later layer overrides earlier for same target)
        if let Some(ref files) = spec.files {
            for managed in &files.managed {
                if let Some(existing) = merged
                    .files
                    .managed
                    .iter_mut()
                    .find(|m| m.target == managed.target)
                {
                    existing.source = managed.source.clone();
                } else {
                    merged.files.managed.push(managed.clone());
                }
            }
            for (path, mode) in &files.permissions {
                merged.files.permissions.insert(path.clone(), mode.clone());
            }
        }

        // System: deep merge at leaf level
        for (key, value) in &spec.system {
            deep_merge_yaml(
                merged
                    .system
                    .entry(key.clone())
                    .or_insert(serde_yaml::Value::Null),
                value,
            );
        }

        // Secrets: append, deduplicate by target (later layer overrides)
        for secret in &spec.secrets {
            if let Some(existing) = merged
                .secrets
                .iter_mut()
                .find(|s| s.target == secret.target)
            {
                *existing = secret.clone();
            } else {
                merged.secrets.push(secret.clone());
            }
        }

        // Scripts: append in order
        if let Some(ref scripts) = spec.scripts {
            merged
                .scripts
                .pre_reconcile
                .extend(scripts.pre_reconcile.clone());
            merged
                .scripts
                .post_reconcile
                .extend(scripts.post_reconcile.clone());
        }
    }

    merged
}

/// Interpolate env vars in a string value.
/// Replaces `${name}` with the corresponding env var value.
pub fn interpolate_env(input: &str, env: &[EnvVar]) -> String {
    let mut result = input.to_string();
    for ev in env {
        let placeholder = format!("${{{}}}", ev.name);
        result = result.replace(&placeholder, &ev.value);
    }
    result
}

/// Get the list of desired packages for a specific package manager from a merged profile.
pub fn desired_packages_for(manager_name: &str, profile: &MergedProfile) -> Vec<String> {
    desired_packages_for_spec(manager_name, &profile.packages)
}

fn desired_packages_for_spec(manager_name: &str, packages: &PackagesSpec) -> Vec<String> {
    match manager_name {
        "brew" => packages
            .brew
            .as_ref()
            .map(|b| b.formulae.clone())
            .unwrap_or_default(),
        "brew-tap" => packages
            .brew
            .as_ref()
            .map(|b| b.taps.clone())
            .unwrap_or_default(),
        "brew-cask" => packages
            .brew
            .as_ref()
            .map(|b| b.casks.clone())
            .unwrap_or_default(),
        "apt" => packages
            .apt
            .as_ref()
            .map(|a| a.packages.clone())
            .unwrap_or_default(),
        "cargo" => packages
            .cargo
            .as_ref()
            .map(|c| c.packages.clone())
            .unwrap_or_default(),
        "npm" => packages
            .npm
            .as_ref()
            .map(|n| n.global.clone())
            .unwrap_or_default(),
        "pipx" => packages.pipx.clone(),
        "dnf" => packages.dnf.clone(),
        "apk" => packages.apk.clone(),
        "pacman" => packages.pacman.clone(),
        "zypper" => packages.zypper.clone(),
        "yum" => packages.yum.clone(),
        "pkg" => packages.pkg.clone(),
        "snap" => packages
            .snap
            .as_ref()
            .map(|s| {
                let mut all = s.packages.clone();
                for p in &s.classic {
                    if !all.contains(p) {
                        all.push(p.clone());
                    }
                }
                all
            })
            .unwrap_or_default(),
        "flatpak" => packages
            .flatpak
            .as_ref()
            .map(|f| f.packages.clone())
            .unwrap_or_default(),
        "nix" => packages.nix.clone(),
        "go" => packages.go.clone(),
        _ => {
            // Check custom managers
            for custom in &packages.custom {
                if custom.name == manager_name {
                    return custom.packages.clone();
                }
            }
            Vec::new()
        }
    }
}

// --- Platform Detection ---

/// Detected platform information for matching source `platform-profiles`.
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    pub os: String,
    pub distro: Option<String>,
    pub distro_version: Option<String>,
}

/// Detect the current platform OS, distro, and version.
pub fn detect_platform() -> PlatformInfo {
    let os = match std::env::consts::OS {
        "macos" => "macos".to_string(),
        other => other.to_string(),
    };
    let (distro, version) = if os == "linux" {
        parse_os_release_file()
    } else {
        (None, None)
    };
    PlatformInfo {
        os,
        distro,
        distro_version: version,
    }
}

/// Match platform info against a source's `platform-profiles` map.
/// Tries exact distro match first, then OS-level match.
pub fn match_platform_profile(
    platform: &PlatformInfo,
    platform_profiles: &HashMap<String, String>,
) -> Option<String> {
    // Try exact distro match first (e.g., "debian", "ubuntu", "fedora")
    if let Some(ref distro) = platform.distro
        && let Some(path) = platform_profiles.get(distro)
    {
        return Some(path.clone());
    }
    // Fall back to OS-level match (e.g., "macos", "linux")
    if let Some(path) = platform_profiles.get(&platform.os) {
        return Some(path.clone());
    }
    None
}

fn parse_os_release_file() -> (Option<String>, Option<String>) {
    parse_os_release_content(&std::fs::read_to_string("/etc/os-release").unwrap_or_default())
}

fn parse_os_release_content(content: &str) -> (Option<String>, Option<String>) {
    let mut id = None;
    let mut version_id = None;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("ID=") {
            id = Some(val.trim_matches('"').to_lowercase());
        } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
            version_id = Some(val.trim_matches('"').to_string());
        }
    }
    (id, version_id)
}

/// Get the list of profile names from a ConfigSource manifest.
/// Prefers `profile_details` if populated, falls back to `profiles`.
pub fn source_profile_names(provides: &ConfigSourceProvides) -> Vec<String> {
    if !provides.profile_details.is_empty() {
        provides
            .profile_details
            .iter()
            .map(|p| p.name.clone())
            .collect()
    } else {
        provides.profiles.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config_yaml() -> &'static str {
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
  origin:
    type: Git
    url: https://github.com/test/repo.git
    branch: master
"#
    }

    fn sample_config_no_origin() -> &'static str {
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
"#
    }

    fn sample_profile_yaml() -> &'static str {
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  env:
    - name: editor
      value: vim
    - name: shell
      value: /bin/zsh
  packages:
    brew:
      formulae:
        - ripgrep
        - fd
    cargo:
      - bat
"#
    }

    #[test]
    fn parse_yaml_config() {
        let config = parse_config(sample_config_yaml(), Path::new("cfgd.yaml")).unwrap();
        assert_eq!(config.metadata.name, "test-config");
        assert_eq!(config.spec.profile.as_deref(), Some("default"));
        assert_eq!(config.spec.origin.len(), 1);
        assert_eq!(
            config.spec.origin[0].url,
            "https://github.com/test/repo.git"
        );
        assert_eq!(config.spec.origin[0].branch, "master");
    }

    #[test]
    fn parse_config_without_origin() {
        let config = parse_config(sample_config_no_origin(), Path::new("cfgd.yaml")).unwrap();
        assert!(config.spec.origin.is_empty());
        assert!(config.spec.sources.is_empty());
    }

    #[test]
    fn parse_profile_yaml() {
        let doc: ProfileDocument = serde_yaml::from_str(sample_profile_yaml()).unwrap();
        assert_eq!(doc.metadata.name, "base");
        assert_eq!(doc.spec.env.len(), 2);
        let pkgs = doc.spec.packages.as_ref().unwrap();
        let brew = pkgs.brew.as_ref().unwrap();
        assert_eq!(brew.formulae, vec!["ripgrep", "fd"]);
        assert_eq!(pkgs.cargo.as_ref().unwrap().packages, vec!["bat"]);
    }

    #[test]
    fn merge_env_override() {
        let layer1 = ProfileLayer {
            source: "local".into(),
            profile_name: "base".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                env: vec![
                    EnvVar {
                        name: "editor".into(),
                        value: "vim".into(),
                    },
                    EnvVar {
                        name: "shell".into(),
                        value: "/bin/bash".into(),
                    },
                ],
                ..Default::default()
            },
        };
        let layer2 = ProfileLayer {
            source: "local".into(),
            profile_name: "work".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                env: vec![EnvVar {
                    name: "editor".into(),
                    value: "code".into(),
                }],
                ..Default::default()
            },
        };

        let merged = merge_layers(&[layer1, layer2]);
        assert_eq!(
            merged
                .env
                .iter()
                .find(|e| e.name == "editor")
                .map(|e| &e.value),
            Some(&"code".to_string())
        );
        assert_eq!(
            merged
                .env
                .iter()
                .find(|e| e.name == "shell")
                .map(|e| &e.value),
            Some(&"/bin/bash".to_string())
        );
    }

    #[test]
    fn merge_packages_union() {
        let layer1 = ProfileLayer {
            source: "local".into(),
            profile_name: "base".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                packages: Some(PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into()],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        let layer2 = ProfileLayer {
            source: "local".into(),
            profile_name: "work".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                packages: Some(PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into(), "exa".into()],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        };

        let merged = merge_layers(&[layer1, layer2]);
        assert_eq!(
            merged.packages.cargo.as_ref().unwrap().packages,
            vec!["bat", "exa"]
        );
    }

    #[test]
    fn merge_files_overlay() {
        let layer1 = ProfileLayer {
            source: "local".into(),
            profile_name: "base".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                files: Some(FilesSpec {
                    managed: vec![ManagedFileSpec {
                        source: "base/.zshrc".into(),
                        target: PathBuf::from("/home/user/.zshrc"),
                        strategy: None,
                        private: false,
                        origin: None,
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        let layer2 = ProfileLayer {
            source: "local".into(),
            profile_name: "work".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                files: Some(FilesSpec {
                    managed: vec![ManagedFileSpec {
                        source: "work/.zshrc".into(),
                        target: PathBuf::from("/home/user/.zshrc"),
                        strategy: None,
                        private: false,
                        origin: None,
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        };

        let merged = merge_layers(&[layer1, layer2]);
        assert_eq!(merged.files.managed.len(), 1);
        assert_eq!(merged.files.managed[0].source, "work/.zshrc");
    }

    #[test]
    fn deep_merge_yaml_maps() {
        let mut base = serde_yaml::from_str::<serde_yaml::Value>(
            r#"
            domain1:
              key1: value1
              key2: value2
            "#,
        )
        .unwrap();

        let overlay = serde_yaml::from_str::<serde_yaml::Value>(
            r#"
            domain1:
              key2: overridden
              key3: value3
            "#,
        )
        .unwrap();

        deep_merge_yaml(&mut base, &overlay);

        let map = base.as_mapping().unwrap();
        let domain = map
            .get(&serde_yaml::Value::String("domain1".into()))
            .unwrap()
            .as_mapping()
            .unwrap();
        assert_eq!(
            domain.get(&serde_yaml::Value::String("key1".into())),
            Some(&serde_yaml::Value::String("value1".into()))
        );
        assert_eq!(
            domain.get(&serde_yaml::Value::String("key2".into())),
            Some(&serde_yaml::Value::String("overridden".into()))
        );
        assert_eq!(
            domain.get(&serde_yaml::Value::String("key3".into())),
            Some(&serde_yaml::Value::String("value3".into()))
        );
    }

    #[test]
    fn interpolate_env_replaces_placeholders() {
        let env = vec![
            EnvVar {
                name: "name".into(),
                value: "cfgd".into(),
            },
            EnvVar {
                name: "version".into(),
                value: "42".into(),
            },
        ];

        let result = interpolate_env("Hello ${name} v${version}!", &env);
        assert_eq!(result, "Hello cfgd v42!");
    }

    #[test]
    fn interpolate_env_no_match() {
        let env: Vec<EnvVar> = vec![];
        let result = interpolate_env("no ${vars} here", &env);
        assert_eq!(result, "no ${vars} here");
    }

    #[test]
    fn profile_resolution_with_filesystem() {
        let dir = tempfile::tempdir().unwrap();

        // Create base profile
        std::fs::write(
            dir.path().join("base.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  env:
    - name: editor
      value: vim
  packages:
    cargo:
      - bat
"#,
        )
        .unwrap();

        // Create work profile inheriting base
        std::fs::write(
            dir.path().join("work.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - base
  env:
    - name: editor
      value: code
  packages:
    cargo:
      - exa
"#,
        )
        .unwrap();

        let resolved = resolve_profile("work", dir.path()).unwrap();

        assert_eq!(resolved.layers.len(), 2);
        assert_eq!(resolved.layers[0].profile_name, "base");
        assert_eq!(resolved.layers[1].profile_name, "work");

        // editor should be overridden by work
        assert_eq!(
            resolved
                .merged
                .env
                .iter()
                .find(|e| e.name == "editor")
                .map(|e| &e.value),
            Some(&"code".to_string())
        );
        // packages should be unioned
        assert_eq!(
            resolved.merged.packages.cargo.as_ref().unwrap().packages,
            vec!["bat", "exa"]
        );
    }

    #[test]
    fn circular_inheritance_detected() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(
            dir.path().join("a.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: a
spec:
  inherits:
    - b
"#,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("b.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: b
spec:
  inherits:
    - a
"#,
        )
        .unwrap();

        let result = resolve_profile("a", dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("circular"));
    }

    #[test]
    fn config_not_found_error() {
        let result = load_config(Path::new("/nonexistent/cfgd.yaml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn parse_config_source_manifest() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme-corp-dev
  version: "2.1.0"
  description: "ACME Corp developer environment"
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
  policy:
    required:
      packages:
        brew:
          formulae:
            - git-secrets
            - pre-commit
    recommended:
      packages:
        brew:
          formulae:
            - k9s
      env:
        - name: EDITOR
          value: "code --wait"
    locked:
      files:
        - source: "security/policy.yaml"
          target: "~/.config/company/security-policy.yaml"
    constraints:
      noScripts: true
      noSecretsRead: true
      allowedTargetPaths:
        - "~/.config/acme/"
        - "~/.eslintrc*"
"#;
        let doc = parse_config_source(yaml).unwrap();
        assert_eq!(doc.metadata.name, "acme-corp-dev");
        assert_eq!(doc.metadata.version.as_deref(), Some("2.1.0"));
        assert_eq!(doc.spec.provides.profiles.len(), 2);

        let required_pkgs = doc.spec.policy.required.packages.as_ref().unwrap();
        let brew = required_pkgs.brew.as_ref().unwrap();
        assert_eq!(brew.formulae, vec!["git-secrets", "pre-commit"]);

        assert!(doc.spec.policy.constraints.no_scripts);
        assert_eq!(doc.spec.policy.constraints.allowed_target_paths.len(), 2);
        assert_eq!(doc.spec.policy.locked.files.len(), 1);
    }

    #[test]
    fn parse_config_source_wrong_kind() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: not-a-source
spec:
  provides:
    profiles: []
  policy: {}
"#;
        let result = parse_config_source(yaml);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ConfigSource"));
    }

    #[test]
    fn config_source_default_constraints() {
        let constraints = SourceConstraints::default();
        assert!(constraints.no_scripts);
        assert!(constraints.no_secrets_read);
        assert!(constraints.allowed_target_paths.is_empty());
        assert!(!constraints.allow_system_changes);
    }

    #[test]
    fn source_spec_defaults() {
        let yaml = r#"
name: test-source
origin:
  type: Git
  url: https://example.com/config.git
"#;
        let spec: SourceSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.subscription.priority, 500);
        assert_eq!(spec.sync.interval, "1h");
        assert!(!spec.sync.auto_apply);
    }

    #[test]
    fn cargo_spec_deserialize_list() {
        let yaml = r#"
cargo:
  - bat
  - ripgrep
"#;
        #[derive(Deserialize)]
        struct Wrapper {
            cargo: CargoSpec,
        }
        let w: Wrapper = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(w.cargo.packages, vec!["bat", "ripgrep"]);
        assert!(w.cargo.file.is_none());
    }

    #[test]
    fn cargo_spec_deserialize_map() {
        let yaml = r#"
cargo:
  file: Cargo.toml
  packages:
    - extra-pkg
"#;
        #[derive(Deserialize)]
        struct Wrapper {
            cargo: CargoSpec,
        }
        let w: Wrapper = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(w.cargo.file.as_deref(), Some("Cargo.toml"));
        assert_eq!(w.cargo.packages, vec!["extra-pkg"]);
    }

    #[test]
    fn cargo_spec_deserialize_file_only() {
        let yaml = r#"
cargo:
  file: Cargo.toml
"#;
        #[derive(Deserialize)]
        struct Wrapper {
            cargo: CargoSpec,
        }
        let w: Wrapper = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(w.cargo.file.as_deref(), Some("Cargo.toml"));
        assert!(w.cargo.packages.is_empty());
    }

    #[test]
    fn packages_spec_with_manifest_files() {
        let yaml = r#"
brew:
  file: Brewfile
  formulae:
    - extra-tool
apt:
  file: packages.apt.txt
npm:
  file: package.json
  global:
    - extra-global
cargo:
  file: Cargo.toml
"#;
        let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();

        let brew = spec.brew.as_ref().unwrap();
        assert_eq!(brew.file.as_deref(), Some("Brewfile"));
        assert_eq!(brew.formulae, vec!["extra-tool"]);

        let apt = spec.apt.as_ref().unwrap();
        assert_eq!(apt.file.as_deref(), Some("packages.apt.txt"));

        let npm = spec.npm.as_ref().unwrap();
        assert_eq!(npm.file.as_deref(), Some("package.json"));
        assert_eq!(npm.global, vec!["extra-global"]);

        let cargo = spec.cargo.as_ref().unwrap();
        assert_eq!(cargo.file.as_deref(), Some("Cargo.toml"));
    }

    #[test]
    fn merge_manifest_file_fields() {
        let layer1 = ProfileLayer {
            source: "local".into(),
            profile_name: "base".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        file: Some("Brewfile".into()),
                        formulae: vec!["git".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        let layer2 = ProfileLayer {
            source: "local".into(),
            profile_name: "work".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["ripgrep".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        };

        let merged = merge_layers(&[layer1, layer2]);
        let brew = merged.packages.brew.as_ref().unwrap();
        // file from base preserved (layer2 didn't override)
        assert_eq!(brew.file.as_deref(), Some("Brewfile"));
        // formulae unioned
        assert_eq!(brew.formulae, vec!["git", "ripgrep"]);
    }

    #[test]
    fn parse_config_source_with_profile_details() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
    profileDetails:
      - name: acme-base
        description: "Core tools and security"
        path: profiles/base.yaml
      - name: acme-backend
        description: "Go, k8s tools"
        path: profiles/backend.yaml
        inherits:
          - acme-base
    platformProfiles:
      macos: profiles/platform/macos.yaml
      debian: profiles/platform/debian.yaml
  policy: {}
"#;
        let doc = parse_config_source(yaml).unwrap();
        assert_eq!(doc.spec.provides.profile_details.len(), 2);
        assert_eq!(doc.spec.provides.profile_details[0].name, "acme-base");
        assert_eq!(
            doc.spec.provides.profile_details[0].description.as_deref(),
            Some("Core tools and security")
        );
        assert_eq!(
            doc.spec.provides.profile_details[1].inherits,
            vec!["acme-base"]
        );
        assert_eq!(doc.spec.provides.platform_profiles.len(), 2);
        assert_eq!(
            doc.spec.provides.platform_profiles.get("macos").unwrap(),
            "profiles/platform/macos.yaml"
        );
    }

    #[test]
    fn parse_os_release_debian() {
        let content = r#"PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"
NAME="Debian GNU/Linux"
VERSION_ID="12"
ID=debian
"#;
        let (id, version) = parse_os_release_content(content);
        assert_eq!(id.as_deref(), Some("debian"));
        assert_eq!(version.as_deref(), Some("12"));
    }

    #[test]
    fn parse_os_release_ubuntu() {
        let content = r#"NAME="Ubuntu"
VERSION="22.04.3 LTS (Jammy Jellyfish)"
ID=ubuntu
VERSION_ID="22.04"
"#;
        let (id, version) = parse_os_release_content(content);
        assert_eq!(id.as_deref(), Some("ubuntu"));
        assert_eq!(version.as_deref(), Some("22.04"));
    }

    #[test]
    fn parse_os_release_empty() {
        let (id, version) = parse_os_release_content("");
        assert!(id.is_none());
        assert!(version.is_none());
    }

    #[test]
    fn match_platform_profile_exact_distro() {
        let mut profiles = HashMap::new();
        profiles.insert("macos".into(), "profiles/macos.yaml".into());
        profiles.insert("debian".into(), "profiles/debian.yaml".into());

        let platform = PlatformInfo {
            os: "linux".into(),
            distro: Some("debian".into()),
            distro_version: Some("12".into()),
        };
        assert_eq!(
            match_platform_profile(&platform, &profiles),
            Some("profiles/debian.yaml".into())
        );
    }

    #[test]
    fn match_platform_profile_os_fallback() {
        let mut profiles = HashMap::new();
        profiles.insert("macos".into(), "profiles/macos.yaml".into());
        profiles.insert("linux".into(), "profiles/linux.yaml".into());

        let platform = PlatformInfo {
            os: "linux".into(),
            distro: Some("arch".into()),
            distro_version: None,
        };
        // No "arch" key, falls back to "linux"
        assert_eq!(
            match_platform_profile(&platform, &profiles),
            Some("profiles/linux.yaml".into())
        );
    }

    #[test]
    fn match_platform_profile_no_match() {
        let mut profiles = HashMap::new();
        profiles.insert("debian".into(), "profiles/debian.yaml".into());

        let platform = PlatformInfo {
            os: "macos".into(),
            distro: None,
            distro_version: None,
        };
        assert!(match_platform_profile(&platform, &profiles).is_none());
    }

    #[test]
    fn source_profile_names_from_details() {
        let provides = ConfigSourceProvides {
            profiles: vec!["old-name".into()],
            profile_details: vec![
                ConfigSourceProfileEntry {
                    name: "base".into(),
                    description: Some("Base profile".into()),
                    path: None,
                    inherits: vec![],
                },
                ConfigSourceProfileEntry {
                    name: "backend".into(),
                    description: None,
                    path: None,
                    inherits: vec!["base".into()],
                },
            ],
            platform_profiles: HashMap::new(),
            modules: vec![],
        };
        // profile_details takes precedence
        assert_eq!(source_profile_names(&provides), vec!["base", "backend"]);
    }

    #[test]
    fn source_profile_names_fallback_to_profiles() {
        let provides = ConfigSourceProvides {
            profiles: vec!["alpha".into(), "beta".into()],
            profile_details: vec![],
            platform_profiles: HashMap::new(),
            modules: vec![],
        };
        assert_eq!(source_profile_names(&provides), vec!["alpha", "beta"]);
    }

    #[test]
    fn auto_apply_policy_defaults() {
        let policy = AutoApplyPolicyConfig::default();
        assert_eq!(policy.new_recommended, PolicyAction::Notify);
        assert_eq!(policy.new_optional, PolicyAction::Ignore);
        assert_eq!(policy.locked_conflict, PolicyAction::Notify);
    }

    #[test]
    fn auto_apply_policy_deserializes() {
        let yaml = r#"
newRecommended: Accept
newOptional: Notify
lockedConflict: Reject
"#;
        let policy: AutoApplyPolicyConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(policy.new_recommended, PolicyAction::Accept);
        assert_eq!(policy.new_optional, PolicyAction::Notify);
        assert_eq!(policy.locked_conflict, PolicyAction::Reject);
    }

    #[test]
    fn reconcile_config_with_policy_deserializes() {
        let yaml = r#"
interval: 5m
onChange: true
autoApply: true
policy:
  newRecommended: Accept
  newOptional: Ignore
  lockedConflict: Notify
"#;
        let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.auto_apply);
        assert!(config.on_change);
        let policy = config.policy.unwrap();
        assert_eq!(policy.new_recommended, PolicyAction::Accept);
    }

    #[test]
    fn reconcile_patches_deserialize() {
        let yaml = r#"
interval: 5m
patches:
  - kind: Module
    name: certificates
    interval: 1m
    driftPolicy: Auto
  - kind: Profile
    name: work
    autoApply: true
  - kind: Module
    interval: 30s
"#;
        let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.patches.len(), 3);
        assert_eq!(config.patches[0].kind, ReconcilePatchKind::Module);
        assert_eq!(config.patches[0].name.as_deref(), Some("certificates"));
        assert_eq!(config.patches[0].interval.as_deref(), Some("1m"));
        assert_eq!(config.patches[0].drift_policy, Some(DriftPolicy::Auto));
        assert!(config.patches[0].auto_apply.is_none());
        assert_eq!(config.patches[1].kind, ReconcilePatchKind::Profile);
        assert_eq!(config.patches[1].name.as_deref(), Some("work"));
        assert_eq!(config.patches[1].auto_apply, Some(true));
        assert!(config.patches[1].interval.is_none());
        // Kind-wide patch (no name)
        assert_eq!(config.patches[2].kind, ReconcilePatchKind::Module);
        assert!(config.patches[2].name.is_none());
        assert_eq!(config.patches[2].interval.as_deref(), Some("30s"));
    }

    #[test]
    fn reconcile_config_without_patches_has_empty_vec() {
        let yaml = "interval: 10m\n";
        let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.patches.is_empty());
    }

    // --- desired_packages_for_spec ---

    #[test]
    fn desired_packages_brew_formulae() {
        let spec = PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["curl".into(), "wget".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            desired_packages_for_spec("brew", &spec),
            vec!["curl", "wget"]
        );
    }

    #[test]
    fn desired_packages_brew_taps() {
        let spec = PackagesSpec {
            brew: Some(BrewSpec {
                taps: vec!["homebrew/core".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            desired_packages_for_spec("brew-tap", &spec),
            vec!["homebrew/core"]
        );
    }

    #[test]
    fn desired_packages_brew_casks() {
        let spec = PackagesSpec {
            brew: Some(BrewSpec {
                casks: vec!["firefox".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            desired_packages_for_spec("brew-cask", &spec),
            vec!["firefox"]
        );
    }

    #[test]
    fn desired_packages_apt() {
        let spec = PackagesSpec {
            apt: Some(AptSpec {
                packages: vec!["git".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(desired_packages_for_spec("apt", &spec), vec!["git"]);
    }

    #[test]
    fn desired_packages_cargo() {
        let spec = PackagesSpec {
            cargo: Some(CargoSpec {
                packages: vec!["ripgrep".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(desired_packages_for_spec("cargo", &spec), vec!["ripgrep"]);
    }

    #[test]
    fn desired_packages_npm() {
        let spec = PackagesSpec {
            npm: Some(NpmSpec {
                global: vec!["typescript".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(desired_packages_for_spec("npm", &spec), vec!["typescript"]);
    }

    #[test]
    fn desired_packages_pipx() {
        let spec = PackagesSpec {
            pipx: vec!["black".into()],
            ..Default::default()
        };
        assert_eq!(desired_packages_for_spec("pipx", &spec), vec!["black"]);
    }

    #[test]
    fn desired_packages_snap_merges_classic() {
        let spec = PackagesSpec {
            snap: Some(SnapSpec {
                packages: vec!["core".into()],
                classic: vec!["code".into()],
            }),
            ..Default::default()
        };
        let result = desired_packages_for_spec("snap", &spec);
        assert_eq!(result, vec!["core", "code"]);
    }

    #[test]
    fn desired_packages_snap_classic_dedup() {
        let spec = PackagesSpec {
            snap: Some(SnapSpec {
                packages: vec!["code".into()],
                classic: vec!["code".into()],
            }),
            ..Default::default()
        };
        let result = desired_packages_for_spec("snap", &spec);
        assert_eq!(result, vec!["code"]);
    }

    #[test]
    fn desired_packages_custom_manager() {
        let spec = PackagesSpec {
            custom: vec![CustomManagerSpec {
                name: "my-mgr".into(),
                check: "which my-mgr".into(),
                list_installed: "my-mgr list".into(),
                install: "my-mgr install".into(),
                uninstall: "my-mgr remove".into(),
                update: None,
                packages: vec!["tool-a".into()],
            }],
            ..Default::default()
        };
        assert_eq!(desired_packages_for_spec("my-mgr", &spec), vec!["tool-a"]);
    }

    #[test]
    fn desired_packages_unknown_manager() {
        let spec = PackagesSpec::default();
        assert!(desired_packages_for_spec("nonexistent", &spec).is_empty());
    }

    // --- load_config filesystem ---

    #[test]
    fn load_config_valid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n"
        );
        std::fs::write(&path, &yaml).unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.metadata.name, "test");
    }

    #[test]
    fn load_config_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.toml");
        let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"test\"\n\n[spec]\nprofile = \"default\"\n";
        std::fs::write(&path, toml).unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.metadata.name, "test");
    }

    #[test]
    fn load_config_missing_file() {
        let result = load_config(std::path::Path::new("/nonexistent-12345/cfgd.yaml"));
        assert!(result.is_err());
    }

    // --- resolve_profile deeper inheritance ---

    #[test]
    fn test_ai_config_defaults() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec: {}
"#;
        let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
        let ai = config.spec.ai.unwrap_or_default();
        assert_eq!(ai.provider, "claude");
        assert_eq!(ai.model, "claude-sonnet-4-6");
        assert_eq!(ai.api_key_env, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn test_ai_config_custom() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  ai:
    provider: claude
    model: claude-opus-4-6
    apiKeyEnv: MY_CLAUDE_KEY
"#;
        let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
        let ai = config.spec.ai.unwrap_or_default();
        assert_eq!(ai.model, "claude-opus-4-6");
        assert_eq!(ai.api_key_env, "MY_CLAUDE_KEY");
    }

    #[test]
    fn test_existing_config_without_ai_still_parses() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work
  theme: default
"#;
        let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.spec.ai.is_none());
    }

    #[test]
    fn three_level_inheritance() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();

        let grandparent = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: grandparent\nspec:\n  inherits: []\n  modules: []\n  env:\n    - name: A\n      value: '1'\n";
        let parent = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: parent\nspec:\n  inherits:\n    - grandparent\n  modules: []\n  env:\n    - name: B\n      value: '2'\n";
        let child = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - parent\n  modules: []\n  env:\n    - name: C\n      value: '3'\n";

        std::fs::write(profiles.join("grandparent.yaml"), grandparent).unwrap();
        std::fs::write(profiles.join("parent.yaml"), parent).unwrap();
        std::fs::write(profiles.join("child.yaml"), child).unwrap();

        let resolved = resolve_profile("child", &profiles).unwrap();

        // Should have all three env vars merged
        let names: Vec<&str> = resolved
            .merged
            .env
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
    }
}
