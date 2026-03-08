// Config types, profile resolution, and multi-source prep
// Types defined in Phase 1 for use in later phases (4-9)
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{ConfigError, Result};

// --- Root Config (cfgd.yaml) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CfgdConfig {
    pub api_version: String,
    pub kind: String,
    pub metadata: ConfigMetadata,
    pub spec: ConfigSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConfigMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConfigSpec {
    pub profile: String,

    #[serde(default)]
    pub origin: Vec<OriginSpec>,

    #[serde(default)]
    pub daemon: Option<DaemonConfig>,

    #[serde(default)]
    pub secrets: Option<SecretsConfig>,

    #[serde(default)]
    pub sources: Vec<SourceSpec>,
}

// Custom deserialization: origin can be a single object or an array
// Internally always Vec<OriginSpec> with primary at index 0
impl ConfigSpec {
    pub fn primary_origin(&self) -> Option<&OriginSpec> {
        self.origin.first()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OriginSpec {
    #[serde(rename = "type")]
    pub origin_type: OriginType,
    pub url: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    pub auth: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OriginType {
    Git,
    Server,
}

fn default_branch() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
pub struct ReconcileConfig {
    #[serde(default = "default_reconcile_interval")]
    pub interval: String,
    #[serde(default)]
    pub on_change: bool,
}

fn default_reconcile_interval() -> String {
    "5m".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SyncConfig {
    #[serde(default)]
    pub auto_push: bool,
    #[serde(default)]
    pub auto_pull: bool,
    #[serde(default = "default_sync_interval")]
    pub interval: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NotifyConfig {
    #[serde(default)]
    pub drift: bool,
    #[serde(default)]
    pub method: NotifyMethod,
    pub webhook_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyMethod {
    #[default]
    Desktop,
    Stdout,
    Webhook,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
pub struct SopsConfig {
    pub age_key: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SecretIntegration {
    pub name: String,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

// --- Multi-source prep (Phase 9) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SourceSpec {
    pub name: String,
    pub origin: OriginSpec,
    #[serde(default)]
    pub subscription: SubscriptionSpec,
    #[serde(default)]
    pub sync: SourceSyncSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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

// --- Profile ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ProfileDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ProfileMetadata,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ProfileMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ProfileSpec {
    #[serde(default)]
    pub inherits: Vec<String>,

    #[serde(default)]
    pub variables: HashMap<String, serde_yaml::Value>,

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
#[serde(rename_all = "kebab-case")]
pub struct PackagesSpec {
    #[serde(default)]
    pub brew: Option<BrewSpec>,
    #[serde(default)]
    pub apt: Option<AptSpec>,
    #[serde(default)]
    pub cargo: Vec<String>,
    #[serde(default)]
    pub npm: Option<NpmSpec>,
    #[serde(default)]
    pub pipx: Vec<String>,
    #[serde(default)]
    pub dnf: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BrewSpec {
    #[serde(default)]
    pub taps: Vec<String>,
    #[serde(default)]
    pub formulae: Vec<String>,
    #[serde(default)]
    pub casks: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AptSpec {
    #[serde(default)]
    pub install: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NpmSpec {
    #[serde(default)]
    pub global: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FilesSpec {
    #[serde(default)]
    pub managed: Vec<ManagedFileSpec>,
    #[serde(default)]
    pub permissions: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ManagedFileSpec {
    pub source: String,
    pub target: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SecretSpec {
    pub source: String,
    pub target: PathBuf,
    pub template: Option<String>,
    pub backend: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ScriptSpec {
    #[serde(default)]
    pub pre_apply: Vec<PathBuf>,
    #[serde(default)]
    pub post_apply: Vec<PathBuf>,
}

// --- Profile Resolution ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerPolicy {
    Local,
    Required,
    Recommended,
    Optional,
}

#[derive(Debug, Clone)]
pub struct ProfileLayer {
    pub source: String,
    pub profile_name: String,
    pub priority: u32,
    pub policy: LayerPolicy,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub layers: Vec<ProfileLayer>,
    pub merged: MergedProfile,
}

#[derive(Debug, Clone, Default)]
pub struct MergedProfile {
    pub variables: HashMap<String, serde_yaml::Value>,
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
        },
    })
}

// Internal raw types for flexible origin deserialization
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawCfgdConfig {
    api_version: String,
    kind: String,
    metadata: ConfigMetadata,
    spec: RawConfigSpec,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawConfigSpec {
    profile: String,
    #[serde(default)]
    origin: Option<RawOrigin>,
    #[serde(default)]
    daemon: Option<DaemonConfig>,
    #[serde(default)]
    secrets: Option<SecretsConfig>,
    #[serde(default)]
    sources: Vec<SourceSpec>,
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

/// Resolve a profile by loading it and its full inheritance chain, then merging.
pub fn resolve_profile(profile_name: &str, profiles_dir: &Path) -> Result<ResolvedProfile> {
    let resolution_order = resolve_inheritance_order(profile_name, profiles_dir, &mut vec![])?;

    let mut layers = Vec::new();
    for name in &resolution_order {
        let path = profiles_dir.join(format!("{}.yaml", name));
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

    let path = profiles_dir.join(format!("{}.yaml", profile_name));
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

/// Extend a list with items from another list, skipping duplicates.
fn union_extend(target: &mut Vec<String>, source: &[String]) {
    for item in source {
        if !target.contains(item) {
            target.push(item.clone());
        }
    }
}

/// Merge profile layers according to merge rules:
/// - packages: union
/// - files: overlay (later overrides earlier for same target)
/// - variables: override (later replaces earlier for same key)
/// - secrets: append (deduplicated by target)
/// - scripts: append in order
/// - system: deep merge (later overrides at leaf level)
fn merge_layers(layers: &[ProfileLayer]) -> MergedProfile {
    let mut merged = MergedProfile::default();

    for layer in layers {
        let spec = &layer.spec;

        // Variables: later overrides earlier
        for (k, v) in &spec.variables {
            merged.variables.insert(k.clone(), v.clone());
        }

        // Packages: union
        if let Some(ref pkgs) = spec.packages {
            if let Some(ref brew) = pkgs.brew {
                let merged_brew = merged.packages.brew.get_or_insert_with(BrewSpec::default);
                union_extend(&mut merged_brew.taps, &brew.taps);
                union_extend(&mut merged_brew.formulae, &brew.formulae);
                union_extend(&mut merged_brew.casks, &brew.casks);
            }
            if let Some(ref apt) = pkgs.apt {
                let apt_spec = merged.packages.apt.get_or_insert_with(AptSpec::default);
                union_extend(&mut apt_spec.install, &apt.install);
            }
            union_extend(&mut merged.packages.cargo, &pkgs.cargo);
            if let Some(ref npm) = pkgs.npm {
                let npm_spec = merged.packages.npm.get_or_insert_with(NpmSpec::default);
                union_extend(&mut npm_spec.global, &npm.global);
            }
            union_extend(&mut merged.packages.pipx, &pkgs.pipx);
            union_extend(&mut merged.packages.dnf, &pkgs.dnf);
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
            merged.scripts.pre_apply.extend(scripts.pre_apply.clone());
            merged.scripts.post_apply.extend(scripts.post_apply.clone());
        }
    }

    merged
}

/// Deep merge two YAML values. Later values override at leaf level.
fn deep_merge_yaml(base: &mut serde_yaml::Value, overlay: &serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                if let Some(base_value) = base_map.get_mut(key) {
                    deep_merge_yaml(base_value, value);
                } else {
                    base_map.insert(key.clone(), value.clone());
                }
            }
        }
        (base, overlay) => {
            *base = overlay.clone();
        }
    }
}

/// Interpolate variables in a string value.
/// Replaces `${var_name}` with the corresponding variable value.
pub fn interpolate_variables(
    input: &str,
    variables: &HashMap<String, serde_yaml::Value>,
) -> String {
    let mut result = input.to_string();
    for (key, value) in variables {
        let placeholder = format!("${{{}}}", key);
        let replacement = match value {
            serde_yaml::Value::String(s) => s.clone(),
            serde_yaml::Value::Number(n) => n.to_string(),
            serde_yaml::Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        result = result.replace(&placeholder, &replacement);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config_yaml() -> &'static str {
        r#"
api-version: cfgd/v1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
  origin:
    type: git
    url: https://github.com/test/repo.git
    branch: main
"#
    }

    fn sample_config_no_origin() -> &'static str {
        r#"
api-version: cfgd/v1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
"#
    }

    fn sample_profile_yaml() -> &'static str {
        r#"
api-version: cfgd/v1
kind: Profile
metadata:
  name: base
spec:
  variables:
    editor: vim
    shell: /bin/zsh
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
        assert_eq!(config.spec.profile, "default");
        assert_eq!(config.spec.origin.len(), 1);
        assert_eq!(
            config.spec.origin[0].url,
            "https://github.com/test/repo.git"
        );
        assert_eq!(config.spec.origin[0].branch, "main");
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
        assert_eq!(doc.spec.variables.len(), 2);
        let pkgs = doc.spec.packages.as_ref().unwrap();
        let brew = pkgs.brew.as_ref().unwrap();
        assert_eq!(brew.formulae, vec!["ripgrep", "fd"]);
        assert_eq!(pkgs.cargo, vec!["bat"]);
    }

    #[test]
    fn merge_variables_override() {
        let layer1 = ProfileLayer {
            source: "local".into(),
            profile_name: "base".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                variables: HashMap::from([
                    ("editor".into(), serde_yaml::Value::String("vim".into())),
                    (
                        "shell".into(),
                        serde_yaml::Value::String("/bin/bash".into()),
                    ),
                ]),
                ..Default::default()
            },
        };
        let layer2 = ProfileLayer {
            source: "local".into(),
            profile_name: "work".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                variables: HashMap::from([(
                    "editor".into(),
                    serde_yaml::Value::String("code".into()),
                )]),
                ..Default::default()
            },
        };

        let merged = merge_layers(&[layer1, layer2]);
        assert_eq!(
            merged.variables.get("editor"),
            Some(&serde_yaml::Value::String("code".into()))
        );
        assert_eq!(
            merged.variables.get("shell"),
            Some(&serde_yaml::Value::String("/bin/bash".into()))
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
                    cargo: vec!["bat".into()],
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
                    cargo: vec!["bat".into(), "exa".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        };

        let merged = merge_layers(&[layer1, layer2]);
        assert_eq!(merged.packages.cargo, vec!["bat", "exa"]);
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
    fn interpolate_variables_replaces_placeholders() {
        let vars = HashMap::from([
            ("name".into(), serde_yaml::Value::String("cfgd".into())),
            ("version".into(), serde_yaml::Value::Number(42.into())),
        ]);

        let result = interpolate_variables("Hello ${name} v${version}!", &vars);
        assert_eq!(result, "Hello cfgd v42!");
    }

    #[test]
    fn interpolate_variables_no_match() {
        let vars = HashMap::new();
        let result = interpolate_variables("no ${vars} here", &vars);
        assert_eq!(result, "no ${vars} here");
    }

    #[test]
    fn profile_resolution_with_filesystem() {
        let dir = tempfile::tempdir().unwrap();

        // Create base profile
        std::fs::write(
            dir.path().join("base.yaml"),
            r#"
api-version: cfgd/v1
kind: Profile
metadata:
  name: base
spec:
  variables:
    editor: vim
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
api-version: cfgd/v1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - base
  variables:
    editor: code
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
            resolved.merged.variables.get("editor"),
            Some(&serde_yaml::Value::String("code".into()))
        );
        // packages should be unioned
        assert_eq!(resolved.merged.packages.cargo, vec!["bat", "exa"]);
    }

    #[test]
    fn circular_inheritance_detected() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(
            dir.path().join("a.yaml"),
            r#"
api-version: cfgd/v1
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
api-version: cfgd/v1
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
    fn source_spec_defaults() {
        let yaml = r#"
name: test-source
origin:
  type: git
  url: https://example.com/config.git
"#;
        let spec: SourceSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.subscription.priority, 500);
        assert_eq!(spec.sync.interval, "1h");
        assert!(!spec.sync.auto_apply);
    }
}
