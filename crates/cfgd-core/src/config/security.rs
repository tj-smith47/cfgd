use serde::{Deserialize, Serialize};

use super::module::ModuleRegistryEntry;

/// `spec.security`: source signature-verification settings.
///
/// ```yaml
/// security:
///   allowUnsigned: false
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityConfig {
    /// Allow unsigned source content even when the source requires signed commits.
    /// Intended for development/testing environments.
    #[serde(default)]
    pub allow_unsigned: bool,
}

/// `spec.modules`: module registries and their security requirements.
///
/// ```yaml
/// modules:
///   registries:
///     - name: acme
///       url: https://github.com/acme/cfgd-modules
///   security:
///     requireSignatures: true
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModulesConfig {
    /// Module registries — git repos containing modules in a prescribed directory structure.
    #[serde(default)]
    pub registries: Vec<ModuleRegistryEntry>,

    /// Signature requirements for modules pulled from these registries.
    /// Omitted, signatures are not required and an unsigned module tag is
    /// accepted.
    #[serde(default)]
    pub security: Option<ModuleSecurityConfig>,
}

/// `spec.modules.security`: signature requirements for modules pulled from a registry.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleSecurityConfig {
    /// Require GPG/SSH signatures on all remote module tags.
    /// When true, unsigned modules are rejected unless `--allow-unsigned` is passed.
    #[serde(default)]
    pub require_signatures: bool,
}
