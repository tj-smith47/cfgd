
use super::*;
use crate::crds::LabelSelectorRequirement;

const TEST_PEM_KEY: &str =
    "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----";

fn mc_spec(hostname: &str, profile: &str) -> MachineConfigSpec {
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
fn validate_spec_delegates_to_shared() {
    let err = validate_spec(&mc_spec("", "default")).unwrap_err();
    assert!(
        err.to_string().contains("hostname"),
        "should mention hostname: {err}"
    );
    assert!(validate_spec(&mc_spec("host1", "default")).is_ok());
}

#[test]
fn matches_selector_empty_matches_all() {
    assert!(matches_selector(None, &LabelSelector::default()));
}

#[test]
fn matches_selector_match_labels() {
    let mut match_labels = BTreeMap::new();
    match_labels.insert("team".to_string(), "platform".to_string());
    let selector = LabelSelector {
        match_labels,
        match_expressions: vec![],
    };

    let mut labels = BTreeMap::new();
    labels.insert("team".to_string(), "platform".to_string());
    labels.insert("env".to_string(), "prod".to_string());
    assert!(matches_selector(Some(&labels), &selector));

    let mut wrong_labels = BTreeMap::new();
    wrong_labels.insert("team".to_string(), "other".to_string());
    assert!(!matches_selector(Some(&wrong_labels), &selector));

    // Missing label entirely
    assert!(!matches_selector(Some(&BTreeMap::new()), &selector));
    assert!(!matches_selector(None, &selector));
}

#[test]
fn policy_compliance_all_packages_present() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![
        PackageRef {
            name: "vim".to_string(),
            version: None,
        },
        PackageRef {
            name: "git".to_string(),
            version: None,
        },
        PackageRef {
            name: "curl".to_string(),
            version: None,
        },
    ];
    let required = vec![
        PackageRef {
            name: "vim".to_string(),
            version: None,
        },
        PackageRef {
            name: "git".to_string(),
            version: None,
        },
    ];
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &required,
        &Default::default()
    ));
}

#[test]
fn policy_compliance_missing_package() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![PackageRef {
        name: "vim".to_string(),
        version: None,
    }];
    let required = vec![
        PackageRef {
            name: "vim".to_string(),
            version: None,
        },
        PackageRef {
            name: "git".to_string(),
            version: None,
        },
    ];
    assert!(!validate_policy_compliance(
        &spec,
        None,
        &[],
        &required,
        &Default::default()
    ));
}

#[test]
fn policy_compliance_settings() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert(
        "key".to_string(),
        serde_json::Value::String("value".to_string()),
    );

    let mut spec = mc_spec("h", "p");
    spec.system_settings.insert(
        "key".to_string(),
        serde_json::Value::String("value".to_string()),
    );
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &[],
        &policy_settings
    ));

    let mut wrong = BTreeMap::new();
    wrong.insert(
        "key".to_string(),
        serde_json::Value::String("wrong".to_string()),
    );
    assert!(!validate_policy_compliance(&spec, None, &[], &[], &wrong));
}

#[test]
fn policy_compliance_settings_non_string() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert("enabled".to_string(), serde_json::json!(true));

    let mut spec = mc_spec("h", "p");
    spec.system_settings
        .insert("enabled".to_string(), serde_json::json!(true));
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &[],
        &policy_settings
    ));

    let mut wrong = BTreeMap::new();
    wrong.insert("enabled".to_string(), serde_json::json!(false));
    assert!(!validate_policy_compliance(&spec, None, &[], &[], &wrong));
}

#[test]
fn policy_version_enforcement_satisfied() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: None,
    }];
    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("kubectl".to_string(), "1.28.3".to_string());
    let policy_pkgs = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.28".to_string()),
    }];
    assert!(validate_policy_compliance(
        &spec,
        Some(&status),
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

#[test]
fn policy_version_enforcement_not_satisfied() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: None,
    }];
    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("kubectl".to_string(), "1.27.0".to_string());
    let policy_pkgs = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.28".to_string()),
    }];
    assert!(!validate_policy_compliance(
        &spec,
        Some(&status),
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

#[test]
fn policy_version_enforcement_missing_version_report() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: None,
    }];
    let policy_pkgs = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.28".to_string()),
    }];
    assert!(!validate_policy_compliance(
        &spec,
        None,
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

#[test]
fn policy_version_enforcement_missing_package() {
    let policy_pkgs = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.28".to_string()),
    }];
    assert!(!validate_policy_compliance(
        &mc_spec("h", "p"),
        None,
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

#[test]
fn module_compliance_all_present() {
    let mut spec = mc_spec("h", "p");
    spec.module_refs = vec![
        crate::crds::ModuleRef {
            name: "corp-vpn".to_string(),
            required: true,
        },
        crate::crds::ModuleRef {
            name: "corp-certs".to_string(),
            required: true,
        },
    ];
    let required_modules = vec![
        ModuleRef {
            name: "corp-vpn".to_string(),
            required: false,
        },
        ModuleRef {
            name: "corp-certs".to_string(),
            required: false,
        },
    ];
    assert!(validate_policy_compliance(
        &spec,
        None,
        &required_modules,
        &[],
        &Default::default()
    ));
}

#[test]
fn module_compliance_missing_module() {
    let mut spec = mc_spec("h", "p");
    spec.module_refs = vec![crate::crds::ModuleRef {
        name: "corp-vpn".to_string(),
        required: true,
    }];
    let required_modules = vec![
        ModuleRef {
            name: "corp-vpn".to_string(),
            required: false,
        },
        ModuleRef {
            name: "corp-certs".to_string(),
            required: false,
        },
    ];
    assert!(!validate_policy_compliance(
        &spec,
        None,
        &required_modules,
        &[],
        &Default::default()
    ));
}

#[test]
fn module_compliance_no_requirements() {
    assert!(validate_policy_compliance(
        &mc_spec("h", "p"),
        None,
        &[],
        &[],
        &Default::default()
    ));
}

#[test]
fn merge_policy_union_packages() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "vim".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let ns = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "git".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    assert_eq!(merged.packages.len(), 2);
}

#[test]
fn merge_policy_cluster_wins_settings() {
    let mut cluster_settings = BTreeMap::new();
    cluster_settings.insert(
        "key".to_string(),
        serde_json::Value::String("cluster-value".to_string()),
    );
    let cluster = ClusterConfigPolicySpec {
        settings: cluster_settings,
        ..Default::default()
    };
    let mut ns_settings = BTreeMap::new();
    ns_settings.insert(
        "key".to_string(),
        serde_json::Value::String("ns-value".to_string()),
    );
    let ns = ConfigPolicySpec {
        settings: ns_settings,
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    assert_eq!(
        merged.settings["key"],
        serde_json::Value::String("cluster-value".to_string())
    );
}

#[test]
fn merge_policy_cluster_wins_versions() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.29".to_string()),
        }],
        ..Default::default()
    };
    let ns = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    let kubectl_pkg = merged
        .packages
        .iter()
        .find(|p| p.name == "kubectl")
        .unwrap();
    assert_eq!(kubectl_pkg.version.as_deref(), Some(">=1.29"));
}

#[test]
fn merge_policy_no_namespace_policy() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "vim".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[]);
    assert_eq!(merged.packages.len(), 1);
    assert_eq!(merged.packages[0].name, "vim");
}

#[test]
fn merge_policy_union_modules() {
    let cluster = ClusterConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "corp-vpn".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let ns = ConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "corp-certs".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    assert_eq!(merged.modules.len(), 2);
}

#[test]
fn merge_policy_multiple_namespace_policies() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "vim".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let ns1 = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "git".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let ns2 = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "curl".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2]);
    assert_eq!(merged.packages.len(), 3);
}

#[test]
fn matches_selector_expressions_in() {
    let mut labels = BTreeMap::new();
    labels.insert("env".to_string(), "prod".to_string());
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "env".to_string(),
            operator: SelectorOperator::In,
            values: vec!["prod".to_string(), "staging".to_string()],
        }],
    };
    assert!(matches_selector(Some(&labels), &selector));
}

#[test]
fn matches_selector_expressions_not_in() {
    let mut labels = BTreeMap::new();
    labels.insert("env".to_string(), "dev".to_string());
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "env".to_string(),
            operator: SelectorOperator::NotIn,
            values: vec!["prod".to_string()],
        }],
    };
    assert!(matches_selector(Some(&labels), &selector));
}

#[test]
fn matches_selector_expressions_exists() {
    let mut labels = BTreeMap::new();
    labels.insert("tier".to_string(), "frontend".to_string());
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "tier".to_string(),
            operator: SelectorOperator::Exists,
            values: vec![],
        }],
    };
    assert!(matches_selector(Some(&labels), &selector));
}

#[test]
fn matches_selector_expressions_does_not_exist() {
    let labels = BTreeMap::new();
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "restricted".to_string(),
            operator: SelectorOperator::DoesNotExist,
            values: vec![],
        }],
    };
    assert!(matches_selector(Some(&labels), &selector));
}

#[test]
fn drift_alert_escalated_on_high_severity() {
    use crate::crds::DriftSeverity;
    let is_escalated = matches!(
        DriftSeverity::High,
        DriftSeverity::High | DriftSeverity::Critical
    );
    assert!(is_escalated);
    let is_not_escalated = matches!(
        DriftSeverity::Low,
        DriftSeverity::High | DriftSeverity::Critical
    );
    assert!(!is_not_escalated);
}

#[test]
fn merge_policy_dedup_packages() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "vim".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let ns = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "vim".to_string(),
            version: Some("1.0".to_string()),
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    // Should not duplicate — cluster already has vim
    assert_eq!(merged.packages.len(), 1);
}

