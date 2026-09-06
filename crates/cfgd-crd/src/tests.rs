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

#[test]
fn api_version_helper_matches_every_kind_derive() {
    use kube::Resource;
    let shared = crate::api_version();
    assert_eq!(shared, "cfgd.io/v1alpha1");
    for got in [
        MachineConfig::api_version(&()),
        ConfigPolicy::api_version(&()),
        ClusterConfigPolicy::api_version(&()),
        DriftAlert::api_version(&()),
        Module::api_version(&()),
    ] {
        assert_eq!(got, shared, "every cfgd CRD kind must share one apiVersion");
    }
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
    assert!(col_names.contains(&"Signature"));
    assert!(col_names.contains(&"Platforms"));
    assert!(col_names.contains(&"Available"));
    assert!(col_names.contains(&"Age"));
}

/// Every kind whose status carries `conditions` exposes its readiness
/// condition as a printer column. `Module` shipped without one, so a module
/// the operator WITHHELD over its signature verdict (`Available: False`) and
/// a served one were the same row in `kubectl get modules` — the one surface
/// a cluster user reaches for. The column's condition type is checked against
/// the literals the operator's controllers write, so a column bound to a
/// condition nothing sets would trip here too.
#[test]
fn every_kind_with_conditions_exposes_its_readiness_condition_as_a_column() {
    use kube::CustomResourceExt;

    let controllers =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cfgd-operator/src/controllers");
    let written: String = std::fs::read_dir(&controllers)
        .expect("the operator's controllers directory is checked out")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect();

    let crds = [
        ("MachineConfig", MachineConfig::crd()),
        ("ConfigPolicy", ConfigPolicy::crd()),
        ("ClusterConfigPolicy", ClusterConfigPolicy::crd()),
        ("DriftAlert", DriftAlert::crd()),
        ("Module", Module::crd()),
    ];
    let mut judged = 0usize;
    for (kind, crd) in crds {
        let version = &crd.spec.versions[0];
        let schema = version
            .schema
            .as_ref()
            .and_then(|s| s.open_api_v3_schema.as_ref())
            .unwrap_or_else(|| panic!("{kind} must publish a schema"));
        if resolve_column_schema(schema, ".status.conditions").as_deref() != Some("array") {
            continue;
        }
        judged += 1;
        let condition_types: Vec<String> = version
            .additional_printer_columns
            .iter()
            .flatten()
            .filter_map(|c| {
                let rest = c
                    .json_path
                    .strip_prefix(".status.conditions[?(@.type==\"")?;
                let (ty, _) = rest.split_once("\")].status")?;
                Some(ty.to_string())
            })
            .collect();
        assert!(
            !condition_types.is_empty(),
            "{kind} writes conditions but exposes none of them as a printer column"
        );
        for ty in condition_types {
            assert!(
                written.contains(&format!("\"{ty}\"")),
                "{kind}'s printer column binds to a `{ty}` condition no controller writes"
            );
        }
    }
    assert_eq!(
        judged, 5,
        "every kind carries conditions; the walk reached {judged}"
    );
}

