// Config types, profile resolution, and multi-source prep.
//
// This module is split into per-concern submodules; everything previously
// public at `cfgd_core::config::X` is preserved here via `pub use` re-exports.

mod ai;
mod compliance;
mod daemon;
mod module;
mod origin;
mod parse;
mod platform;
mod profile_spec;
mod resolve;
mod root;
mod security;
mod source;
mod sync_secrets;
mod theme;

#[cfg(test)]
mod tests;

pub use ai::AiConfig;
pub use compliance::{ComplianceConfig, ComplianceExport, ComplianceFormat, ComplianceScope};
pub use daemon::{
    AutoApplyPolicyConfig, DaemonConfig, DriftPolicy, PolicyAction, ReconcileConfig,
    ReconcilePatch, ReconcilePatchKind,
};
pub use module::{
    ModuleDocument, ModuleFileEntry, ModuleLockEntry, ModuleLockfile, ModuleMetadata,
    ModulePackageEntry, ModuleRegistryEntry, ModuleSpec, ScriptEntry, ScriptShell, parse_module,
};
pub use origin::{OriginSpec, OriginType, SshHostKeyPolicy};
pub use parse::{
    CONFIG_FILENAME, CONFIG_FILENAME_TOML, load_config, load_profile, parse_config,
    parse_config_source, resolve_config_path,
};
pub use platform::{PlatformInfo, detect_platform, match_platform_profile, source_profile_names};
pub use profile_spec::{
    AptSpec, BrewSpec, CargoSpec, CustomManagerSpec, EncryptionConstraint, EncryptionMode,
    EncryptionSpec, EnvScope, FileStrategy, FilesSpec, FlatpakSpec, ManagedFileSpec, NpmSpec,
    PackagesSpec, ProfileDocument, ProfileMetadata, ProfileSpec, ScriptSpec, SecretSpec, SnapSpec,
    validate_secret_specs,
};
pub use resolve::{
    ALL_MANAGER_NAMES, LayerPolicy, MergedProfile, PackageClaim, ProfileLayer, ResolvedProfile,
    desired_packages_for, desired_packages_for_spec, resolve_profile,
};
pub use root::{
    CfgdConfig, ConfigMetadata, ConfigSpec, for_each_yaml_file, is_yaml_ext, minimal_config,
};
pub use security::{ModuleSecurityConfig, ModulesConfig, SecurityConfig};
pub use source::{
    ConfigSourceDocument, ConfigSourceMetadata, ConfigSourcePolicy, ConfigSourceProfileEntry,
    ConfigSourceProvides, ConfigSourceSpec, EnvVar, MAX_SOURCE_PRIORITY, PolicyItems, ShellAlias,
    SourceConstraints, SourceSpec, SourceSyncSpec, SubscriptionSpec, validate_source_priority,
};
pub use sync_secrets::{
    NotifyConfig, NotifyMethod, SecretIntegration, SecretsConfig, SopsConfig, SyncConfig,
};
pub use theme::{ThemeConfig, ThemeOverrides};