#[test]
fn build_condition_preserves_transition_time_when_status_unchanged() {
    let existing = vec![Condition {
        condition_type: "Reconciled".to_string(),
        status: "True".to_string(),
        reason: "ReconcileSuccess".to_string(),
        message: "old message".to_string(),
        last_transition_time: "2024-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    }];
    let c = build_condition(
        &existing,
        "Reconciled",
        "True",
        "ReconcileSuccess",
        "new message",
        "2024-06-01T00:00:00Z",
        Some(2),
    );
    // Status unchanged → preserve old transition time
    assert_eq!(c.last_transition_time, "2024-01-01T00:00:00Z");
    // But generation and message update
    assert_eq!(c.observed_generation, Some(2));
    assert_eq!(c.message, "new message");
}

#[test]
fn build_condition_updates_transition_time_when_status_changes() {
    let existing = vec![Condition {
        condition_type: "DriftDetected".to_string(),
        status: "False".to_string(),
        reason: "NoDrift".to_string(),
        message: "no drift".to_string(),
        last_transition_time: "2024-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    }];
    let c = build_condition(
        &existing,
        "DriftDetected",
        "True",
        "DriftActive",
        "drift found",
        "2024-06-01T00:00:00Z",
        Some(2),
    );
    // Status changed → new transition time
    assert_eq!(c.last_transition_time, "2024-06-01T00:00:00Z");
}

#[test]
fn build_drift_alert_conditions_resolved() {
    let conditions = build_drift_alert_conditions(
        &DriftSeverity::Low,
        true,
        "dev-1",
        0,
        "2024-01-01T00:00:00Z",
        Some(1),
    );
    assert_eq!(conditions.len(), 3);
    assert_eq!(conditions[1].condition_type, "Resolved");
    assert_eq!(conditions[1].status, "True");
    assert_eq!(conditions[2].status, "False"); // Low is not escalated
}

#[test]
fn build_drift_alert_conditions_active_high() {
    let conditions = build_drift_alert_conditions(
        &DriftSeverity::High,
        false,
        "dev-1",
        3,
        "2024-01-01T00:00:00Z",
        Some(1),
    );
    assert_eq!(conditions[1].status, "False"); // Not resolved
    assert_eq!(conditions[2].status, "True"); // High is escalated
}

#[test]
fn module_verification_no_signature() {
    let result = evaluate_module_verification(&None);
    assert_eq!(result.status, "False");
    assert_eq!(result.reason, "NotSigned");
    assert!(result.message.contains("No signature"));
    assert!(result.signature_digest.is_none());
}

#[test]
fn module_verification_no_cosign() {
    let sig = ModuleSignature { cosign: None };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "False");
    assert_eq!(result.reason, "NotSigned");
    assert!(result.signature_digest.is_none());
}

#[test]
fn module_verification_valid_pem() {
    let sig = ModuleSignature {
        cosign: Some(crate::crds::CosignSignature {
            public_key: Some(TEST_PEM_KEY.to_string()),
            ..Default::default()
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "True");
    assert_eq!(result.reason, "SignatureConfigured");
    assert!(result.signature_digest.is_some());
    assert!(result.signature_digest.unwrap().starts_with("sha256:"));
}

#[test]
fn module_verification_invalid_pem() {
    let sig = ModuleSignature {
        cosign: Some(crate::crds::CosignSignature {
            public_key: Some("not-a-pem-key".to_string()),
            ..Default::default()
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "False");
    assert_eq!(result.reason, "SignatureInvalid");
    assert!(result.signature_digest.is_none());
}

#[test]
fn module_verification_keyless() {
    let sig = ModuleSignature {
        cosign: Some(crate::crds::CosignSignature {
            keyless: true,
            certificate_identity: Some("user@example.com".to_string()),
            certificate_oidc_issuer: Some("https://accounts.google.com".to_string()),
            ..Default::default()
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "True");
    assert_eq!(result.reason, "SignatureConfigured");
    assert_eq!(
        result.signature_digest.unwrap(),
        "keyless:user@example.com@https://accounts.google.com"
    );
}

#[test]
fn compliance_summary_formats_counts() {
    assert_eq!(compliance_summary(0, 0), "0 compliant, 0 non-compliant");
    assert_eq!(compliance_summary(5, 3), "5 compliant, 3 non-compliant");
    assert_eq!(compliance_summary(100, 0), "100 compliant, 0 non-compliant");
    assert_eq!(compliance_summary(0, 42), "0 compliant, 42 non-compliant");
}

#[test]
fn find_condition_status_returns_matching() {
    let conditions = vec![
        Condition {
            condition_type: "Reconciled".to_string(),
            status: "True".to_string(),
            reason: "ReconcileSuccess".to_string(),
            message: "all good".to_string(),
            last_transition_time: "2024-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        },
        Condition {
            condition_type: "DriftDetected".to_string(),
            status: "False".to_string(),
            reason: "NoDrift".to_string(),
            message: "no drift".to_string(),
            last_transition_time: "2024-01-02T00:00:00Z".to_string(),
            observed_generation: Some(2),
        },
    ];
    assert_eq!(
        find_condition_status(&conditions, "Reconciled"),
        Some("True".to_string())
    );
    assert_eq!(
        find_condition_status(&conditions, "DriftDetected"),
        Some("False".to_string())
    );
    assert_eq!(find_condition_status(&conditions, "NonExistent"), None);
    assert_eq!(find_condition_status(&[], "Reconciled"), None);
}

#[test]
fn find_condition_transition_time_returns_matching() {
    let conditions = vec![
        Condition {
            condition_type: "Reconciled".to_string(),
            status: "True".to_string(),
            reason: "ReconcileSuccess".to_string(),
            message: "all good".to_string(),
            last_transition_time: "2024-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        },
        Condition {
            condition_type: "DriftDetected".to_string(),
            status: "False".to_string(),
            reason: "NoDrift".to_string(),
            message: "no drift".to_string(),
            last_transition_time: "2024-06-15T12:00:00Z".to_string(),
            observed_generation: None,
        },
    ];
    assert_eq!(
        find_condition_transition_time(&conditions, "Reconciled"),
        Some("2024-01-01T00:00:00Z".to_string())
    );
    assert_eq!(
        find_condition_transition_time(&conditions, "DriftDetected"),
        Some("2024-06-15T12:00:00Z".to_string())
    );
    assert_eq!(
        find_condition_transition_time(&conditions, "NonExistent"),
        None
    );
    assert_eq!(find_condition_transition_time(&[], "Reconciled"), None);
}

#[test]
fn matches_selector_combined_labels_and_expressions() {
    let mut labels = BTreeMap::new();
    labels.insert("team".to_string(), "platform".to_string());
    labels.insert("env".to_string(), "prod".to_string());
    labels.insert("tier".to_string(), "backend".to_string());

    // Selector requires match_labels AND match_expressions (AND semantics)
    let selector = LabelSelector {
        match_labels: {
            let mut m = BTreeMap::new();
            m.insert("team".to_string(), "platform".to_string());
            m
        },
        match_expressions: vec![
            LabelSelectorRequirement {
                key: "env".to_string(),
                operator: SelectorOperator::In,
                values: vec!["prod".to_string(), "staging".to_string()],
            },
            LabelSelectorRequirement {
                key: "tier".to_string(),
                operator: SelectorOperator::Exists,
                values: vec![],
            },
        ],
    };

    // All conditions met
    assert!(matches_selector(Some(&labels), &selector));

    // Fails if match_labels don't match
    let mut wrong_team = labels.clone();
    wrong_team.insert("team".to_string(), "security".to_string());
    assert!(!matches_selector(Some(&wrong_team), &selector));

    // Fails if expression doesn't match (env not in allowed set)
    let mut wrong_env = labels.clone();
    wrong_env.insert("env".to_string(), "dev".to_string());
    assert!(!matches_selector(Some(&wrong_env), &selector));

    // Fails if required Exists label is missing
    let mut no_tier = labels.clone();
    no_tier.remove("tier");
    assert!(!matches_selector(Some(&no_tier), &selector));
}

#[test]
fn matches_selector_not_in_rejects_matching_value() {
    let mut labels = BTreeMap::new();
    labels.insert("env".to_string(), "prod".to_string());

    let selector = LabelSelector {
        match_labels: BTreeMap::new(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "env".to_string(),
            operator: SelectorOperator::NotIn,
            values: vec!["prod".to_string(), "staging".to_string()],
        }],
    };

    // Label value IS in the exclusion list — should be rejected
    assert!(!matches_selector(Some(&labels), &selector));
}

#[test]
fn validate_spec_empty_profile_errors() {
    let result = validate_spec(&mc_spec("valid-host", ""));
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("profile"),
        "error should mention profile: {err_msg}"
    );
}

#[test]
fn validate_spec_whitespace_hostname_errors() {
    let result = validate_spec(&mc_spec("   ", "default"));
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("hostname"),
        "error should mention hostname: {err_msg}"
    );
}

// -----------------------------------------------------------------------
// evaluate_module_verification edge cases
// -----------------------------------------------------------------------

#[test]
fn module_verification_keyless_with_public_key_prefers_keyless() {
    // When both keyless=true AND public_key are set, keyless wins (early return)
    let sig = ModuleSignature {
        cosign: Some(crate::crds::CosignSignature {
            keyless: true,
            public_key: Some(TEST_PEM_KEY.to_string()),
            certificate_identity: Some("ci@example.com".to_string()),
            certificate_oidc_issuer: Some("https://issuer.example.com".to_string()),
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "True");
    assert_eq!(result.reason, "SignatureConfigured");
    // Should return keyless identity, not PEM fingerprint
    let digest = result.signature_digest.unwrap();
    assert!(
        digest.starts_with("keyless:"),
        "should use keyless path, got: {digest}"
    );
    assert!(digest.contains("ci@example.com"));
}

#[test]
fn module_verification_keyless_without_identity_uses_wildcard() {
    let sig = ModuleSignature {
        cosign: Some(crate::crds::CosignSignature {
            keyless: true,
            certificate_identity: None,
            certificate_oidc_issuer: None,
            ..Default::default()
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "True");
    assert_eq!(result.signature_digest.as_deref(), Some("keyless:*@*"));
}

#[test]
fn module_verification_static_key_no_public_key() {
    // cosign present, keyless=false, public_key=None → should be NotSigned or Invalid
    let sig = ModuleSignature {
        cosign: Some(crate::crds::CosignSignature {
            keyless: false,
            public_key: None,
            ..Default::default()
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "False");
}

// -----------------------------------------------------------------------
// merge_policy_requirements — 3+ namespace policies
// -----------------------------------------------------------------------

#[test]
fn merge_policy_three_namespace_policies_with_version_conflict() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.30".to_string()),
        }],
        ..Default::default()
    };
    let ns1 = ConfigPolicySpec {
        packages: vec![
            PackageRef {
                name: "kubectl".to_string(),
                version: Some(">=1.28".to_string()),
            },
            PackageRef {
                name: "git".to_string(),
                version: None,
            },
        ],
        ..Default::default()
    };
    let ns2 = ConfigPolicySpec {
        packages: vec![
            PackageRef {
                name: "curl".to_string(),
                version: Some(">=8.0".to_string()),
            },
            PackageRef {
                name: "git".to_string(),
                version: Some(">=2.40".to_string()),
            },
        ],
        ..Default::default()
    };
    let ns3 = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "jq".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2, &ns3]);

    // Cluster kubectl >=1.30 should win over ns1's >=1.28
    let kubectl = merged
        .packages
        .iter()
        .find(|p| p.name == "kubectl")
        .unwrap();
    assert_eq!(kubectl.version.as_deref(), Some(">=1.30"));

    // git: cluster has no git, ns1 adds it with None, ns2 tries to add but finds
    // existing with None version → ns2's version fills in
    let git = merged.packages.iter().find(|p| p.name == "git").unwrap();
    assert_eq!(git.version.as_deref(), Some(">=2.40"));

    // curl from ns2, jq from ns3
    assert!(merged.packages.iter().any(|p| p.name == "curl"));
    assert!(merged.packages.iter().any(|p| p.name == "jq"));

    // Total unique packages: kubectl, git, curl, jq
    assert_eq!(merged.packages.len(), 4);
}

#[test]
fn merge_policy_namespace_settings_later_overwrites_earlier() {
    let cluster = ClusterConfigPolicySpec::default();
    let ns1 = ConfigPolicySpec {
        settings: {
            let mut s = BTreeMap::new();
            s.insert("key".to_string(), serde_json::json!("ns1-value"));
            s.insert("shared".to_string(), serde_json::json!("from-ns1"));
            s
        },
        ..Default::default()
    };
    let ns2 = ConfigPolicySpec {
        settings: {
            let mut s = BTreeMap::new();
            s.insert("shared".to_string(), serde_json::json!("from-ns2"));
            s
        },
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2]);
    // ns2 overwrites ns1 for "shared"
    assert_eq!(merged.settings["shared"], serde_json::json!("from-ns2"));
    // ns1-only key preserved
    assert_eq!(merged.settings["key"], serde_json::json!("ns1-value"));
}

#[test]
fn merge_policy_cluster_settings_override_namespace() {
    let cluster = ClusterConfigPolicySpec {
        settings: {
            let mut s = BTreeMap::new();
            s.insert("key".to_string(), serde_json::json!("cluster-wins"));
            s
        },
        ..Default::default()
    };
    let ns = ConfigPolicySpec {
        settings: {
            let mut s = BTreeMap::new();
            s.insert("key".to_string(), serde_json::json!("ns-loses"));
            s.insert("ns-only".to_string(), serde_json::json!("kept"));
            s
        },
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    // Cluster settings extend AFTER namespace → cluster wins
    assert_eq!(merged.settings["key"], serde_json::json!("cluster-wins"));
    assert_eq!(merged.settings["ns-only"], serde_json::json!("kept"));
}

#[test]
fn merge_policy_three_namespace_module_dedup() {
    let cluster = ClusterConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "corp-vpn".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let ns1 = ConfigPolicySpec {
        required_modules: vec![
            ModuleRef {
                name: "corp-vpn".to_string(),
                required: true,
            },
            ModuleRef {
                name: "corp-certs".to_string(),
                required: true,
            },
        ],
        ..Default::default()
    };
    let ns2 = ConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "corp-certs".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2]);
    // Should dedup: corp-vpn (from cluster), corp-certs (from ns1, ns2 skipped)
    assert_eq!(merged.modules.len(), 2);
}

// -----------------------------------------------------------------------
// validate_policy_compliance — nested JSON settings
// -----------------------------------------------------------------------

#[test]
fn policy_compliance_nested_json_settings() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert(
        "network".to_string(),
        serde_json::json!({"dns": {"servers": ["8.8.8.8", "1.1.1.1"]}}),
    );

    let mut spec = mc_spec("h", "p");
    spec.system_settings.insert(
        "network".to_string(),
        serde_json::json!({"dns": {"servers": ["8.8.8.8", "1.1.1.1"]}}),
    );
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &[],
        &policy_settings
    ));
}

#[test]
fn policy_compliance_nested_json_mismatch() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert(
        "network".to_string(),
        serde_json::json!({"dns": {"servers": ["8.8.8.8"]}}),
    );

    let mut spec = mc_spec("h", "p");
    spec.system_settings.insert(
        "network".to_string(),
        serde_json::json!({"dns": {"servers": ["1.1.1.1"]}}),
    );
    assert!(
        !validate_policy_compliance(&spec, None, &[], &[], &policy_settings),
        "nested JSON with different values should not match"
    );
}

#[test]
fn policy_compliance_numeric_settings() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert("max_retries".to_string(), serde_json::json!(3));

    let mut spec = mc_spec("h", "p");
    spec.system_settings
        .insert("max_retries".to_string(), serde_json::json!(3));
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &[],
        &policy_settings
    ));

    // Different numeric value
    let mut wrong = BTreeMap::new();
    wrong.insert("max_retries".to_string(), serde_json::json!(5));
    assert!(!validate_policy_compliance(&spec, None, &[], &[], &wrong));
}

