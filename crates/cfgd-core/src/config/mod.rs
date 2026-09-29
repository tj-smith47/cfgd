// Config types, profile resolution, and multi-source prep.
//
// This module is split into per-concern submodules; everything previously
// public at `cfgd_core::config::X` is preserved here via `pub use` re-exports.

mod ai;
mod comments;
mod compliance;
mod daemon;
mod image_lock;
mod modeline;
mod module;
mod origin;
mod output;
mod parse;
mod platform;
mod preferences;
mod profile_spec;
mod resolve;
mod root;
mod security;
mod source;
mod source_lock;
mod sync_secrets;
mod theme;

#[cfg(test)]
mod tests;

pub use ai::AiConfig;
pub use comments::{leading_comment_block, with_leading_comments};
pub use compliance::{ComplianceConfig, ComplianceExport, ComplianceFormat, ComplianceScope};
pub use daemon::{
    AutoApplyPolicyConfig, DaemonConfig, DriftPolicy, PolicyAction, ReconcileConfig,
    ReconcilePatch, ReconcilePatchKind,
};
pub use image_lock::{ImageLockEntry, ImagesLockfile};
pub use modeline::{SchemaDocKind, docs_url, schema_modeline, with_schema_modeline};
pub use module::{
    ModuleDocument, ModuleFileEntry, ModuleLockEntry, ModuleLockfile, ModuleMetadata,
    ModulePackageEntry, ModuleRegistryEntry, ModuleSpec, parse_module,
    validate_module_file_entries, validate_module_package_entries,
};
pub use origin::{OriginSpec, OriginType, SshHostKeyPolicy};
pub use output::{MaskEnvValues, OutputConfig};
pub(crate) use parse::{API_VERSION_CONVERSIONS, readable_api_versions, validate_api_version};
pub use parse::{
    CONFIG_FILENAME, CONFIG_FILENAME_TOML, LEGACY_OUTPUT_KEYS, PROFILE_FILENAME, ProfileEntry,
    ProfileForm, ProfileManifests, ProfileScanEntry, canonical_profile_path, config_document_in,
    find_profile_path, load_config, load_profile, parse_config, parse_config_source,
    read_config_document, resolve_config_path, scan_profile_manifests, scan_profiles,
    scan_profiles_tolerant,
};
pub use platform::{PlatformInfo, detect_platform, match_platform_profile, source_profile_names};
pub(crate) use preferences::ChainPreferences;
pub use preferences::{PreferencesSpec, resolved_env, validate_preferences};
pub use profile_spec::{
    AptSpec, BrewSpec, CargoSpec, CustomManagerSpec, EncryptionConstraint, EnvScope, FilesSpec,
    FlatpakSpec, ManagedFileSpec, MergeSpec, NpmSpec, PackagesSpec, ProfileDocument,
    ProfileMetadata, ProfileSpec, SecretSpec, SnapSpec, SystemSettings, render_backup_name_pattern,
    validate_backup_specs, validate_managed_file_specs, validate_package_specs,
    validate_secret_specs,
};

// The value types cfgd-schema owns, kept resolvable at their long-standing
// `cfgd_core::config::*` paths so the CRD sharing them changes no caller here.
pub use cfgd_schema::{
    BackupSpec, EncryptionMode, EncryptionSpec, FileStrategy, MigrationPolicy, PatchFormat,
    PatchSpec, ScheduleOwner, ScriptCommand, ScriptEntry, ScriptShell, ScriptSpec,
};
pub(crate) use profile_spec::{profile_spec_from_value, validate_backup_name};
pub(crate) use resolve::fold_preferences;
pub use resolve::{
    ALL_MANAGER_NAMES, DEFAULT_PACKAGE_NOUN, EntryOwners, LOCAL_LAYER, LOCAL_LAYER_PRIORITY,
    LayerPolicy, LayerSources, MergedProfile, PACKAGE_SCHEMA_PATHS, PackageClaim,
    PackageSchemaPath, ProfileLayer, ResolvedProfile, desired_packages_for,
    desired_packages_for_spec, merge_layers, package_schema_path, resolve_profile,
};
pub use root::{
    CfgdConfig, ConfigMetadata, ConfigSpec, STABLE_UPDATE_CHANNEL, SkillUpdateConfig,
    SkillUpdatePolicy, UpdateConfig, UpdatePolicy, minimal_config,
};
pub use security::{ModuleSecurityConfig, ModulesConfig, SecurityConfig};
pub use source::{
    ConfigSourceDocument, ConfigSourceMetadata, ConfigSourcePolicy, ConfigSourceProfileEntry,
    ConfigSourceProvides, ConfigSourceSpec, EnvVar, MAX_SOURCE_PRIORITY, PolicyItems, ShellAlias,
    SourceConstraints, SourceSpec, SourceSyncSpec, SubscriptionSpec, validate_source_priority,
};
pub use source_lock::{SourceLockEntry, SourcesLockfile};
pub use sync_secrets::{
    NotifyConfig, NotifyMethod, SecretIntegration, SecretsConfig, SopsConfig, SyncConfig,
};
pub use theme::{ThemeConfig, ThemeOverrides};

/// Read an explicit `null` as the field's default, the way a bare `key:` with
/// nothing after it already reads. A writer that serializes an emptied block
/// prints `null`, and a document holding the file's own output must load.
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    Ok(<Option<T> as serde::Deserialize>::deserialize(deserializer)?.unwrap_or_default())
}
