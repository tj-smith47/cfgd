use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ai::AiConfig;
use super::compliance::ComplianceConfig;
use super::daemon::DaemonConfig;
use super::origin::OriginSpec;
use super::profile_spec::{FileStrategy, ProfileDocument};
use super::root::{CfgdConfig, ConfigMetadata, ConfigSpec, UpdateConfig};
use super::security::{ModulesConfig, SecurityConfig};
use super::source::{ConfigSourceDocument, SourceSpec};
use super::sync_secrets::SecretsConfig;
use super::theme::ThemeConfig;
use crate::PathDisplayExt;
use crate::errors::{ConfigError, Result};

/// Canonical discovery filename for the root cfgd config (YAML form). The single
/// source of truth so the CLI default and the directory-inference path can never
/// diverge onto a sibling filename.
pub const CONFIG_FILENAME: &str = "cfgd.yaml";

/// Canonical discovery filename for the root cfgd config (TOML form). TOML is a
/// first-class supported config form (see [`parse_config`]), so a config repo may
/// carry `cfgd.toml` instead of [`CONFIG_FILENAME`].
pub const CONFIG_FILENAME_TOML: &str = "cfgd.toml";

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

/// Reject a document whose `apiVersion` is not the version this build understands.
///
/// Every document parse path ([`parse_config`], [`load_profile`], `parse_module`,
/// [`parse_config_source`]) routes through this single check so an unknown version
/// (e.g. a future `cfgd.io/v1alpha2`) fails loudly with a typed
/// [`ConfigError::UnsupportedApiVersion`] instead of silently deserializing under
/// the current schema. The typed variant is the matchable hook a future
/// version-migration path plugs into.
pub(crate) fn validate_api_version(api_version: &str) -> Result<()> {
    if api_version != crate::API_VERSION {
        return Err(ConfigError::UnsupportedApiVersion {
            found: api_version.to_string(),
        }
        .into());
    }
    Ok(())
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
    validate_api_version(&doc.api_version)?;

    Ok(doc)
}

/// Resolve a config path that may name a directory to the concrete config file
/// inside it. A directory argument (`--config <dir>`, `CFGD_CONFIG=<dir>`, or a
/// default that resolves to a dir) infers the conventional discovery filename:
/// [`CONFIG_FILENAME`] if present, else [`CONFIG_FILENAME_TOML`], else falls back
/// to the `.yaml` path so the NotFound error names a concrete file the user can
/// create rather than the bare directory. Non-directory paths pass through
/// unchanged.
///
/// Callers that derive sibling locations from the config path (e.g. the
/// `profiles/` directory next to it) should normalize through this first so the
/// loaded file and its derived siblings agree on a single resolved path.
pub fn resolve_config_path(path: &Path) -> PathBuf {
    if !path.is_dir() {
        return path.to_path_buf();
    }
    let yaml = path.join(CONFIG_FILENAME);
    if yaml.exists() {
        return yaml;
    }
    let toml = path.join(CONFIG_FILENAME_TOML);
    if toml.exists() {
        return toml;
    }
    yaml
}

/// Load and parse the root cfgd.yaml config file
pub fn load_config(path: &Path) -> Result<CfgdConfig> {
    let resolved = resolve_config_path(path);
    let path = resolved.as_path();
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

    validate_api_version(&raw.api_version)?;

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
            update: raw.spec.update,
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
    #[serde(default)]
    update: Option<UpdateConfig>,
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
    validate_api_version(&doc.api_version)?;
    Ok(doc)
}

/// Fixed manifest filename inside a canonical profile bundle directory
/// (`profiles/<name>/profile.yaml`). Mirrors the module convention
/// (`modules/<name>/module.yaml`): fixed manifest names take no `.yml` variant.
pub const PROFILE_FILENAME: &str = "profile.yaml";

/// The on-disk layout form a profile manifest was discovered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProfileForm {
    /// Bundle form: `profiles/<name>/profile.yaml` (the canonical layout).
    Canonical,
    /// Flat form: `profiles/<name>.yaml` / `.yml` (supported until 1.0).
    LegacyFlat,
}

