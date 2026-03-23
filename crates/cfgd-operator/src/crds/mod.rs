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
    status = "MachineConfigStatus",
    shortname = "mc",
    category = "cfgd",
    printcolumn = r#"{"name": "Hostname", "type": "string", "jsonPath": ".spec.hostname"}"#,
    printcolumn = r#"{"name": "Profile", "type": "string", "jsonPath": ".spec.profile"}"#,
    printcolumn = r#"{"name": "Reconciled", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Reconciled\")].status"}"#,
    printcolumn = r#"{"name": "Drift", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"DriftDetected\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
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
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Reference to a module that should be installed on the machine.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleRef {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
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

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
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

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
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
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelector {
    #[serde(default)]
    pub match_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub match_expressions: Vec<LabelSelectorRequirement>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum SelectorOperator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
}

/// A single requirement for label selector expressions.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelectorRequirement {
    pub key: String,
    pub operator: SelectorOperator,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ConfigPolicy",
    namespaced,
    status = "ConfigPolicyStatus",
    shortname = "cpol",
    category = "cfgd",
    printcolumn = r#"{"name": "Compliant", "type": "integer", "jsonPath": ".status.compliantCount"}"#,
    printcolumn = r#"{"name": "NonCompliant", "type": "integer", "jsonPath": ".status.nonCompliantCount"}"#,
    printcolumn = r#"{"name": "Enforced", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Enforced\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPolicySpec {
    #[serde(default)]
    pub required_modules: Vec<ModuleRef>,
    /// Modules staged as debug-only (CSI volume without volumeMount on declared containers).
    #[serde(default)]
    pub debug_modules: Vec<ModuleRef>,
    #[serde(default)]
    pub packages: Vec<PackageRef>,
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub target_selector: LabelSelector,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
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
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
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
    status = "DriftAlertStatus",
    shortname = "da",
    category = "cfgd",
    printcolumn = r#"{"name": "Device", "type": "string", "jsonPath": ".spec.deviceId"}"#,
    printcolumn = r#"{"name": "Severity", "type": "string", "jsonPath": ".spec.severity"}"#,
    printcolumn = r#"{"name": "Resolved", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Resolved\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct DriftAlertSpec {
    pub device_id: String,
    pub machine_config_ref: MachineConfigReference,
    #[serde(default)]
    pub drift_details: Vec<DriftDetail>,
    pub severity: DriftSeverity,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftAlertStatus {
    pub detected_at: Option<String>,
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftDetail {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum DriftSeverity {
    Low,
    Medium,
    High,
    Critical,
}

// ---------------------------------------------------------------------------
// ClusterConfigPolicy
// ---------------------------------------------------------------------------

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ClusterConfigPolicy",
    status = "ClusterConfigPolicyStatus",
    shortname = "ccpol",
    category = "cfgd",
    printcolumn = r#"{"name": "Compliant", "type": "integer", "jsonPath": ".status.compliantCount"}"#,
    printcolumn = r#"{"name": "NonCompliant", "type": "integer", "jsonPath": ".status.nonCompliantCount"}"#,
    printcolumn = r#"{"name": "Enforced", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Enforced\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfigPolicySpec {
    /// Select which namespaces this cluster policy applies to.
    #[serde(default)]
    pub namespace_selector: LabelSelector,
    #[serde(default)]
    pub required_modules: Vec<ModuleRef>,
    /// Modules staged as debug-only across matching namespaces.
    #[serde(default)]
    pub debug_modules: Vec<ModuleRef>,
    #[serde(default)]
    pub packages: Vec<PackageRef>,
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub security: SecurityPolicy,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityPolicy {
    #[serde(default)]
    pub trusted_registries: Vec<String>,
    #[serde(default)]
    pub allow_unsigned: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfigPolicyStatus {
    pub compliant_count: u32,
    pub non_compliant_count: u32,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// An entry in a Module's package list with optional per-platform overrides.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageEntry {
    pub name: String,
    /// Per-platform package name overrides (e.g. {"brew": "gnu-sed", "apt": "sed"}).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub platforms: BTreeMap<String, String>,
}

/// A file managed by a Module.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleFileSpec {
    pub source: String,
    pub target: String,
}

/// Scripts that run during module lifecycle.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleScripts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_apply: Option<String>,
}

/// An environment variable set by a Module.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleEnvVar {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub append: bool,
}

/// Cosign configuration for OCI artifact signature verification.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CosignSignature {
    /// PEM-encoded public key for static key verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// Enable keyless verification via Fulcio/Rekor (OIDC identity-based).
    #[serde(default)]
    pub keyless: bool,
    /// Certificate identity pattern for keyless verification (regex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer pattern for keyless verification (regex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
}