/// A printer column resolving to an ARRAY prints the Go rendering of the
/// slice, so an empty one reads as the literal `[]` where an absent value
/// leaves the cell blank — `kubectl get` has no way to join one. A column
/// resolving to a BOOL prints `true`/`false`, a second vocabulary beside the
/// word every human surface spells for the same verdict. Every column on every
/// kind therefore binds to a derived display field: a list gets a summary
/// string (`platformsSummary`), a verdict gets a word (`signature`), and the
/// raw `availablePlatforms` / `verified` fields stay on the wire for machines.
#[test]
fn every_printer_column_binds_to_a_display_field() {
    use kube::CustomResourceExt;

    let crds = [
        ("MachineConfig", MachineConfig::crd()),
        ("ConfigPolicy", ConfigPolicy::crd()),
        ("ClusterConfigPolicy", ClusterConfigPolicy::crd()),
        ("DriftAlert", DriftAlert::crd()),
        ("Module", Module::crd()),
    ];

    for (kind, crd) in crds {
        for version in &crd.spec.versions {
            let schema = version
                .schema
                .as_ref()
                .and_then(|s| s.open_api_v3_schema.as_ref())
                .unwrap_or_else(|| panic!("{kind} must publish a schema"));
            for column in version.additional_printer_columns.iter().flatten() {
                let Some(resolved) = resolve_column_schema(schema, &column.json_path) else {
                    // `metadata.*` and a `conditions[?(...)]` filter resolve
                    // outside the spec/status schema this walk can see; both
                    // select a scalar by construction.
                    continue;
                };
                assert!(
                    resolved != "array" && resolved != "boolean",
                    "{kind}'s {} column binds to a raw {resolved} ({}) — bind it to a derived display field instead",
                    column.name,
                    column.json_path,
                );
            }
        }
    }
}

/// The walk above is only a fence if it can SEE the shapes it rejects.
/// `Module` carries both (`availablePlatforms`, the list `platformsSummary`
/// renders; `verified`, the bool `signature` spells), so resolving them is the
/// positive control the fence's negative depends on.
#[test]
fn the_display_column_fence_resolves_the_raw_shapes_it_rejects() {
    use kube::CustomResourceExt;

    let crd = Module::crd();
    let schema = crd.spec.versions[0]
        .schema
        .as_ref()
        .and_then(|s| s.open_api_v3_schema.as_ref())
        .expect("Module publishes a schema");
    assert_eq!(
        resolve_column_schema(schema, ".status.availablePlatforms").as_deref(),
        Some("array"),
    );
    assert_eq!(
        resolve_column_schema(schema, ".status.verified").as_deref(),
        Some("boolean"),
    );
    assert_eq!(
        resolve_column_schema(schema, ".status.signature").as_deref(),
        Some("string"),
    );
}

/// Walk a dotted `jsonPath` through a CRD's OpenAPI schema and answer the
/// leaf's `type`. `None` means the path leaves what the schema describes
/// (a `metadata` field, or a JSONPath filter expression).
fn resolve_column_schema(
    root: &k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::JSONSchemaProps,
    json_path: &str,
) -> Option<String> {
    if json_path.contains('[') {
        return None;
    }
    let mut node = root;
    for segment in json_path.trim_start_matches('.').split('.') {
        if segment == "metadata" {
            return None;
        }
        node = node.properties.as_ref()?.get(segment)?;
    }
    node.type_.clone()
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
            platforms: vec![],
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

/// A DriftAlert carries what `cfgd checkin` sends, and that payload is the
/// answers of the device's system CONFIGURATORS alone — no package, file, env or
/// alias finding has ever reached the fleet. `kubectl explain driftalert` reads
/// these descriptions, so they name the class rather than letting an operator
/// read a settings report as a whole-machine verdict.
#[test]
fn the_driftalert_schema_names_the_class_of_drift_a_device_reports() {
    use kube::CustomResourceExt;
    let crd = DriftAlert::crd();
    let schema = crd.spec.versions[0]
        .schema
        .as_ref()
        .and_then(|s| s.open_api_v3_schema.as_ref())
        .expect("DriftAlert publishes a schema");
    let spec = schema
        .properties
        .as_ref()
        .and_then(|p| p.get("spec"))
        .expect("DriftAlert spec property");

    let described = spec.description.clone().unwrap_or_default();
    assert!(
        described.to_lowercase().contains("system setting"),
        "the DriftAlert spec description must name the class a device reports: {described}"
    );

    let details = spec
        .properties
        .as_ref()
        .and_then(|p| p.get("driftDetails"))
        .and_then(|d| d.description.clone())
        .unwrap_or_default();
    assert!(
        details.to_lowercase().contains("system setting"),
        "driftDetails must name what each entry is: {details}"
    );
}