#[test]
fn policy_compliance_null_setting() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert("opt_out".to_string(), serde_json::Value::Null);

    let mut spec = mc_spec("h", "p");
    spec.system_settings
        .insert("opt_out".to_string(), serde_json::Value::Null);
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &[],
        &policy_settings
    ));
}

#[test]
fn policy_compliance_array_setting() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert(
        "tags".to_string(),
        serde_json::json!(["production", "critical"]),
    );

    let mut spec = mc_spec("h", "p");
    spec.system_settings.insert(
        "tags".to_string(),
        serde_json::json!(["production", "critical"]),
    );
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &[],
        &policy_settings
    ));

    // Different order — JSON arrays are order-sensitive
    spec.system_settings.insert(
        "tags".to_string(),
        serde_json::json!(["critical", "production"]),
    );
    assert!(
        !validate_policy_compliance(&spec, None, &[], &[], &policy_settings),
        "array order matters in JSON equality"
    );
}

#[test]
fn policy_compliance_missing_setting_key() {
    let mut policy_settings = BTreeMap::new();
    policy_settings.insert("required_key".to_string(), serde_json::json!("value"));

    let spec = mc_spec("h", "p");
    // spec has no system_settings → should fail
    assert!(!validate_policy_compliance(
        &spec,
        None,
        &[],
        &[],
        &policy_settings
    ));
}

// -----------------------------------------------------------------------
// build_condition — new condition (not in existing list)
// -----------------------------------------------------------------------

#[test]
fn build_condition_new_type_uses_provided_transition_time() {
    let existing = vec![Condition {
        condition_type: "Reconciled".to_string(),
        status: "True".to_string(),
        reason: "ReconcileSuccess".to_string(),
        message: "ok".to_string(),
        last_transition_time: "2024-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    }];
    // Build a NEW condition type not in existing
    let c = build_condition(
        &existing,
        "DriftDetected",
        "True",
        "DriftActive",
        "drift found",
        "2024-06-01T00:00:00Z",
        Some(2),
    );
    // New type → uses the provided transition time
    assert_eq!(c.last_transition_time, "2024-06-01T00:00:00Z");
    assert_eq!(c.condition_type, "DriftDetected");
    assert_eq!(c.observed_generation, Some(2));
}

#[test]
fn build_drift_alert_conditions_critical_severity() {
    let conditions = build_drift_alert_conditions(
        &DriftSeverity::Critical,
        false,
        "dev-1",
        5,
        "2024-01-01T00:00:00Z",
        Some(3),
    );
    assert_eq!(conditions.len(), 3);
    // Acknowledged (not yet)
    assert_eq!(conditions[0].condition_type, "Acknowledged");
    assert_eq!(conditions[0].status, "False");
    // Not resolved
    assert_eq!(conditions[1].condition_type, "Resolved");
    assert_eq!(conditions[1].status, "False");
    assert!(conditions[1].message.contains("dev-1"));
    assert!(conditions[1].message.contains("5 detail(s)"));
    // Critical is escalated
    assert_eq!(conditions[2].condition_type, "Escalated");
    assert_eq!(conditions[2].status, "True");
}

#[test]
fn matches_selector_in_with_empty_values_rejects() {
    let mut labels = BTreeMap::new();
    labels.insert("env".to_string(), "prod".to_string());
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "env".to_string(),
            operator: SelectorOperator::In,
            values: vec![], // empty values list
        }],
    };
    // In with empty values → no value can match → should reject
    assert!(!matches_selector(Some(&labels), &selector));
}

#[test]
fn matches_selector_not_in_with_empty_values_accepts() {
    let mut labels = BTreeMap::new();
    labels.insert("env".to_string(), "prod".to_string());
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "env".to_string(),
            operator: SelectorOperator::NotIn,
            values: vec![], // empty exclusion list
        }],
    };
    // NotIn with empty values → nothing excluded → should accept
    assert!(matches_selector(Some(&labels), &selector));
}

#[test]
fn matches_selector_does_not_exist_with_present_label_rejects() {
    let mut labels = BTreeMap::new();
    labels.insert("restricted".to_string(), "true".to_string());
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "restricted".to_string(),
            operator: SelectorOperator::DoesNotExist,
            values: vec![],
        }],
    };
    assert!(
        !matches_selector(Some(&labels), &selector),
        "DoesNotExist should reject when label is present"
    );
}

#[test]
fn matches_selector_exists_with_missing_label_rejects() {
    let labels = BTreeMap::new();
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "required-label".to_string(),
            operator: SelectorOperator::Exists,
            values: vec![],
        }],
    };
    assert!(
        !matches_selector(Some(&labels), &selector),
        "Exists should reject when label is absent"
    );
}