/// A profile discovered by [`scan_profiles`]: its name, manifest path, and
/// which layout form it uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileEntry {
    pub name: String,
    pub path: PathBuf,
    pub form: ProfileForm,
}

/// Canonical (write-side) manifest path for a profile:
/// `<profiles_dir>/<name>/profile.yaml`. Pure path construction — no I/O and
/// no existence check; readers go through [`find_profile_path`] instead.
pub fn canonical_profile_path(profiles_dir: &Path, name: &str) -> PathBuf {
    profiles_dir.join(name).join(PROFILE_FILENAME)
}

/// Find a profile manifest by name, probing the canonical bundle form
/// (`<name>/profile.yaml`) first, then the legacy flat forms (`<name>.yaml`,
/// `<name>.yml`).
///
/// Fails closed on ambiguity: if the canonical form coexists with a legacy
/// file, or both legacy extensions exist, returns
/// [`ConfigError::AmbiguousProfile`] naming both paths. Returns
/// [`ConfigError::ProfileNotFound`] when no form exists.
pub fn find_profile_path(
    profiles_dir: &Path,
    name: &str,
) -> std::result::Result<PathBuf, ConfigError> {
    let canonical = canonical_profile_path(profiles_dir, name);
    let legacy_yaml = profiles_dir.join(format!("{}.yaml", name));
    let legacy_yml = profiles_dir.join(format!("{}.yml", name));

    let legacy = match (legacy_yaml.is_file(), legacy_yml.is_file()) {
        (true, true) => {
            return Err(ConfigError::AmbiguousProfile {
                name: name.to_string(),
                path_a: legacy_yaml,
                path_b: legacy_yml,
            });
        }
        (true, false) => Some(legacy_yaml),
        (false, true) => Some(legacy_yml),
        (false, false) => None,
    };

    match (canonical.is_file(), legacy) {
        (true, Some(legacy)) => Err(ConfigError::AmbiguousProfile {
            name: name.to_string(),
            path_a: canonical,
            path_b: legacy,
        }),
        (true, None) => Ok(canonical),
        (false, Some(legacy)) => Ok(legacy),
        (false, None) => Err(ConfigError::ProfileNotFound {
            name: name.to_string(),
        }),
    }
}

/// One name yielded by the ambiguity-tolerant walk ([`scan_profiles_tolerant`]):
/// an unambiguous profile, or the [`ConfigError::AmbiguousProfile`] a strict
/// load of that name would raise.
#[derive(Debug)]
pub enum ProfileScanEntry {
    /// Exactly one manifest form exists for this name.
    Found(ProfileEntry),
    /// Multiple forms coexist on disk; `error` is the fail-closed load error.
    Ambiguous { name: String, error: ConfigError },
}

impl ProfileScanEntry {
    /// The profile name, regardless of variant.
    pub fn name(&self) -> &str {
        match self {
            ProfileScanEntry::Found(entry) => &entry.name,
            ProfileScanEntry::Ambiguous { name, .. } => name,
        }
    }
}

/// Classify one `profiles_dir` child as a profile manifest, or `None` when it
/// is not one. The single classifier shared by [`scan_profiles`] and
/// [`scan_profiles_tolerant`] so the two walks cannot drift.
fn classify_profile_dir_entry(path: PathBuf) -> Option<(String, ProfileForm, PathBuf)> {
    if path.is_dir() {
        let manifest = path.join(PROFILE_FILENAME);
        if !manifest.is_file() {
            return None; // legacy payload dir (e.g. <name>/files/), not a profile
        }
        let name = path.file_name().and_then(|n| n.to_str())?.to_string();
        Some((name, ProfileForm::Canonical, manifest))
    } else if path.is_file()
        && matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml") | Some("yml")
        )
    {
        // Exact lowercase extensions only (not the case-insensitive
        // is_yaml_ext): find_profile_path probes literal `.yaml`/`.yml`,
        // so a case-insensitive scan would enumerate profiles that are
        // unresolvable by name.
        let stem = path.file_stem().and_then(|s| s.to_str())?.to_string();
        Some((stem, ProfileForm::LegacyFlat, path))
    } else {
        None
    }
}

