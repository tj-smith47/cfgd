use serde::{Deserialize, Serialize};

use super::source::default_true;

// ---------------------------------------------------------------------------
// Compliance configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComplianceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_compliance_interval")]
    pub interval: String,
    #[serde(default = "default_compliance_retention")]
    pub retention: String,
    #[serde(default)]
    pub scope: ComplianceScope,
    #[serde(default)]
    pub export: ComplianceExport,
}

fn default_compliance_interval() -> String {
    "1h".into()
}
fn default_compliance_retention() -> String {
    "30d".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComplianceScope {
    #[serde(default = "default_true")]
    pub files: bool,
    #[serde(default = "default_true")]
    pub packages: bool,
    #[serde(default = "default_true")]
    pub system: bool,
    #[serde(default = "default_true")]
    pub secrets: bool,
    #[serde(default)]
    pub watch_paths: Vec<String>,
    #[serde(default)]
    pub watch_package_managers: Vec<String>,
}

impl Default for ComplianceScope {
    fn default() -> Self {
        Self {
            files: true,
            packages: true,
            system: true,
            secrets: true,
            watch_paths: Vec::new(),
            watch_package_managers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub enum ComplianceFormat {
    #[default]
    Json,
    Yaml,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComplianceExport {
    #[serde(default)]
    pub format: ComplianceFormat,
    #[serde(default = "default_compliance_path")]
    pub path: String,
}

fn default_compliance_path() -> String {
    "~/.local/share/cfgd/compliance/".into()
}

impl Default for ComplianceExport {
    fn default() -> Self {
        Self {
            format: ComplianceFormat::default(),
            path: default_compliance_path(),
        }
    }
}