/// Signature configuration for a Module's OCI artifact.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSignature {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cosign: Option<CosignSignature>,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "Module",
    status = "ModuleStatus",
    shortname = "mod",
    category = "cfgd",
    printcolumn = r#"{"name": "Artifact", "type": "string", "jsonPath": ".spec.ociArtifact"}"#,
    printcolumn = r#"{"name": "Verified", "type": "boolean", "jsonPath": ".status.verified"}"#,
    printcolumn = r#"{"name": "Platforms", "type": "string", "jsonPath": ".status.availablePlatforms"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSpec {
    #[serde(default)]
    pub packages: Vec<PackageEntry>,
    #[serde(default)]
    pub files: Vec<ModuleFileSpec>,
    #[serde(default)]
    pub scripts: ModuleScripts,
    #[serde(default)]
    pub env: Vec<ModuleEnvVar>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ModuleSignature>,
    /// Controls how the module is mounted in pods.
    /// `Always` (default): CSI volume + volumeMount on all declared containers + env vars.
    /// `Debug`: CSI volume only — no volumeMount or env on declared containers.
    ///   Only accessible via ephemeral debug containers (`kubectl cfgd debug`).
    #[serde(default)]
    pub mount_policy: MountPolicy,
}

/// Controls how a module is exposed to pod containers.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum MountPolicy {
    /// Mount into all declared containers with volumeMount and env vars.
    #[default]
    Always,
    /// Stage the CSI volume on the pod but do not mount into declared containers.
    /// Only accessible via ephemeral debug containers.
    Debug,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_artifact: Option<String>,
    #[serde(default)]
    pub available_platforms: Vec<String>,
    #[serde(default)]
    pub verified: bool,
    /// Digest of the cosign signature (if verified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_digest: Option<String>,
    /// Attestation types found on the artifact (e.g. "slsaprovenance").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attestations: Vec<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

// ---------------------------------------------------------------------------
// Shared validation
// ---------------------------------------------------------------------------