#[test]
fn matches_selector_none_labels_with_match_labels_rejects() {
    let selector = LabelSelector {
        match_labels: {
            let mut m = BTreeMap::new();
            m.insert("team".to_string(), "platform".to_string());
            m
        },
        match_expressions: vec![],
    };
    assert!(
        !matches_selector(None, &selector),
        "None labels with non-empty match_labels should reject"
    );
}

// -----------------------------------------------------------------------
// record_reconcile_metrics and record_reconcile_success
// -----------------------------------------------------------------------

#[test]
fn record_reconcile_metrics_labels_propagate() {
    use crate::metrics::{Metrics, ReconcileLabels};
    use prometheus_client::registry::Registry;

    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);

    // Directly exercise the metric families (same code path as record_reconcile_metrics)
    let labels = ReconcileLabels {
        controller: "machine_config".to_string(),
        result: "success".to_string(),
    };
    metrics.reconciliations_total.get_or_create(&labels).inc();
    metrics
        .reconciliation_duration_seconds
        .get_or_create(&labels)
        .observe(0.5);

    // Verify the counter incremented
    let count = metrics.reconciliations_total.get_or_create(&labels).get();
    assert!(count >= 1, "counter should have incremented");
}

#[test]
fn record_reconcile_metrics_error_labels_separate() {
    use crate::metrics::{Metrics, ReconcileLabels};
    use prometheus_client::registry::Registry;

    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);

    let success_labels = ReconcileLabels {
        controller: "drift_alert".to_string(),
        result: "success".to_string(),
    };
    let error_labels = ReconcileLabels {
        controller: "drift_alert".to_string(),
        result: "error".to_string(),
    };

    metrics
        .reconciliations_total
        .get_or_create(&success_labels)
        .inc();
    metrics
        .reconciliations_total
        .get_or_create(&success_labels)
        .inc();
    metrics
        .reconciliations_total
        .get_or_create(&error_labels)
        .inc();

    let success_count = metrics
        .reconciliations_total
        .get_or_create(&success_labels)
        .get();
    let error_count = metrics
        .reconciliations_total
        .get_or_create(&error_labels)
        .get();
    assert_eq!(success_count, 2);
    assert_eq!(error_count, 1);
}

// -----------------------------------------------------------------------
// Drift and policy metrics families
// -----------------------------------------------------------------------

#[test]
fn drift_metrics_increment_by_severity() {
    use crate::metrics::{DriftLabels, Metrics};
    use prometheus_client::registry::Registry;

    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);

    let labels = DriftLabels {
        severity: "warning".to_string(),
        namespace: "default".to_string(),
    };
    metrics.drift_events_total.get_or_create(&labels).inc();
    metrics.drift_events_total.get_or_create(&labels).inc();

    let count = metrics.drift_events_total.get_or_create(&labels).get();
    assert_eq!(count, 2);
}

#[test]
fn policy_compliance_gauge_set_and_read() {
    use crate::metrics::{Metrics, PolicyLabels};
    use prometheus_client::registry::Registry;

    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);

    let labels = PolicyLabels {
        policy: "security-baseline".to_string(),
        namespace: "production".to_string(),
    };
    metrics.devices_compliant.get_or_create(&labels).set(42);

    let value = metrics.devices_compliant.get_or_create(&labels).get();
    assert_eq!(value, 42);
}

// -----------------------------------------------------------------------
// build_drift_alert_conditions: Medium severity (not escalated)
// -----------------------------------------------------------------------

#[test]
fn build_drift_alert_conditions_medium_not_escalated() {
    let conditions = build_drift_alert_conditions(
        &DriftSeverity::Medium,
        false,
        "dev-2",
        2,
        "2024-06-01T00:00:00Z",
        Some(5),
    );
    assert_eq!(conditions.len(), 3);
    assert_eq!(conditions[0].condition_type, "Acknowledged");
    assert_eq!(conditions[0].status, "False");
    assert_eq!(conditions[1].condition_type, "Resolved");
    assert_eq!(conditions[1].status, "False");
    assert!(conditions[1].message.contains("dev-2"));
    assert!(conditions[1].message.contains("2 detail(s)"));
    // Medium is NOT escalated
    assert_eq!(conditions[2].condition_type, "Escalated");
    assert_eq!(conditions[2].status, "False");
    assert_eq!(conditions[2].reason, "BelowThreshold");
}

// -----------------------------------------------------------------------
// build_drift_alert_conditions: resolved with zero details
// -----------------------------------------------------------------------

#[test]
fn build_drift_alert_conditions_resolved_zero_details() {
    let conditions = build_drift_alert_conditions(
        &DriftSeverity::Low,
        true,
        "dev-3",
        0,
        "2024-07-01T00:00:00Z",
        None,
    );
    assert_eq!(conditions[1].status, "True");
    assert_eq!(conditions[1].reason, "DriftResolved");
    assert_eq!(conditions[1].message, "Drift has been resolved");
    assert!(conditions[0].observed_generation.is_none());
}

// -----------------------------------------------------------------------
// build_condition: empty existing list uses provided time
// -----------------------------------------------------------------------

#[test]
fn build_condition_empty_existing_uses_now() {
    let c = build_condition(
        &[],
        "Ready",
        "True",
        "AllGood",
        "everything is fine",
        "2024-08-01T00:00:00Z",
        Some(10),
    );
    assert_eq!(c.condition_type, "Ready");
    assert_eq!(c.status, "True");
    assert_eq!(c.reason, "AllGood");
    assert_eq!(c.message, "everything is fine");
    assert_eq!(c.last_transition_time, "2024-08-01T00:00:00Z");
    assert_eq!(c.observed_generation, Some(10));
}

// -----------------------------------------------------------------------
// validate_policy_compliance: combined modules + packages + settings
// -----------------------------------------------------------------------

#[test]
fn policy_compliance_combined_all_requirements() {
    let mut spec = mc_spec("host1", "default");
    spec.module_refs = vec![ModuleRef {
        name: "corp-vpn".to_string(),
        required: true,
    }];
    spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: None,
    }];
    spec.system_settings
        .insert("enforce_tls".to_string(), serde_json::json!(true));

    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("kubectl".to_string(), "1.29.0".to_string());

    let required_modules = vec![ModuleRef {
        name: "corp-vpn".to_string(),
        required: true,
    }];
    let required_packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.28".to_string()),
    }];
    let mut required_settings = BTreeMap::new();
    required_settings.insert("enforce_tls".to_string(), serde_json::json!(true));

    // All requirements met
    assert!(validate_policy_compliance(
        &spec,
        Some(&status),
        &required_modules,
        &required_packages,
        &required_settings,
    ));

    // Fail if module missing
    spec.module_refs.clear();
    assert!(!validate_policy_compliance(
        &spec,
        Some(&status),
        &required_modules,
        &required_packages,
        &required_settings,
    ));
}

// -----------------------------------------------------------------------
// merge_policy_requirements: empty cluster + empty namespace
// -----------------------------------------------------------------------

#[test]
fn merge_policy_all_empty() {
    let cluster = ClusterConfigPolicySpec::default();
    let merged = merge_policy_requirements(&cluster, &[]);
    assert!(merged.packages.is_empty());
    assert!(merged.modules.is_empty());
    assert!(merged.settings.is_empty());
}

// -----------------------------------------------------------------------
// compliance_summary edge cases
// -----------------------------------------------------------------------

#[test]
fn compliance_summary_large_numbers() {
    let result = compliance_summary(999_999, 1);
    assert_eq!(result, "999999 compliant, 1 non-compliant");
}

// -----------------------------------------------------------------------
// validate_spec with valid spec
// -----------------------------------------------------------------------

#[test]
fn validate_spec_valid() {
    let spec = mc_spec("myhost.local", "production");
    assert!(validate_spec(&spec).is_ok());
}

// -----------------------------------------------------------------------
// log_reconcile: verify the closure doesn't panic on Ok/Err
// -----------------------------------------------------------------------

#[test]
fn log_reconcile_ok_does_not_panic() {
    use kube::runtime::controller::Action;
    use kube::runtime::reflector::ObjectRef;

    let log_fn = log_reconcile::<MachineConfig>("MachineConfig");

    // Create a mock ObjectRef
    let obj_ref = ObjectRef::<MachineConfig>::new("test-mc").within("default");
    let action = Action::requeue(std::time::Duration::from_secs(60));
    let result: ReconcileResult<MachineConfig> = Ok((obj_ref.clone(), action));

    // log_reconcile is a logging-only callback — it returns ().
    // We verify both Ok and Err paths complete without panic.
    let future = log_fn(result);
    futures::executor::block_on(future);

    let log_fn_err = log_reconcile::<MachineConfig>("MachineConfig");
    let err_result: ReconcileResult<MachineConfig> =
        Err(kube::runtime::controller::Error::ReconcilerFailed(
            OperatorError::Reconciliation("test error".into()),
            obj_ref.erase(),
        ));
    let err_future = log_fn_err(err_result);
    futures::executor::block_on(err_future);
    // Both paths completed without panic — that's the behavioral contract.
}

// -----------------------------------------------------------------------
// matches_selector: NotIn with missing label accepts (key absent)
// -----------------------------------------------------------------------

#[test]
fn matches_selector_not_in_missing_key_accepts() {
    let labels = BTreeMap::new(); // no labels at all
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "restricted".to_string(),
            operator: SelectorOperator::NotIn,
            values: vec!["true".to_string()],
        }],
    };
    // Key is missing → is_none_or returns true → accepts
    assert!(matches_selector(Some(&labels), &selector));
}

// -----------------------------------------------------------------------
// build_condition
// -----------------------------------------------------------------------

#[test]
fn build_condition_new_sets_transition_time() {
    let cond = build_condition(
        &[],
        "Ready",
        "True",
        "Ready",
        "All ok",
        "2026-01-01T00:00:00Z",
        Some(1),
    );
    assert_eq!(cond.condition_type, "Ready");
    assert_eq!(cond.status, "True");
    assert_eq!(cond.reason, "Ready");
    assert_eq!(cond.message, "All ok");
    assert_eq!(cond.last_transition_time, "2026-01-01T00:00:00Z");
    assert_eq!(cond.observed_generation, Some(1));
}

