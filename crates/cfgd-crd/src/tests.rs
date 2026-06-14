use super::*;

const TEST_PEM_KEY: &str =
    "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----";

#[test]
fn crd_specs_and_validate_are_exposed() {
    use kube::CustomResourceExt;
    // types resolve from cfgd-crd, not cfgd-operator
    let _ = MachineConfig::crd();
    let bad = MachineConfigSpec::example_with_traversal_path();
    assert!(
        bad.validate().is_err(),
        "cross-field validate must reject `..` in file paths"
    );
}

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
    let errs = minimal_mc_spec("", "default").validate().unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("hostname")),
        "should mention hostname: {errs:?}"
    );
}

#[test]
fn mc_validate_rejects_empty_profile() {
    let errs = minimal_mc_spec("host1", "").validate().unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("profile")),
        "should mention profile: {errs:?}"
    );
}

#[test]
fn mc_validate_collects_all_errors() {
    // Both hostname and profile empty — should report BOTH, not just first
    let errs = minimal_mc_spec("", "").validate().unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("hostname")),
        "should mention hostname: {errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.contains("profile")),
        "should mention profile: {errs:?}"
    );
    assert!(errs.len() >= 2, "should report at least 2 errors: {errs:?}");
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
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("9999") && e.contains("octal")),
        "should mention the bad mode and that octal is expected: {errs:?}"
    );
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
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("packages") && e.contains("name")),
        "should mention packages and name: {errs:?}"
    );
}

#[test]
fn mc_validate_rejects_path_traversal() {
    let mut spec = minimal_mc_spec("host1", "default");
    spec.files.push(FileSpec {
        path: "/etc/../shadow".to_string(),
        content: Some("data".to_string()),
        source: None,
        mode: "0644".to_string(),
    });
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("..") && e.contains("traversal")),
        "should reject path traversal: {errs:?}"
    );
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
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("content") && e.contains("source")),
        "should require content or source: {errs:?}"
    );
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
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("packages") && e.contains("name")),
        "should mention packages and name: {errs:?}"
    );
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
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("requiredModules") || e.contains("name")),
        "should mention required modules or name: {errs:?}"
    );
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
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("not valid")),
        "should mention the invalid version string: {errs:?}"
    );
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
    let errs = spec.validate().unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("not valid")),
        "should mention the invalid version string: {errs:?}"
    );
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

// Spec-side unknown-field rejection is now enforced by k8s structural
// schema (admission prunes unknown fields with a warning before persistence
// to etcd) instead of `#[serde(deny_unknown_fields)]`. The serde attribute
// emits `additionalProperties: false` via schemars 0.8, which k8s rejects
// alongside `properties:` (mutually exclusive in structural-schema rules).
// Status types still need forward-compat acceptance — pinned below.

#[test]
fn machine_config_status_accepts_unknown_field_for_forward_compat() {
    // CRD status subresources must accept unknown fields: a newer controller
    // may emit fields the old binary does not know yet, and a strict reject
    // would break the rolling upgrade window. Pin that behavior.
    let yaml = "lastReconciled: '2026-01-01T00:00:00Z'\nbrandNewField: 42\n";
    let result: Result<MachineConfigStatus, _> = serde_yaml::from_str(yaml);
    assert!(
        result.is_ok(),
        "MachineConfigStatus must accept unknown fields for forward compat, got: {:?}",
        result.err()
    );
}