fn validate_policy_fields(
    packages: &[PackageRef],
    required_modules: &[ModuleRef],
    settings: &BTreeMap<String, serde_json::Value>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (i, pkg) in packages.iter().enumerate() {
        if pkg.name.is_empty() {
            errors.push(format!("spec.packages[{i}].name must not be empty"));
        }
        if let Some(ver) = &pkg.version {
            if VersionReq::parse(ver).is_err() {
                errors.push(format!(
                    "spec.packages[{i}].version '{ver}' is not a valid semver requirement"
                ));
            }
        }
    }
    for (i, mr) in required_modules.iter().enumerate() {
        if mr.name.is_empty() {
            errors.push(format!("spec.requiredModules[{i}].name must not be empty"));
        }
    }
    for key in settings.keys() {
        if key.is_empty() {
            errors.push("spec.settings key must not be empty".to_string());
        }
    }
    errors
}

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
        for (i, m) in self.module_refs.iter().enumerate() {
            if m.name.is_empty() {
                errors.push(format!("spec.moduleRefs[{i}].name must not be empty"));
            }
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
        let errors = validate_policy_fields(
            &self.packages,
            &self.required_modules,
            &self.settings,
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl ClusterConfigPolicySpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let errors = validate_policy_fields(
            &self.packages,
            &self.required_modules,
            &self.settings,
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl DriftAlertSpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.device_id.is_empty() {
            errors.push("spec.deviceId must not be empty".to_string());
        }
        if self.machine_config_ref.name.is_empty() {
            errors.push("spec.machineConfigRef.name must not be empty".to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl ModuleSpec {
    /// Validate the spec, returning all validation errors found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        for (i, pkg) in self.packages.iter().enumerate() {
            if pkg.name.is_empty() {
                errors.push(format!("spec.packages[{i}].name must not be empty"));
            }
        }
        for (i, dep) in self.depends.iter().enumerate() {
            if dep.is_empty() {
                errors.push(format!("spec.depends[{i}] must not be empty"));
            }
        }
        if let Some(ref oci) = self.oci_artifact
            && !is_valid_oci_reference(oci)
        {
            errors.push(format!(
                "spec.ociArtifact '{}' is not a valid OCI reference",
                oci
            ));
        }
        if let Some(ref sig) = self.signature
            && let Some(ref cosign) = sig.cosign
            && let Some(ref pk) = cosign.public_key
            && !is_valid_pem_public_key(pk)
        {
            errors.push(
                "spec.signature.cosign.publicKey is not a valid PEM-encoded public key".to_string(),
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Validate an OCI artifact reference using cfgd-core's full OCI reference parser.
pub(crate) fn is_valid_oci_reference(reference: &str) -> bool {
    cfgd_core::oci::OciReference::parse(reference).is_ok()
}

/// Check if a string looks like a PEM-encoded public key.
pub(crate) fn is_valid_pem_public_key(key: &str) -> bool {
    let trimmed = key.trim();
    trimmed.starts_with("-----BEGIN PUBLIC KEY-----")
        && trimmed.ends_with("-----END PUBLIC KEY-----")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PEM_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----";

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
            packages: vec![PackageRef {
                name: String::new(),
                version: None,
            }],
            ..Default::default()
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
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn cp_validate_rejects_invalid_version_req() {
        let spec = ConfigPolicySpec {
            packages: vec![PackageRef {
                name: "kubectl".to_string(),
                version: Some("not valid".to_string()),
            }],
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn cp_validate_accepts_valid_version_reqs() {
        let spec = ConfigPolicySpec {
            packages: vec![
                PackageRef {
                    name: "kubectl".to_string(),
                    version: Some(">=1.28".to_string()),
                },
                PackageRef {
                    name: "git".to_string(),
                    version: Some("~2.40".to_string()),
                },
            ],
            ..Default::default()
        };
        assert!(spec.validate().is_ok());
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
    fn cluster_config_policy_is_cluster_scoped() {
        use kube::CustomResourceExt;
        let crd = ClusterConfigPolicy::crd();
        assert_eq!(crd.spec.scope, "Cluster");
    }

    #[test]
    fn cluster_config_policy_has_short_name() {
        use kube::CustomResourceExt;
        let crd = ClusterConfigPolicy::crd();
        let short_names = crd.spec.names.short_names.as_ref().unwrap();
        assert!(short_names.contains(&"ccpol".to_string()));
    }

    #[test]
    fn ccp_validate_accepts_minimal() {
        let spec = ClusterConfigPolicySpec::default();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn ccp_validate_rejects_invalid_version() {
        let spec = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: "kubectl".to_string(),
                version: Some("not valid".to_string()),
            }],
            ..Default::default()
        };
        assert!(spec.validate().is_err());
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

    #[test]
    fn machineconfig_crd_has_printer_columns() {
        use kube::CustomResourceExt;
        let crd = MachineConfig::crd();
        let version = &crd.spec.versions[0];
        let columns = version.additional_printer_columns.as_ref().unwrap();
        let col_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        assert!(col_names.contains(&"Hostname"));
        assert!(col_names.contains(&"Profile"));
        assert!(col_names.contains(&"Age"));
    }

    #[test]
    fn machineconfig_crd_has_short_names() {
        use kube::CustomResourceExt;
        let crd = MachineConfig::crd();
        let short_names = crd.spec.names.short_names.as_ref().unwrap();
        assert!(short_names.contains(&"mc".to_string()));
    }

    #[test]
    fn configpolicy_crd_has_short_names() {
        use kube::CustomResourceExt;
        let crd = ConfigPolicy::crd();
        let short_names = crd.spec.names.short_names.as_ref().unwrap();
        assert!(short_names.contains(&"cpol".to_string()));
    }

    #[test]
    fn driftalert_crd_has_short_names() {
        use kube::CustomResourceExt;
        let crd = DriftAlert::crd();
        let short_names = crd.spec.names.short_names.as_ref().unwrap();
        assert!(short_names.contains(&"da".to_string()));
    }

    #[test]
    fn mc_validate_accepts_file_with_both_content_and_source() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.files.push(FileSpec {
            path: "/etc/foo".to_string(),
            content: Some("data".to_string()),
            source: Some("https://example.com/foo".to_string()),
            mode: "0644".to_string(),
        });
        // Both content and source is allowed — content takes priority at apply time
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn mc_validate_rejects_file_mode_exceeding_7777() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.files.push(FileSpec {
            path: "/etc/foo".to_string(),
            content: Some("data".to_string()),
            source: None,
            mode: "17777".to_string(),
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn mc_validate_rejects_empty_module_ref_name() {
        let mut spec = minimal_mc_spec("host1", "default");
        spec.module_refs.push(ModuleRef {
            name: String::new(),
            required: false,
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn da_validate_rejects_empty_device_id() {
        let spec = DriftAlertSpec {
            device_id: String::new(),
            machine_config_ref: MachineConfigReference {
                name: "mc-1".to_string(),
                namespace: None,
            },
            drift_details: vec![],
            severity: DriftSeverity::Low,
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn da_validate_rejects_empty_mc_ref_name() {
        let spec = DriftAlertSpec {
            device_id: "dev-1".to_string(),
            machine_config_ref: MachineConfigReference {
                name: String::new(),
                namespace: None,
            },
            drift_details: vec![],
            severity: DriftSeverity::Low,
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn da_validate_accepts_valid() {
        let spec = DriftAlertSpec {
            device_id: "dev-1".to_string(),
            machine_config_ref: MachineConfigReference {
                name: "mc-1".to_string(),
                namespace: None,
            },
            drift_details: vec![],
            severity: DriftSeverity::Medium,
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn ccp_validate_rejects_empty_package_name() {
        let spec = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: String::new(),
                version: None,
            }],
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn ccp_validate_rejects_empty_module_name() {
        let spec = ClusterConfigPolicySpec {
            required_modules: vec![ModuleRef {
                name: String::new(),
                required: false,
            }],
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    // -----------------------------------------------------------------------
    // Module CRD tests
    // -----------------------------------------------------------------------

    #[test]
    fn module_crd_is_cluster_scoped() {
        use kube::CustomResourceExt;
        let crd = Module::crd();
        assert_eq!(crd.spec.scope, "Cluster");
    }

    #[test]
    fn module_crd_has_short_name() {
        use kube::CustomResourceExt;
        let crd = Module::crd();
        let short_names = crd.spec.names.short_names.as_ref().unwrap();
        assert!(short_names.contains(&"mod".to_string()));
    }

    #[test]
    fn module_crd_has_category() {
        use kube::CustomResourceExt;
        let crd = Module::crd();
        let categories = crd.spec.names.categories.as_ref().unwrap();
        assert!(categories.contains(&"cfgd".to_string()));
    }

    #[test]
    fn module_crd_has_printer_columns() {
        use kube::CustomResourceExt;
        let crd = Module::crd();
        let version = &crd.spec.versions[0];
        let columns = version.additional_printer_columns.as_ref().unwrap();
        let col_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        assert!(col_names.contains(&"Artifact"));
        assert!(col_names.contains(&"Verified"));
        assert!(col_names.contains(&"Platforms"));
        assert!(col_names.contains(&"Age"));
    }

    #[test]
    fn module_validate_accepts_minimal() {
        let spec = ModuleSpec::default();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn module_validate_accepts_full() {
        let spec = ModuleSpec {
            packages: vec![PackageEntry {
                name: "vim".to_string(),
                platforms: BTreeMap::new(),
            }],
            files: vec![ModuleFileSpec {
                source: "vimrc".to_string(),
                target: "~/.vimrc".to_string(),
            }],
            scripts: ModuleScripts {
                post_apply: Some("echo done".to_string()),
            },
            env: vec![ModuleEnvVar {
                name: "EDITOR".to_string(),
                value: "vim".to_string(),
                append: false,
            }],
            depends: vec!["base".to_string()],
            oci_artifact: Some("registry.example.com/modules/vim:v1".to_string()),
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    public_key: Some(TEST_PEM_KEY.to_string()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn module_validate_rejects_empty_package_name() {
        let spec = ModuleSpec {
            packages: vec![PackageEntry {
                name: String::new(),
                platforms: BTreeMap::new(),
            }],
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn module_validate_rejects_empty_depends() {
        let spec = ModuleSpec {
            depends: vec![String::new()],
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn module_validate_rejects_malformed_oci_ref() {
        let spec = ModuleSpec {
            oci_artifact: Some("  ".to_string()),
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn module_validate_rejects_invalid_pem_key() {
        let spec = ModuleSpec {
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    public_key: Some("not-a-pem-key".to_string()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn oci_reference_validation() {
        assert!(is_valid_oci_reference("registry.example.com/repo:v1"));
        assert!(is_valid_oci_reference("repo:tag"));
        assert!(is_valid_oci_reference(
            "repo@sha256:abcdef1234567890abcdef1234567890"
        ));
        assert!(is_valid_oci_reference("myrepo"));
        assert!(!is_valid_oci_reference(""));
        assert!(!is_valid_oci_reference("has space"));
    }

    #[test]
    fn pem_key_validation() {
        assert!(is_valid_pem_public_key(
            "-----BEGIN PUBLIC KEY-----\ndata\n-----END PUBLIC KEY-----"
        ));
        assert!(!is_valid_pem_public_key("not-pem"));
        assert!(!is_valid_pem_public_key(
            "-----BEGIN PRIVATE KEY-----\ndata\n-----END PRIVATE KEY-----"
        ));
    }

    #[test]
    fn module_status_defaults() {
        let status = ModuleStatus::default();
        assert!(!status.verified);
        assert!(status.available_platforms.is_empty());
        assert!(status.resolved_artifact.is_none());
        assert!(status.signature_digest.is_none());
        assert!(status.attestations.is_empty());
    }
}