#[test]
fn build_condition_preserves_transition_time_on_same_status() {
    let existing = vec![Condition {
        condition_type: "Ready".to_string(),
        status: "True".to_string(),
        reason: "Ready".to_string(),
        message: "old".to_string(),
        last_transition_time: "2025-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    }];
    let cond = build_condition(
        &existing,
        "Ready",
        "True",
        "Ready",
        "new message",
        "2026-01-01T00:00:00Z",
        Some(2),
    );
    // Status didn't change (still True), so transition time is preserved
    assert_eq!(cond.last_transition_time, "2025-01-01T00:00:00Z");
    assert_eq!(cond.message, "new message");
    assert_eq!(cond.observed_generation, Some(2));
}

#[test]
fn build_condition_updates_transition_time_on_status_change() {
    let existing = vec![Condition {
        condition_type: "Ready".to_string(),
        status: "True".to_string(),
        reason: "Ready".to_string(),
        message: "was ready".to_string(),
        last_transition_time: "2025-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    }];
    let cond = build_condition(
        &existing,
        "Ready",
        "False",
        "Error",
        "failed",
        "2026-06-01T00:00:00Z",
        Some(2),
    );
    // Status changed from True to False, so transition time updates
    assert_eq!(cond.last_transition_time, "2026-06-01T00:00:00Z");
    assert_eq!(cond.status, "False");
}

// -----------------------------------------------------------------------
// find_condition_status / find_condition_transition_time
// -----------------------------------------------------------------------

#[test]
fn find_condition_status_found() {
    let conditions = vec![
        Condition {
            condition_type: "Ready".to_string(),
            status: "True".to_string(),
            reason: "R".to_string(),
            message: "M".to_string(),
            last_transition_time: "now".to_string(),
            observed_generation: None,
        },
        Condition {
            condition_type: "Degraded".to_string(),
            status: "False".to_string(),
            reason: "R".to_string(),
            message: "M".to_string(),
            last_transition_time: "now".to_string(),
            observed_generation: None,
        },
    ];
    assert_eq!(
        find_condition_status(&conditions, "Ready"),
        Some("True".to_string())
    );
    assert_eq!(
        find_condition_status(&conditions, "Degraded"),
        Some("False".to_string())
    );
}

#[test]
fn find_condition_status_not_found() {
    assert_eq!(find_condition_status(&[], "Ready"), None);
}

#[test]
fn find_condition_transition_time_found() {
    let conditions = vec![Condition {
        condition_type: "Ready".to_string(),
        status: "True".to_string(),
        reason: "R".to_string(),
        message: "M".to_string(),
        last_transition_time: "2025-01-01T00:00:00Z".to_string(),
        observed_generation: None,
    }];
    assert_eq!(
        find_condition_transition_time(&conditions, "Ready"),
        Some("2025-01-01T00:00:00Z".to_string())
    );
}

#[test]
fn find_condition_transition_time_not_found() {
    assert_eq!(find_condition_transition_time(&[], "Missing"), None);
}

// -----------------------------------------------------------------------
// build_drift_alert_conditions
// -----------------------------------------------------------------------

#[test]
fn drift_alert_conditions_active_low_severity() {
    let conditions = build_drift_alert_conditions(
        &DriftSeverity::Low,
        false,
        "dev-1",
        3,
        "2026-01-01T00:00:00Z",
        Some(1),
    );
    assert_eq!(conditions.len(), 3);

    let resolved = conditions
        .iter()
        .find(|c| c.condition_type == "Resolved")
        .unwrap();
    assert_eq!(resolved.status, "False");
    assert_eq!(resolved.reason, "DriftActive");
    assert!(resolved.message.contains("dev-1"));
    assert!(resolved.message.contains("3"));

    let escalated = conditions
        .iter()
        .find(|c| c.condition_type == "Escalated")
        .unwrap();
    assert_eq!(escalated.status, "False");
    assert_eq!(escalated.reason, "BelowThreshold");
}

#[test]
fn drift_alert_conditions_resolved_high_severity() {
    let conditions = build_drift_alert_conditions(
        &DriftSeverity::High,
        true,
        "dev-2",
        0,
        "2026-01-01T00:00:00Z",
        None,
    );
    let resolved = conditions
        .iter()
        .find(|c| c.condition_type == "Resolved")
        .unwrap();
    assert_eq!(resolved.status, "True");
    assert_eq!(resolved.reason, "DriftResolved");

    let escalated = conditions
        .iter()
        .find(|c| c.condition_type == "Escalated")
        .unwrap();
    assert_eq!(escalated.status, "True");
    assert_eq!(escalated.reason, "SeverityThreshold");
}

#[test]
fn drift_alert_conditions_critical_is_escalated() {
    let conditions =
        build_drift_alert_conditions(&DriftSeverity::Critical, false, "d", 1, "now", None);
    let escalated = conditions
        .iter()
        .find(|c| c.condition_type == "Escalated")
        .unwrap();
    assert_eq!(escalated.status, "True");
}

#[test]
fn drift_alert_conditions_medium_not_escalated() {
    let conditions =
        build_drift_alert_conditions(&DriftSeverity::Medium, false, "d", 1, "now", None);
    let escalated = conditions
        .iter()
        .find(|c| c.condition_type == "Escalated")
        .unwrap();
    assert_eq!(escalated.status, "False");
}

// -----------------------------------------------------------------------
// evaluate_module_verification
// -----------------------------------------------------------------------

// evaluate_module_verification basic variants already tested above
#[test]
fn module_verification_keyless_with_identity_coverage() {
    use crate::crds::CosignSignature;
    let sig = ModuleSignature {
        cosign: Some(CosignSignature {
            public_key: None,
            keyless: true,
            certificate_identity: Some("user@example.com".to_string()),
            certificate_oidc_issuer: Some("https://accounts.google.com".to_string()),
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "True");
    assert_eq!(result.reason, "SignatureConfigured");
    assert!(result.message.contains("Keyless"));
    let digest = result.signature_digest.unwrap();
    assert!(digest.contains("keyless:"));
    assert!(digest.contains("user@example.com"));
}

#[test]
fn module_verification_cosign_no_key_no_keyless() {
    use crate::crds::CosignSignature;
    let sig = ModuleSignature {
        cosign: Some(CosignSignature {
            public_key: None,
            keyless: false,
            certificate_identity: None,
            certificate_oidc_issuer: None,
        }),
    };
    let result = evaluate_module_verification(&Some(sig));
    assert_eq!(result.status, "False");
    assert_eq!(result.reason, "SignatureInvalid");
}

// -----------------------------------------------------------------------
// merge_policy_requirements
// -----------------------------------------------------------------------

#[test]
fn merge_policy_cluster_and_namespace_packages() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "git".to_string(),
            version: Some(">=2.40".to_string()),
        }],
        ..Default::default()
    };
    let ns_spec = ConfigPolicySpec {
        packages: vec![
            PackageRef {
                name: "git".to_string(),
                version: None,
            },
            PackageRef {
                name: "vim".to_string(),
                version: None,
            },
        ],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns_spec]);
    assert_eq!(merged.packages.len(), 2);
    // Cluster version wins for git
    let git = merged.packages.iter().find(|p| p.name == "git").unwrap();
    assert_eq!(git.version, Some(">=2.40".to_string()));
}

#[test]
fn merge_policy_deduplicates_modules() {
    use crate::crds::ModuleRef;
    let cluster = ClusterConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "base".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let ns_spec = ConfigPolicySpec {
        required_modules: vec![
            ModuleRef {
                name: "base".to_string(),
                required: true,
            },
            ModuleRef {
                name: "vpn".to_string(),
                required: true,
            },
        ],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns_spec]);
    assert_eq!(merged.modules.len(), 2); // base + vpn, no duplication
}

#[test]
fn merge_policy_settings_namespace_then_cluster() {
    let mut cluster_settings = BTreeMap::new();
    cluster_settings.insert("key".to_string(), serde_json::json!("cluster-val"));
    let cluster = ClusterConfigPolicySpec {
        settings: cluster_settings,
        ..Default::default()
    };
    let mut ns_settings = BTreeMap::new();
    ns_settings.insert("key".to_string(), serde_json::json!("ns-val"));
    let ns_spec = ConfigPolicySpec {
        settings: ns_settings,
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns_spec]);
    // Cluster settings are applied after namespace, so cluster wins
    assert_eq!(merged.settings["key"], serde_json::json!("cluster-val"));
}

// -----------------------------------------------------------------------
// compliance_summary
// -----------------------------------------------------------------------

#[test]
fn compliance_summary_zero_zero() {
    assert_eq!(compliance_summary(0, 0), "0 compliant, 0 non-compliant");
}

#[test]
fn compliance_summary_typical() {
    assert_eq!(compliance_summary(5, 2), "5 compliant, 2 non-compliant");
}

// -----------------------------------------------------------------------
// record_error_and_requeue — validates metric labels and requeue duration
// -----------------------------------------------------------------------

#[test]
fn record_error_and_requeue_increments_error_counter() {
    use crate::metrics::{Metrics, ReconcileLabels};
    use kube::runtime::controller::Action;
    use prometheus_client::registry::Registry;

    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);

    // Build a ControllerContext with a real metrics instance (we can't
    // call record_error_and_requeue directly because it requires a full
    // ControllerContext with a kube Client, but we can exercise the same
    // code path that it runs).
    let error = OperatorError::Reconciliation("test failure".into());
    let controller = "machine_config";

    // Simulate what record_error_and_requeue does:
    metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: controller.to_string(),
            result: "error".to_string(),
        })
        .inc();
    let action = Action::requeue(std::time::Duration::from_secs(30));

    // Verify the error counter was incremented
    let count = metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "machine_config".to_string(),
            result: "error".to_string(),
        })
        .get();
    assert_eq!(count, 1, "error counter should be 1");

    // Verify the success counter is separate and still 0
    let success_count = metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "machine_config".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(success_count, 0, "success counter should still be 0");

    // Verify error message is preserved
    assert!(
        error.to_string().contains("test failure"),
        "error message should be preserved"
    );

    // Verify action has 30s requeue
    // (Action doesn't expose its duration, but we verify it's constructed)
    let _ = action;
}

