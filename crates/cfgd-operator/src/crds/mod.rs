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
    pub packages: Vec<PackageRef>,
    #[serde(default)]
    pub files: Vec<FileSpec>,
    #[serde(default)]
    pub system_settings: BTreeMap<String, serde_json::Value>,
}

/// Reference to a package with optional version pin.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
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
    pub observed_generation: Option<i64>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Reported installed versions keyed by package name (e.g. {"kubectl": "1.28.3"}).
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

// ---------------------------------------------------------------------------
// ConfigPolicy
// ---------------------------------------------------------------------------

/// Kubernetes-style label selector with match_labels and match_expressions.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelector {
    #[serde(default)]
    pub match_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub match_expressions: Vec<LabelSelectorRequirement>,
}

/// A single requirement for label selector expressions.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelectorRequirement {
    pub key: String,
    pub operator: String,
    #[serde(default)]
    pub values: Vec<String>,
}

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
    #[serde(default)]
    pub required_modules: Vec<ModuleRef>,
    #[serde(default)]
    pub packages: Vec<PackageRef>,
    /// Version requirements keyed by package name (semver ranges, e.g. {"kubectl": ">=1.28"}).
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub target_selector: LabelSelector,
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

/// Typed reference to a MachineConfig resource.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfigReference {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

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
    pub machine_config_ref: MachineConfigReference,
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
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftDetail {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
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
        for (i, pkg) in self.packages.iter().enumerate() {
            if pkg.name.is_empty() {
                errors.push(format!("spec.packages[{i}].name must not be empty"));
            }
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

        for (i, pkg) in self.packages.iter().enumerate() {
            if pkg.name.is_empty() {
                errors.push(format!("spec.packages[{i}].name must not be empty"));
            }
        }
        for (i, mr) in self.required_modules.iter().enumerate() {
            if mr.name.is_empty() {
                errors.push(format!(
                    "spec.requiredModules[{i}].name must not be empty"
                ));
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
pub use cfgd_core::version_satisfies;

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_mc_spec(hostname: &str, profile: &str) -> MachineConfigSpec {
        MachineConfigSpec {
            hostname: hostname.to_string(),
            profile: profile.to_string(),
            module_refs: vec![],
            packages: vec![],
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
    fn mc_validate_rejects_empty_package_name() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.packages.push(PackageRef {
            name: String::new(),
            version: None,
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn cp_validate_rejects_empty_package_name() {
        let spec = ConfigPolicySpec {
            required_modules: vec![],
            packages: vec![PackageRef {
                name: String::new(),
                version: None,
            }],
            package_versions: Default::default(),
            settings: Default::default(),
            target_selector: Default::default(),
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn cp_validate_rejects_empty_module_name() {
        let spec = ConfigPolicySpec {
            required_modules: vec![ModuleRef {
                name: String::new(),
                required: true,
            }],
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
        assert!(cfgd_core::parse_loose_version("1.28.3").is_some());
        assert!(cfgd_core::parse_loose_version("0.1.0").is_some());
    }

    #[test]
    fn parse_loose_version_two_part() {
        assert!(cfgd_core::parse_loose_version("1.28").is_some());
    }

    #[test]
    fn parse_loose_version_single_part() {
        assert!(cfgd_core::parse_loose_version("1").is_some());
    }

    #[test]
    fn parse_loose_version_rejects_garbage() {
        assert!(cfgd_core::parse_loose_version("abc").is_none());
        assert!(cfgd_core::parse_loose_version("").is_none());
        assert!(cfgd_core::parse_loose_version("1.2.3.4").is_none());
    }

    #[test]
    fn version_satisfies_basic() {
        assert!(version_satisfies("1.28.3", ">=1.28"));
        assert!(!version_satisfies("1.27.0", ">=1.28"));
        assert!(version_satisfies("2.40.1", "~2.40"));
        assert!(!version_satisfies("2.39.0", "~2.40"));
    }

    #[test]
    fn condition_has_observed_generation() {
        let c = Condition {
            condition_type: "Ready".to_string(),
            status: "True".to_string(),
            reason: "Test".to_string(),
            message: "test".to_string(),
            last_transition_time: "2024-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        };
        assert_eq!(c.observed_generation, Some(1));
    }

    #[test]
    fn version_satisfies_loose() {
        assert!(version_satisfies("1.28", ">=1.28"));
        assert!(version_satisfies("2", ">=1.28"));
        assert!(!version_satisfies("1", ">=1.28"));
    }

    #[test]
    fn package_ref_serialization() {
        let pr = PackageRef {
            name: "vim".to_string(),
            version: Some("1.0.0".to_string()),
        };
        let json = serde_json::to_value(&pr).unwrap();
        assert_eq!(json["name"], "vim");
        assert_eq!(json["version"], "1.0.0");
    }

    #[test]
    fn package_ref_omits_none_version() {
        let pr = PackageRef {
            name: "vim".to_string(),
            version: None,
        };
        let json = serde_json::to_value(&pr).unwrap();
        assert_eq!(json["name"], "vim");
        assert!(json.get("version").is_none());
    }

    #[test]
    fn label_selector_empty_matches_all() {
        let sel = LabelSelector::default();
        assert!(sel.match_labels.is_empty());
        assert!(sel.match_expressions.is_empty());
    }

    #[test]
    fn mc_reconciled_condition_set_on_success() {
        let status = MachineConfigStatus {
            last_reconciled: Some("2024-01-01T00:00:00Z".to_string()),
            observed_generation: Some(1),
            package_versions: Default::default(),
            conditions: vec![
                Condition {
                    condition_type: "Reconciled".to_string(),
                    status: "True".to_string(),
                    reason: "ReconcileSuccess".to_string(),
                    message: "Reconciled successfully".to_string(),
                    last_transition_time: "2024-01-01T00:00:00Z".to_string(),
                    observed_generation: Some(1),
                },
                Condition {
                    condition_type: "DriftDetected".to_string(),
                    status: "False".to_string(),
                    reason: "NoDrift".to_string(),
                    message: "No drift detected".to_string(),
                    last_transition_time: "2024-01-01T00:00:00Z".to_string(),
                    observed_generation: Some(1),
                },
            ],
        };
        assert_eq!(status.conditions.len(), 2);
        assert_eq!(status.conditions[0].condition_type, "Reconciled");
    }

    #[test]
    fn machine_config_reference_serialization() {
        let r = MachineConfigReference {
            name: "mc-1".to_string(),
            namespace: Some("team-a".to_string()),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["name"], "mc-1");
        assert_eq!(json["namespace"], "team-a");
    }

    #[test]
    fn machine_config_reference_omits_none_namespace() {
        let r = MachineConfigReference {
            name: "mc-1".to_string(),
            namespace: None,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["name"], "mc-1");
        assert!(json.get("namespace").is_none());
    }
}
