use serde::{Deserialize, Serialize};

use super::module::ModuleRegistryEntry;

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityConfig {
    /// Allow unsigned source content even when the source requires signed commits.
    /// Intended for development/testing environments.
    #[serde(default)]
    pub allow_unsigned: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModulesConfig {
    /// Module registries — git repos containing modules in a prescribed directory structure.
    #[serde(default)]
    pub registries: Vec<ModuleRegistryEntry>,

    /// Module security settings.
    #[serde(default)]
    pub security: Option<ModuleSecurityConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleSecurityConfig {
    /// Require GPG/SSH signatures on all remote module tags.
    /// When true, unsigned modules are rejected unless `--allow-unsigned` is passed.
    #[serde(default)]
    pub require_signatures: bool,
}