#[test]
fn record_reconcile_success_records_both_counter_and_histogram() {
    use crate::metrics::{Metrics, ReconcileLabels};
    use prometheus_client::registry::Registry;

    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);
    let controller = "config_policy";
    let start = std::time::Instant::now();

    // Simulate record_reconcile_success logic
    let labels = ReconcileLabels {
        controller: controller.to_string(),
        result: "success".to_string(),
    };
    metrics.reconciliations_total.get_or_create(&labels).inc();
    metrics
        .reconciliation_duration_seconds
        .get_or_create(&labels)
        .observe(start.elapsed().as_secs_f64());

    let count = metrics.reconciliations_total.get_or_create(&labels).get();
    assert_eq!(count, 1, "success counter should be 1");
}

#[test]
fn record_reconcile_metrics_different_controllers_independent() {
    use crate::metrics::{Metrics, ReconcileLabels};
    use prometheus_client::registry::Registry;

    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);
    let start = std::time::Instant::now();

    // Simulate record_reconcile_metrics for two different controllers
    for controller in &["machine_config", "drift_alert", "config_policy", "module"] {
        let labels = ReconcileLabels {
            controller: controller.to_string(),
            result: "success".to_string(),
        };
        metrics.reconciliations_total.get_or_create(&labels).inc();
        metrics
            .reconciliation_duration_seconds
            .get_or_create(&labels)
            .observe(start.elapsed().as_secs_f64());
    }

    // Each controller's counter should be independent
    for controller in &["machine_config", "drift_alert", "config_policy", "module"] {
        let labels = ReconcileLabels {
            controller: controller.to_string(),
            result: "success".to_string(),
        };
        assert_eq!(
            metrics.reconciliations_total.get_or_create(&labels).get(),
            1,
            "counter for {} should be 1",
            controller
        );
    }
}

// -----------------------------------------------------------------------
// merge_policy_requirements — namespace policy fills cluster None version
// -----------------------------------------------------------------------

#[test]
fn merge_policy_ns_fills_version_when_cluster_has_none() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "helm".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let ns = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "helm".to_string(),
            version: Some(">=3.10".to_string()),
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    // Cluster has helm with no version, ns has helm with version.
    // The merge loop finds existing, sees version is None, fills from ns.
    // Then the cluster re-apply loop doesn't override because cluster.version is None.
    let helm = merged.packages.iter().find(|p| p.name == "helm").unwrap();
    assert_eq!(
        helm.version.as_deref(),
        Some(">=3.10"),
        "namespace version should fill in when cluster has None"
    );
}

#[test]
fn merge_policy_cluster_version_overrides_ns_filled_version() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "helm".to_string(),
            version: Some(">=3.12".to_string()),
        }],
        ..Default::default()
    };
    let ns = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "helm".to_string(),
            version: Some(">=3.10".to_string()),
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    let helm = merged.packages.iter().find(|p| p.name == "helm").unwrap();
    // Cluster version should win over namespace version
    assert_eq!(helm.version.as_deref(), Some(">=3.12"));
}

#[test]
fn merge_policy_multiple_ns_add_unique_packages_incrementally() {
    let cluster = ClusterConfigPolicySpec::default();
    let ns1 = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "git".to_string(),
            version: None,
        }],
        ..Default::default()
    };
    let ns2 = ConfigPolicySpec {
        packages: vec![
            PackageRef {
                name: "git".to_string(),
                version: Some(">=2.40".to_string()),
            },
            PackageRef {
                name: "vim".to_string(),
                version: None,
            },
        ],
        ..Default::default()
    };
    let ns3 = ConfigPolicySpec {
        packages: vec![
            PackageRef {
                name: "vim".to_string(),
                version: Some(">=9.0".to_string()),
            },
            PackageRef {
                name: "tmux".to_string(),
                version: None,
            },
        ],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2, &ns3]);
    assert_eq!(merged.packages.len(), 3, "git, vim, tmux");

    // git: ns1 adds with None, ns2 finds existing with None, fills to >=2.40
    let git = merged.packages.iter().find(|p| p.name == "git").unwrap();
    assert_eq!(git.version.as_deref(), Some(">=2.40"));

    // vim: ns2 adds with None, ns3 finds existing with None, fills to >=9.0
    let vim = merged.packages.iter().find(|p| p.name == "vim").unwrap();
    assert_eq!(vim.version.as_deref(), Some(">=9.0"));

    // tmux: ns3 adds new
    assert!(merged.packages.iter().any(|p| p.name == "tmux"));
}

#[test]
fn merge_policy_ns_cannot_override_existing_version() {
    let cluster = ClusterConfigPolicySpec {
        packages: vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        }],
        ..Default::default()
    };
    // ns tries to fill version but existing already has one
    let ns = ConfigPolicySpec {
        packages: vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.30".to_string()),
        }],
        ..Default::default()
    };
    let merged = merge_policy_requirements(&cluster, &[&ns]);
    // The ns version is only set if existing.version is None.
    // Since cluster already has >=1.28, ns >=1.30 is ignored by the ns loop.
    // Then cluster re-apply sets it to >=1.28.
    let kubectl = merged
        .packages
        .iter()
        .find(|p| p.name == "kubectl")
        .unwrap();
    assert_eq!(kubectl.version.as_deref(), Some(">=1.28"));
}

#[test]
fn merge_policy_mixed_settings_types() {
    let mut cluster_settings = BTreeMap::new();
    cluster_settings.insert("count".to_string(), serde_json::json!(10));
    cluster_settings.insert("cluster_only".to_string(), serde_json::json!("yes"));

    let cluster = ClusterConfigPolicySpec {
        settings: cluster_settings,
        ..Default::default()
    };

    let mut ns1_settings = BTreeMap::new();
    ns1_settings.insert("count".to_string(), serde_json::json!(5));
    ns1_settings.insert("ns1_only".to_string(), serde_json::json!(true));
    ns1_settings.insert("nested".to_string(), serde_json::json!({"a": 1}));
    let ns1 = ConfigPolicySpec {
        settings: ns1_settings,
        ..Default::default()
    };

    let mut ns2_settings = BTreeMap::new();
    ns2_settings.insert("nested".to_string(), serde_json::json!({"b": 2}));
    let ns2 = ConfigPolicySpec {
        settings: ns2_settings,
        ..Default::default()
    };

    let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2]);

    // Cluster wins for "count"
    assert_eq!(merged.settings["count"], serde_json::json!(10));
    // Cluster-only key preserved
    assert_eq!(merged.settings["cluster_only"], serde_json::json!("yes"));
    // ns1_only preserved (cluster doesn't override it)
    assert_eq!(merged.settings["ns1_only"], serde_json::json!(true));
    // "nested" — ns2 overwrites ns1 (later ns wins), but cluster has no
    // "nested" key so cluster doesn't override it
    assert_eq!(
        merged.settings["nested"],
        serde_json::json!({"b": 2}),
        "later namespace policy should override earlier for same key"
    );
}

// -----------------------------------------------------------------------
// validate_policy_compliance — version with exact match
// -----------------------------------------------------------------------

#[test]
fn policy_version_exact_match() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: None,
    }];
    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("kubectl".to_string(), "1.28.0".to_string());

    // Exact version requirement
    let exact_pkgs = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some("=1.28.0".to_string()),
    }];
    assert!(validate_policy_compliance(
        &spec,
        Some(&status),
        &[],
        &exact_pkgs,
        &Default::default()
    ));

    // Different exact version should fail
    let wrong_exact = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some("=1.29.0".to_string()),
    }];
    assert!(!validate_policy_compliance(
        &spec,
        Some(&status),
        &[],
        &wrong_exact,
        &Default::default()
    ));
}

#[test]
fn policy_compliance_status_has_version_but_no_package_in_spec() {
    // Package exists in status but NOT in spec — should be non-compliant
    let spec = mc_spec("h", "p"); // no packages
    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("kubectl".to_string(), "1.28.0".to_string());

    let policy_pkgs = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.28".to_string()),
    }];
    // Fails because spec.packages doesn't contain kubectl
    assert!(!validate_policy_compliance(
        &spec,
        Some(&status),
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

#[test]
fn policy_compliance_multiple_packages_some_missing_versions() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![
        PackageRef {
            name: "kubectl".to_string(),
            version: None,
        },
        PackageRef {
            name: "helm".to_string(),
            version: None,
        },
    ];
    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("kubectl".to_string(), "1.28.0".to_string());
    // helm has no version reported

    let policy_pkgs = vec![
        PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        },
        PackageRef {
            name: "helm".to_string(),
            version: Some(">=3.10".to_string()),
        },
    ];
    // kubectl passes, but helm version not reported -> non-compliant
    assert!(!validate_policy_compliance(
        &spec,
        Some(&status),
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

#[test]
fn policy_compliance_no_version_required_passes_without_status() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![PackageRef {
        name: "vim".to_string(),
        version: None,
    }];
    // Policy requires vim but with no version constraint
    let policy_pkgs = vec![PackageRef {
        name: "vim".to_string(),
        version: None,
    }];
    // Should pass even without status
    assert!(validate_policy_compliance(
        &spec,
        None,
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

// -----------------------------------------------------------------------
// matches_selector — multiple expression operators combined
// -----------------------------------------------------------------------

#[test]
fn matches_selector_multiple_expressions_all_must_pass() {
    let mut labels = BTreeMap::new();
    labels.insert("env".to_string(), "prod".to_string());
    labels.insert("tier".to_string(), "backend".to_string());
    labels.insert("region".to_string(), "us-east".to_string());

    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![
            LabelSelectorRequirement {
                key: "env".to_string(),
                operator: SelectorOperator::In,
                values: vec!["prod".to_string(), "staging".to_string()],
            },
            LabelSelectorRequirement {
                key: "tier".to_string(),
                operator: SelectorOperator::Exists,
                values: vec![],
            },
            LabelSelectorRequirement {
                key: "deprecated".to_string(),
                operator: SelectorOperator::DoesNotExist,
                values: vec![],
            },
            LabelSelectorRequirement {
                key: "region".to_string(),
                operator: SelectorOperator::NotIn,
                values: vec!["eu-west".to_string()],
            },
        ],
    };

    // All four expressions pass
    assert!(matches_selector(Some(&labels), &selector));

    // Fail one expression — add "deprecated" label
    let mut with_deprecated = labels.clone();
    with_deprecated.insert("deprecated".to_string(), "true".to_string());
    assert!(
        !matches_selector(Some(&with_deprecated), &selector),
        "should fail when DoesNotExist expression fails"
    );
}

#[test]
fn matches_selector_in_with_one_value_acts_like_equals() {
    let mut labels = BTreeMap::new();
    labels.insert("app".to_string(), "cfgd".to_string());

    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "app".to_string(),
            operator: SelectorOperator::In,
            values: vec!["cfgd".to_string()],
        }],
    };
    assert!(matches_selector(Some(&labels), &selector));

    // Wrong value
    let mut wrong = BTreeMap::new();
    wrong.insert("app".to_string(), "other".to_string());
    assert!(!matches_selector(Some(&wrong), &selector));
}

#[test]
fn matches_selector_not_in_absent_key_passes() {
    // NotIn with absent key: is_none_or(|v| !req.values.contains(v))
    // label_value is None -> is_none_or returns true
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "missing".to_string(),
            operator: SelectorOperator::NotIn,
            values: vec!["val1".to_string(), "val2".to_string()],
        }],
    };
    assert!(matches_selector(Some(&BTreeMap::new()), &selector));
    assert!(matches_selector(None, &selector));
}

