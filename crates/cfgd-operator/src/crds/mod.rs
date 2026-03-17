use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use semver::VersionReq;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MachineConfig
// ---------------------------------------------------------------------------

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "MachineConfig",
    namespaced,
    status = "MachineConfigStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfigSpec {
    pub hostname: String,
    pub profile: String,
    #[serde(default)]
    pub module_refs: Vec<ModuleRef>,
    #[serde(default)]
    pub packages: Vec<String>,
    /// Reported installed versions keyed by package name (e.g. {"kubectl": "1.28.3"}).
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub files: Vec<FileSpec>,
    #[serde(default)]
    pub system_settings: BTreeMap<String, String>,
}

/// Reference to a module that should be installed on the machine.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleRef {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileSpec {
    pub path: String,
    pub content: Option<String>,
    pub source: Option<String>,
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "0644".to_string()
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfigStatus {
    pub last_reconciled: Option<String>,
    #[serde(default)]
    pub drift_detected: bool,
    #[serde(default)]
    pub observed_generation: Option<i64>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub status: String,
    pub reason: String,
    pub message: String,
    pub last_transition_time: String,
}

// ---------------------------------------------------------------------------
// ConfigPolicy
// ---------------------------------------------------------------------------

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ConfigPolicy",
    namespaced,
    status = "ConfigPolicyStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPolicySpec {
    pub name: String,
    #[serde(default)]
    pub required_modules: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    /// Version requirements keyed by package name (semver ranges, e.g. {"kubectl": ">=1.28"}).
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub target_selector: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPolicyStatus {
    pub compliant_count: u32,
    pub non_compliant_count: u32,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

// ---------------------------------------------------------------------------
// DriftAlert
// ---------------------------------------------------------------------------

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "DriftAlert",
    namespaced,
    status = "DriftAlertStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct DriftAlertSpec {
    pub device_id: String,
    pub machine_config_ref: String,
    #[serde(default)]
    pub drift_details: Vec<DriftDetail>,
    pub severity: DriftSeverity,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftAlertStatus {
    pub detected_at: Option<String>,
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub resolved: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftDetail {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DriftSeverity {
    Low,
    Medium,
    High,
    Critical,
}

// ---------------------------------------------------------------------------
// Shared validation
// ---------------------------------------------------------------------------

impl MachineConfigSpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.hostname.is_empty() {
            errors.push("spec.hostname must not be empty".to_string());
        }
        if self.profile.is_empty() {
            errors.push("spec.profile must not be empty".to_string());
        }
        for (i, file) in self.files.iter().enumerate() {
            if file.path.is_empty() {
                errors.push(format!("spec.files[{i}].path must not be empty"));
            }
            if file.content.is_none() && file.source.is_none() {
                errors.push(format!(
                    "spec.files[{i}] ('{}') must have either content or source",
                    file.path
                ));
            }
            match u32::from_str_radix(&file.mode, 8) {
                Ok(mode) if mode > 0o7777 => {
                    errors.push(format!(
                        "spec.files[{i}].mode '{}' exceeds maximum 7777",
                        file.mode
                    ));
                }
                Err(_) => {
                    errors.push(format!(
                        "spec.files[{i}].mode '{}' is not valid octal",
                        file.mode
                    ));
                }
                _ => {}
            }
        }
        for (pkg, version) in &self.package_versions {
            if pkg.is_empty() {
                errors.push("spec.packageVersions key must not be empty".to_string());
            }
            if parse_loose_version(version).is_none() {
                errors.push(format!(
                    "spec.packageVersions['{pkg}'] = '{version}' is not a valid version"
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl ConfigPolicySpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.name.is_empty() {
            errors.push("spec.name must not be empty".to_string());
        }
        for (i, pkg) in self.packages.iter().enumerate() {
            if pkg.is_empty() {
                errors.push(format!("spec.packages[{i}] must not be empty"));
            }
        }
        for (pkg, req_str) in &self.package_versions {
            if pkg.is_empty() {
                errors.push("spec.packageVersions key must not be empty".to_string());
            }
            if VersionReq::parse(req_str).is_err() {
                errors.push(format!(
                    "spec.packageVersions['{pkg}'] = '{req_str}' is not a valid semver requirement"
                ));
            }
        }
        for key in self.settings.keys() {
            if key.is_empty() {
                errors.push("spec.settings key must not be empty".to_string());
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

// Re-export shared version utilities from cfgd-core
pub use cfgd_core::{parse_loose_version, version_satisfies};

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_mc_spec(hostname: &str, profile: &str) -> MachineConfigSpec {
        MachineConfigSpec {
            hostname: hostname.to_string(),
            profile: profile.to_string(),
            module_refs: vec![],
            packages: vec![],
            package_versions: Default::default(),
            files: vec![],
            system_settings: Default::default(),
        }
    }

    #[test]
    fn mc_validate_rejects_empty_hostname() {
        assert!(minimal_mc_spec("", "default").validate().is_err());
    }

    #[test]
    fn mc_validate_rejects_empty_profile() {
        assert!(minimal_mc_spec("host1", "").validate().is_err());
    }

    #[test]
    fn mc_validate_rejects_file_without_content_or_source() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.files.push(FileSpec {
            path: "/etc/foo".to_string(),
            content: None,
            source: None,
            mode: "0644".to_string(),
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn mc_validate_rejects_invalid_file_mode() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.files.push(FileSpec {
            path: "/etc/foo".to_string(),
            content: Some("data".to_string()),
            source: None,
            mode: "9999".to_string(),
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn mc_validate_accepts_valid() {
        assert!(minimal_mc_spec("host1", "default").validate().is_ok());
    }

    #[test]
    fn mc_validate_rejects_invalid_version() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.package_versions
            .insert("kubectl".to_string(), "not-a-version".to_string());
        assert!(spec.validate().is_err());
    }

    #[test]
    fn mc_validate_accepts_loose_versions() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.package_versions
            .insert("kubectl".to_string(), "1.28".to_string());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn cp_validate_rejects_empty_name() {
        let spec = ConfigPolicySpec {
            name: String::new(),
            required_modules: vec![],
            packages: vec![],
            package_versions: Default::default(),
            settings: Default::default(),
            target_selector: Default::default(),
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn cp_validate_rejects_invalid_version_req() {
        let mut versions = BTreeMap::new();
        versions.insert("kubectl".to_string(), "not valid".to_string());
        let spec = ConfigPolicySpec {
            name: "policy1".to_string(),
            required_modules: vec![],
            packages: vec![],
            package_versions: versions,
            settings: Default::default(),
            target_selector: Default::default(),
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn cp_validate_accepts_valid_version_reqs() {
        let mut versions = BTreeMap::new();
        versions.insert("kubectl".to_string(), ">=1.28".to_string());
        versions.insert("git".to_string(), "~2.40".to_string());
        let spec = ConfigPolicySpec {
            name: "policy1".to_string(),
            required_modules: vec![],
            packages: vec![],
            package_versions: versions,
            settings: Default::default(),
            target_selector: Default::default(),
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn parse_loose_version_full_semver() {
        assert!(parse_loose_version("1.28.3").is_some());
        assert!(parse_loose_version("0.1.0").is_some());
    }

    #[test]
    fn parse_loose_version_two_part() {
        assert!(parse_loose_version("1.28").is_some());
    }

    #[test]
    fn parse_loose_version_single_part() {
        assert!(parse_loose_version("1").is_some());
    }

    #[test]
    fn parse_loose_version_rejects_garbage() {
        assert!(parse_loose_version("abc").is_none());
        assert!(parse_loose_version("").is_none());
        assert!(parse_loose_version("1.2.3.4").is_none());
    }

    #[test]
    fn version_satisfies_basic() {
        assert!(version_satisfies("1.28.3", ">=1.28"));
        assert!(!version_satisfies("1.27.0", ">=1.28"));
        assert!(version_satisfies("2.40.1", "~2.40"));
        assert!(!version_satisfies("2.39.0", "~2.40"));
    }

    #[test]
    fn version_satisfies_loose() {
        assert!(version_satisfies("1.28", ">=1.28"));
        assert!(version_satisfies("2", ">=1.28"));
        assert!(!version_satisfies("1", ">=1.28"));
    }
}