/// Enumerate every profile in `profiles_dir` in a single directory walk.
///
/// Yields one [`ProfileEntry`] per profile, covering both layout forms:
/// subdirectories containing a `profile.yaml` (canonical) and flat
/// `<name>.yaml` / `.yml` files (legacy). A subdirectory *without* a
/// `profile.yaml` is not a profile — it is ignored (it's the payload dir of a
/// legacy flat profile). Entries are sorted by name. Ambiguity (both forms, or
/// both legacy extensions, for one name) fails closed with
/// [`ConfigError::AmbiguousProfile`]. A missing `profiles_dir` yields an empty
/// list; any other I/O failure is an error.
///
/// The fail-closed wrapper over [`scan_profiles_tolerant`] — use that when a
/// per-name report (including ambiguous names) is needed instead of a hard
/// stop at the first ambiguity.
pub fn scan_profiles(profiles_dir: &Path) -> std::result::Result<Vec<ProfileEntry>, ConfigError> {
    let mut entries = Vec::new();
    for entry in scan_profiles_tolerant(profiles_dir)? {
        match entry {
            ProfileScanEntry::Found(found) => entries.push(found),
            ProfileScanEntry::Ambiguous { error, .. } => return Err(error),
        }
    }
    Ok(entries)
}

/// Ambiguity-tolerant variant of [`scan_profiles`]: walks `profiles_dir` once
/// and yields one [`ProfileScanEntry`] per name. A name whose forms are
/// ambiguous (canonical + legacy, or both legacy extensions) is still
/// enumerated — as [`ProfileScanEntry::Ambiguous`] carrying the error a strict
/// load would raise — so callers can report each name individually
/// (e.g. `cfgd profile migrate --all`, `cfgd doctor`). Entries are sorted by
/// name. A missing `profiles_dir` yields an empty list; any other I/O failure
/// (e.g. permission denied) is an error, never an empty result.
pub fn scan_profiles_tolerant(
    profiles_dir: &Path,
) -> std::result::Result<Vec<ProfileScanEntry>, ConfigError> {
    let read_dir = match std::fs::read_dir(profiles_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ConfigError::Invalid {
                message: format!("failed to read {}: {}", profiles_dir.posix(), e),
            });
        }
    };

    let mut entries: Vec<ProfileScanEntry> = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|e| ConfigError::Invalid {
            message: format!("failed to read {}: {}", profiles_dir.posix(), e),
        })?;
        let Some((name, form, manifest)) = classify_profile_dir_entry(entry.path()) else {
            continue;
        };

        match entries.iter().position(|e| e.name() == name) {
            Some(idx) => {
                let ProfileScanEntry::Found(existing) = &entries[idx] else {
                    continue; // a third form: the name is already ambiguous
                };
                // read_dir order is filesystem-dependent; order the pair the
                // way find_profile_path reports it (canonical, then `.yaml`,
                // then `.yml`) so the error message is stable.
                let rank = |form: ProfileForm, path: &Path| match form {
                    ProfileForm::Canonical => 0,
                    ProfileForm::LegacyFlat => {
                        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                            1
                        } else {
                            2
                        }
                    }
                };
                let (path_a, path_b) =
                    if rank(existing.form, &existing.path) <= rank(form, &manifest) {
                        (existing.path.clone(), manifest)
                    } else {
                        (manifest, existing.path.clone())
                    };
                entries[idx] = ProfileScanEntry::Ambiguous {
                    name: name.clone(),
                    error: ConfigError::AmbiguousProfile {
                        name,
                        path_a,
                        path_b,
                    },
                };
            }
            None => entries.push(ProfileScanEntry::Found(ProfileEntry {
                name,
                path: manifest,
                form,
            })),
        }
    }

    entries.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(entries)
}