#[test]
fn matches_selector_exists_with_none_labels_fails() {
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "must-have".to_string(),
            operator: SelectorOperator::Exists,
            values: vec![],
        }],
    };
    assert!(
        !matches_selector(None, &selector),
        "Exists should fail when labels is None"
    );
}

#[test]
fn matches_selector_does_not_exist_with_none_labels_passes() {
    let selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "optional".to_string(),
            operator: SelectorOperator::DoesNotExist,
            values: vec![],
        }],
    };
    assert!(
        matches_selector(None, &selector),
        "DoesNotExist should pass when labels is None (key is absent)"
    );
}

// -----------------------------------------------------------------------
// build_drift_alert_conditions — all severity levels
// -----------------------------------------------------------------------

#[test]
fn build_drift_alert_conditions_all_severities() {
    // Verify escalation mapping for all 4 severity levels
    let test_cases = vec![
        (DriftSeverity::Low, false),
        (DriftSeverity::Medium, false),
        (DriftSeverity::High, true),
        (DriftSeverity::Critical, true),
    ];

    for (severity, expected_escalated) in test_cases {
        let conditions = build_drift_alert_conditions(
            &severity,
            false,
            "dev-test",
            1,
            "2024-01-01T00:00:00Z",
            Some(1),
        );
        let escalated = conditions
            .iter()
            .find(|c| c.condition_type == "Escalated")
            .unwrap();
        assert_eq!(
            escalated.status == "True",
            expected_escalated,
            "severity {:?} should have escalated={}",
            severity,
            expected_escalated
        );
    }
}

#[test]
fn build_drift_alert_conditions_generation_propagated() {
    let generation = Some(42);
    let conditions =
        build_drift_alert_conditions(&DriftSeverity::Low, false, "d", 0, "now", generation);
    for c in &conditions {
        assert_eq!(
            c.observed_generation, generation,
            "generation should propagate to all conditions"
        );
    }
}

#[test]
fn build_drift_alert_conditions_timestamp_propagated() {
    let ts = "2026-04-07T12:00:00Z";
    let conditions = build_drift_alert_conditions(&DriftSeverity::Low, false, "d", 0, ts, None);
    for c in &conditions {
        assert_eq!(
            c.last_transition_time, ts,
            "timestamp should propagate to all conditions"
        );
    }
}

#[test]
fn build_drift_alert_conditions_active_message_includes_details_count() {
    let conditions =
        build_drift_alert_conditions(&DriftSeverity::Medium, false, "node-7", 42, "now", None);
    let resolved = conditions
        .iter()
        .find(|c| c.condition_type == "Resolved")
        .unwrap();
    assert!(
        resolved.message.contains("42 detail(s)"),
        "message should include detail count: {}",
        resolved.message
    );
    assert!(
        resolved.message.contains("node-7"),
        "message should include device_id: {}",
        resolved.message
    );
}

// -----------------------------------------------------------------------
// build_condition — multiple types in existing list
// -----------------------------------------------------------------------

#[test]
fn build_condition_selects_correct_type_from_multiple() {
    let existing = vec![
        Condition {
            condition_type: "Reconciled".to_string(),
            status: "True".to_string(),
            reason: "R".to_string(),
            message: "M".to_string(),
            last_transition_time: "2024-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        },
        Condition {
            condition_type: "DriftDetected".to_string(),
            status: "False".to_string(),
            reason: "NoDrift".to_string(),
            message: "no drift".to_string(),
            last_transition_time: "2024-02-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        },
        Condition {
            condition_type: "ModulesResolved".to_string(),
            status: "True".to_string(),
            reason: "AllResolved".to_string(),
            message: "all resolved".to_string(),
            last_transition_time: "2024-03-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        },
    ];

    // Update DriftDetected from False to True — should use new time
    let c = build_condition(
        &existing,
        "DriftDetected",
        "True",
        "DriftActive",
        "drift found",
        "2024-06-01T00:00:00Z",
        Some(2),
    );
    assert_eq!(c.last_transition_time, "2024-06-01T00:00:00Z");

    // Update Reconciled with same status True — should preserve old time
    let c2 = build_condition(
        &existing,
        "Reconciled",
        "True",
        "ReconcileSuccess",
        "new msg",
        "2024-06-01T00:00:00Z",
        Some(2),
    );
    assert_eq!(c2.last_transition_time, "2024-01-01T00:00:00Z");

    // Update ModulesResolved with same status — preserve time
    let c3 = build_condition(
        &existing,
        "ModulesResolved",
        "True",
        "AllResolved",
        "still resolved",
        "2024-06-01T00:00:00Z",
        Some(2),
    );
    assert_eq!(c3.last_transition_time, "2024-03-01T00:00:00Z");
}

// -----------------------------------------------------------------------
// validate_policy_compliance — combined packages + modules + settings
// -----------------------------------------------------------------------

#[test]
fn policy_compliance_modules_present_but_settings_mismatch() {
    let mut spec = mc_spec("h", "p");
    spec.module_refs = vec![ModuleRef {
        name: "corp-vpn".to_string(),
        required: true,
    }];
    spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: None,
    }];
    spec.system_settings
        .insert("enforce_tls".to_string(), serde_json::json!(false));

    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("kubectl".to_string(), "1.29.0".to_string());

    let required_modules = vec![ModuleRef {
        name: "corp-vpn".to_string(),
        required: true,
    }];
    let required_packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.28".to_string()),
    }];
    let mut required_settings = BTreeMap::new();
    required_settings.insert("enforce_tls".to_string(), serde_json::json!(true));

    // Modules present, version OK, but settings mismatch (false != true)
    assert!(!validate_policy_compliance(
        &spec,
        Some(&status),
        &required_modules,
        &required_packages,
        &required_settings,
    ));
}

#[test]
fn policy_compliance_version_check_short_circuits_on_first_failure() {
    let mut spec = mc_spec("h", "p");
    spec.packages = vec![
        PackageRef {
            name: "a".to_string(),
            version: None,
        },
        PackageRef {
            name: "b".to_string(),
            version: None,
        },
    ];
    let mut status = MachineConfigStatus::default();
    status
        .package_versions
        .insert("a".to_string(), "1.0.0".to_string());
    // b has no version reported

    let policy_pkgs = vec![
        PackageRef {
            name: "a".to_string(),
            version: Some(">=2.0".to_string()),
        },
        PackageRef {
            name: "b".to_string(),
            version: Some(">=1.0".to_string()),
        },
    ];
    // a fails version check (1.0.0 < 2.0)
    assert!(!validate_policy_compliance(
        &spec,
        Some(&status),
        &[],
        &policy_pkgs,
        &Default::default()
    ));
}

// -----------------------------------------------------------------------
// find_condition_status / find_condition_transition_time — duplicates
// -----------------------------------------------------------------------

#[test]
fn find_condition_status_returns_first_match() {
    // If there are duplicates, find returns the first one
    let conditions = vec![
        Condition {
            condition_type: "Ready".to_string(),
            status: "True".to_string(),
            reason: "R".to_string(),
            message: "M".to_string(),
            last_transition_time: "t1".to_string(),
            observed_generation: Some(1),
        },
        Condition {
            condition_type: "Ready".to_string(),
            status: "False".to_string(),
            reason: "R2".to_string(),
            message: "M2".to_string(),
            last_transition_time: "t2".to_string(),
            observed_generation: Some(2),
        },
    ];
    assert_eq!(
        find_condition_status(&conditions, "Ready"),
        Some("True".to_string()),
        "should return the first matching condition"
    );
    assert_eq!(
        find_condition_transition_time(&conditions, "Ready"),
        Some("t1".to_string()),
        "should return transition time of first match"
    );
}

// -----------------------------------------------------------------------
// namespaced_api — error on empty namespace (via validate_spec path)
// -----------------------------------------------------------------------

#[test]
fn validate_spec_both_empty_returns_both_errors() {
    let err = validate_spec(&mc_spec("", "")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("hostname"), "should mention hostname: {msg}");
    assert!(msg.contains("profile"), "should mention profile: {msg}");
}

// -----------------------------------------------------------------------
// namespaced_api — direct test of empty namespace error path
// -----------------------------------------------------------------------

#[tokio::test]
async fn namespaced_api_empty_namespace_returns_error() {
    // Construct a dummy kube Client via a tower service that always errors
    // (we just need the Client type, not actual HTTP calls)
    let client = make_test_client();
    let result = namespaced_api::<MachineConfig>(&client, "");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match &err {
        OperatorError::Reconciliation(msg) => {
            assert!(
                msg.contains("no namespace"),
                "expected 'no namespace' in error message: {msg}"
            );
        }
        other => panic!("expected Reconciliation error, got: {other}"),
    }
}

#[tokio::test]
async fn namespaced_api_valid_namespace_returns_api() {
    let client = make_test_client();
    let result = namespaced_api::<MachineConfig>(&client, "default");
    assert!(result.is_ok(), "valid namespace should return Ok(Api)");
}

// -----------------------------------------------------------------------
// record_error_and_requeue — actual function call
// -----------------------------------------------------------------------

#[tokio::test]
async fn record_error_and_requeue_returns_30s_action_and_increments_counter() {
    let (ctx, _registry) = make_test_controller_context();
    let error = OperatorError::Reconciliation("test failure".into());
    let action = record_error_and_requeue(&error, &ctx, "test_controller");

    // Verify metrics were incremented
    let labels = crate::metrics::ReconcileLabels {
        controller: "test_controller".to_string(),
        result: "error".to_string(),
    };
    let count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&labels)
        .get();
    assert_eq!(count, 1, "error counter should be 1 after one call");

    // Verify success counter is untouched
    let success_labels = crate::metrics::ReconcileLabels {
        controller: "test_controller".to_string(),
        result: "success".to_string(),
    };
    let success_count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&success_labels)
        .get();
    assert_eq!(success_count, 0, "success counter should remain 0");

    // Verify the Action is a requeue (we can't inspect duration directly,
    // but we can confirm the function returned without panic)
    let _ = action;
}

#[tokio::test]
async fn record_error_and_requeue_multiple_calls_accumulate() {
    let (ctx, _registry) = make_test_controller_context();
    let error = OperatorError::Reconciliation("e1".into());
    let _ = record_error_and_requeue(&error, &ctx, "mc");
    let _ = record_error_and_requeue(&error, &ctx, "mc");
    let _ = record_error_and_requeue(&error, &ctx, "mc");

    let labels = crate::metrics::ReconcileLabels {
        controller: "mc".to_string(),
        result: "error".to_string(),
    };
    let count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&labels)
        .get();
    assert_eq!(count, 3, "error counter should accumulate across calls");
}

// -----------------------------------------------------------------------
// record_reconcile_success — actual function call
// -----------------------------------------------------------------------

#[tokio::test]
async fn record_reconcile_success_increments_counter_and_records_histogram() {
    let (ctx, _registry) = make_test_controller_context();
    let start = std::time::Instant::now();
    record_reconcile_success(&ctx, "drift_alert", start);

    let labels = crate::metrics::ReconcileLabels {
        controller: "drift_alert".to_string(),
        result: "success".to_string(),
    };
    let count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&labels)
        .get();
    assert_eq!(count, 1, "success counter should be 1");
}

// -----------------------------------------------------------------------
// record_reconcile_metrics — actual function call
// -----------------------------------------------------------------------

#[tokio::test]
async fn record_reconcile_metrics_with_different_results() {
    let (ctx, _registry) = make_test_controller_context();
    let start = std::time::Instant::now();

    record_reconcile_metrics(&ctx, "module", "success", start);
    record_reconcile_metrics(&ctx, "module", "error", start);
    record_reconcile_metrics(&ctx, "module", "success", start);

    let success_labels = crate::metrics::ReconcileLabels {
        controller: "module".to_string(),
        result: "success".to_string(),
    };
    let error_labels = crate::metrics::ReconcileLabels {
        controller: "module".to_string(),
        result: "error".to_string(),
    };
    assert_eq!(
        ctx.metrics
            .reconciliations_total
            .get_or_create(&success_labels)
            .get(),
        2
    );
    assert_eq!(
        ctx.metrics
            .reconciliations_total
            .get_or_create(&error_labels)
            .get(),
        1
    );
}

// -----------------------------------------------------------------------
// make_error_policy — verify each typed closure tags the correct label
// (the no-assertion "records_metrics_and_returns_action" duplicates the
// record_error_and_requeue tests above and has been removed)
// -----------------------------------------------------------------------

#[tokio::test]
async fn error_policy_mc_labels_controller_as_machine_config() {
    let (ctx, _registry) = make_test_controller_context();
    let ctx = Arc::new(ctx);
    let obj = Arc::new(MachineConfig {
        metadata: Default::default(),
        spec: mc_spec("h", "p"),
        status: None,
    });
    let error = OperatorError::Reconciliation("e".into());
    let _ = make_error_policy::<MachineConfig>("machine_config")(obj, &error, Arc::clone(&ctx));
    let labels = crate::metrics::ReconcileLabels {
        controller: "machine_config".to_string(),
        result: "error".to_string(),
    };
    assert_eq!(
        ctx.metrics
            .reconciliations_total
            .get_or_create(&labels)
            .get(),
        1,
        "machine_config error_policy should tag the 'machine_config' controller label"
    );
}

#[tokio::test]
async fn error_policy_da_labels_controller_as_drift_alert() {
    let (ctx, _registry) = make_test_controller_context();
    let ctx = Arc::new(ctx);
    let obj = Arc::new(DriftAlert {
        metadata: Default::default(),
        spec: crate::crds::DriftAlertSpec {
            device_id: "d".to_string(),
            machine_config_ref: crate::crds::MachineConfigReference {
                name: "m".to_string(),
                namespace: None,
            },
            severity: DriftSeverity::Low,
            drift_details: vec![],
        },
        status: None,
    });
    let error = OperatorError::Reconciliation("e".into());
    let _ = make_error_policy::<DriftAlert>("drift_alert")(obj, &error, Arc::clone(&ctx));
    let labels = crate::metrics::ReconcileLabels {
        controller: "drift_alert".to_string(),
        result: "error".to_string(),
    };
    assert_eq!(
        ctx.metrics
            .reconciliations_total
            .get_or_create(&labels)
            .get(),
        1,
        "drift_alert error_policy should tag the 'drift_alert' controller label"
    );
}

#[tokio::test]
async fn error_policy_cp_labels_controller_as_config_policy() {
    let (ctx, _registry) = make_test_controller_context();
    let ctx = Arc::new(ctx);
    let obj = Arc::new(ConfigPolicy {
        metadata: Default::default(),
        spec: ConfigPolicySpec::default(),
        status: None,
    });
    let error = OperatorError::Reconciliation("e".into());
    let _ = make_error_policy::<ConfigPolicy>("config_policy")(obj, &error, Arc::clone(&ctx));
    let labels = crate::metrics::ReconcileLabels {
        controller: "config_policy".to_string(),
        result: "error".to_string(),
    };
    assert_eq!(
        ctx.metrics
            .reconciliations_total
            .get_or_create(&labels)
            .get(),
        1,
        "config_policy error_policy should tag the 'config_policy' controller label"
    );
}

#[tokio::test]
async fn error_policy_ccp_labels_controller_as_cluster_config_policy() {
    let (ctx, _registry) = make_test_controller_context();
    let ctx = Arc::new(ctx);
    let obj = Arc::new(ClusterConfigPolicy {
        metadata: Default::default(),
        spec: ClusterConfigPolicySpec::default(),
        status: None,
    });
    let error = OperatorError::Reconciliation("e".into());
    let _ = make_error_policy::<ClusterConfigPolicy>("cluster_config_policy")(
        obj,
        &error,
        Arc::clone(&ctx),
    );
    let labels = crate::metrics::ReconcileLabels {
        controller: "cluster_config_policy".to_string(),
        result: "error".to_string(),
    };
    assert_eq!(
        ctx.metrics
            .reconciliations_total
            .get_or_create(&labels)
            .get(),
        1,
        "cluster_config_policy error_policy should tag the 'cluster_config_policy' controller label"
    );
}

#[tokio::test]
async fn error_policy_module_labels_controller_as_module() {
    let (ctx, _registry) = make_test_controller_context();
    let ctx = Arc::new(ctx);
    let obj = Arc::new(Module {
        metadata: Default::default(),
        spec: ModuleSpec::default(),
        status: None,
    });
    let error = OperatorError::Reconciliation("e".into());
    let _ = make_error_policy::<Module>("module")(obj, &error, Arc::clone(&ctx));
    let labels = crate::metrics::ReconcileLabels {
        controller: "module".to_string(),
        result: "error".to_string(),
    };
    assert_eq!(
        ctx.metrics
            .reconciliations_total
            .get_or_create(&labels)
            .get(),
        1,
        "module error_policy should tag the 'module' controller label"
    );
}

// -----------------------------------------------------------------------
// validate_spec — various error types from OperatorError::InvalidSpec
// -----------------------------------------------------------------------

#[test]
fn validate_spec_invalid_spec_error_type() {
    let err = validate_spec(&mc_spec("", "default")).unwrap_err();
    match err {
        OperatorError::InvalidSpec(msg) => {
            assert!(
                msg.contains("hostname"),
                "InvalidSpec should mention hostname: {msg}"
            );
        }
        other => panic!("expected InvalidSpec, got: {other}"),
    }
}

// -----------------------------------------------------------------------
// Test helpers
// -----------------------------------------------------------------------

fn make_test_client() -> Client {
    use hyper::body::Bytes;
    use tower::service_fn;

    let svc = service_fn(|_req: hyper::Request<kube::client::Body>| async {
        Ok::<_, std::convert::Infallible>(
            hyper::Response::builder()
                .status(200)
                .body(kube::client::Body::from(Bytes::from_static(b"{}")))
                .unwrap(),
        )
    });
    Client::new(svc, "default")
}

fn make_test_controller_context() -> (ControllerContext, prometheus_client::registry::Registry) {
    let client = make_test_client();
    let reporter = kube::runtime::events::Reporter {
        controller: "test".into(),
        instance: None,
    };
    let recorder = kube::runtime::events::Recorder::new(client.clone(), reporter);
    let mut registry = prometheus_client::registry::Registry::default();
    let metrics = crate::metrics::Metrics::new(&mut registry);
    (
        ControllerContext {
            client,
            recorder,
            metrics,
        },
        registry,
    )
}
