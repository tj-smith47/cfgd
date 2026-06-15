//! CRD cross-field validation, converged through `cfgd-crd`.
//!
//! The CLI `validate` path (registry → `validate_fn`) and the operator webhook
//! both reach the same `cfgd_crd::*Spec::validate()` impls, so a cross-field
//! violation rejected at admission is rejected identically at the CLI. These
//! tests pin that convergence (CLI-path errors == webhook-path errors) and
//! confirm minimal valid documents for all five CRD kinds still pass.
//!
//! In-process Doc-capture style (matching `validate_cli.rs`): the rejection
//! test drives `cmd_machineconfig_validate` against a ground-truth fixture
//! derived from `cfgd_crd::MachineConfigSpec::example_with_traversal_path()`.

use cfgd_core::generate::validate::validate_document;
use cfgd_core::output::Printer;

/// The bad-path fixture: a full KRM document serialized from the real
/// `MachineConfigSpec::example_with_traversal_path()` producer.
const BAD_PATH_DOC: &str = include_str!("fixtures/machineconfig_bad_path.yaml");

/// A minimal valid document for each CRD kind, used to confirm the converged
/// `validate_fn` does not over-reject well-formed specs.
const VALID_MACHINECONFIG: &str = "apiVersion: cfgd.io/v1alpha1\nkind: MachineConfig\nmetadata:\n  name: mc\nspec:\n  hostname: host1\n  profile: default\n";
const VALID_CONFIGPOLICY: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: ConfigPolicy\nmetadata:\n  name: cp\nspec: {}\n";
const VALID_CLUSTERCONFIGPOLICY: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: ClusterConfigPolicy\nmetadata:\n  name: ccp\nspec: {}\n";
const VALID_DRIFTALERT: &str = "apiVersion: cfgd.io/v1alpha1\nkind: DriftAlert\nmetadata:\n  name: da\nspec:\n  deviceId: dev-1\n  machineConfigRef:\n    name: mc\n  severity: High\n";
const VALID_MODULE_CRD: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: m\nspec: {}\n";

/// Ground-truth guard: the committed bad-path fixture must equal the producer's
/// current serialization, so a drift in `MachineConfigSpec`'s fields fails loudly
/// here instead of silently rotting the fixture against its provenance prose.
#[test]
fn fixture_matches_producer() {
    let doc = serde_yaml::to_string(&serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "MachineConfig",
        "metadata": {"name": "bad-path"},
        "spec": cfgd_crd::MachineConfigSpec::example_with_traversal_path(),
    }))
    .expect("serialize producer spec");
    assert_eq!(
        doc, BAD_PATH_DOC,
        "committed fixture must equal the producer's serialization"
    );
}

/// The bad MachineConfig document, fed through the user-facing `machineconfig
/// validate` command path, must FAIL with the webhook's path-traversal message.
#[test]
fn validate_machineconfig_rejects_path_traversal() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("machineconfig_bad_path.yaml");
    std::fs::write(&path, BAD_PATH_DOC).expect("write fixture");
    let path_str = path.to_str().expect("utf8 temp path");

    let (printer, _cap) = Printer::for_test_doc();
    let err = cfgd::cli::validate::cmd_machineconfig_validate(&printer, path_str)
        .expect_err("path traversal must fail validation");
    let msg = format!("{err:#}");
    // Anti-tautology: pin the traversal phrasing, not merely "invalid".
    assert!(
        msg.contains("path traversal") && msg.contains(".."),
        "rejection must name the path-traversal rule with `..`, got: {msg}"
    );
}

/// The cross-field violation surfaces through the registry-driven
/// `validate_document` too (not only the CLI wrapper), so every consumer of the
/// unified registry rejects it.
#[test]
fn validate_document_rejects_machineconfig_path_traversal() {
    let result = validate_document(BAD_PATH_DOC);
    assert!(
        !result.valid,
        "bad-path MachineConfig must be invalid via the registry, got: {result:?}"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("path traversal") && e.contains("..")),
        "registry errors must name the traversal rule, got: {:?}",
        result.errors
    );
}

/// Convergence proof: the error strings produced through the CLI/registry path
/// (`validate_document`) are EQUAL to those produced by calling the webhook's
/// impl (`cfgd_crd::MachineConfigSpec::validate()`) on the same spec. One impl,
/// no fork.
#[test]
fn cli_path_errors_match_webhook_path_errors() {
    // Webhook path: deserialize the document's spec exactly as the admission
    // handler does, then call the shared inherent validate().
    let value: serde_yaml::Value = serde_yaml::from_str(BAD_PATH_DOC).expect("parse fixture");
    let spec_value = value.get("spec").cloned().expect("fixture has spec");
    let spec: cfgd_crd::MachineConfigSpec =
        serde_yaml::from_value(spec_value).expect("spec deserializes");
    let webhook_errors = spec
        .validate()
        .expect_err("traversal spec must be rejected");

    // CLI/registry path.
    let cli_result = validate_document(BAD_PATH_DOC);
    assert!(!cli_result.valid, "CLI path must reject the same spec");

    assert_eq!(
        cli_result.errors, webhook_errors,
        "CLI-path validation errors must equal webhook-path errors (no fork)"
    );
}

/// Convergence proof across every CRD kind with a non-trivial rejection path.
///
/// For each kind a REJECTING spec is built in Rust and serialized into a full
/// KRM document (producer-derived — the spec struct is the source of truth, not
/// hand-authored YAML), then the registry-driven validation errors are asserted
/// byte-equal to the webhook path (`<Spec>::validate()`), proving the two paths
/// share one impl for ConfigPolicy, ClusterConfigPolicy, DriftAlert, and the CRD
/// Module — not only MachineConfig.
///
/// The three policy/alert kinds dispatch through `validate_document`. The CRD
/// `Module` shares its `kind` string with the LOCAL Module document, which
/// `validate_document` deliberately prefers (local documents are what that path
/// receives), so the CRD Module's validator is reached only through the
/// registry's `crd` entry — the same dispatch the operator-side registry uses.
/// That entry's `validate_fn` is the convergence target for the CRD Module.
#[test]
fn cli_path_errors_match_webhook_path_for_every_crd_kind() {
    use cfgd_core::schema::KIND_REGISTRY;
    fn doc_for(kind: &str, spec: serde_json::Value) -> String {
        serde_yaml::to_string(&serde_json::json!({
            "apiVersion": "cfgd.io/v1alpha1",
            "kind": kind,
            "metadata": {"name": "reject"},
            "spec": spec,
        }))
        .expect("serialize rejecting document")
    }

    // ConfigPolicy: empty package name + empty required-module name.
    let configpolicy_spec = cfgd_crd::ConfigPolicySpec {
        packages: vec![cfgd_crd::PackageRef {
            name: String::new(),
            version: None,
        }],
        required_modules: vec![cfgd_crd::ModuleRef {
            name: String::new(),
            required: true,
        }],
        ..Default::default()
    };

    // ClusterConfigPolicy: same shared policy-field rejections.
    let clusterconfigpolicy_spec = cfgd_crd::ClusterConfigPolicySpec {
        packages: vec![cfgd_crd::PackageRef {
            name: String::new(),
            version: None,
        }],
        required_modules: vec![cfgd_crd::ModuleRef {
            name: String::new(),
            required: true,
        }],
        ..Default::default()
    };

    // DriftAlert: empty deviceId + empty machineConfigRef.name.
    let driftalert_spec = cfgd_crd::DriftAlertSpec {
        device_id: String::new(),
        machine_config_ref: cfgd_crd::MachineConfigReference {
            name: String::new(),
            namespace: None,
        },
        drift_details: Vec::new(),
        severity: cfgd_crd::DriftSeverity::High,
    };

    // CRD Module: empty package name + empty depends entry.
    let module_spec = cfgd_crd::ModuleSpec {
        packages: vec![cfgd_crd::PackageEntry {
            name: String::new(),
            ..Default::default()
        }],
        depends: vec![String::new()],
        ..Default::default()
    };

    // Kinds with no local-document shadow: `validate_document` reaches their CRD
    // validator directly.
    let document_cases: Vec<(&str, String, Vec<String>)> = vec![
        (
            "ConfigPolicy",
            doc_for(
                "ConfigPolicy",
                serde_json::to_value(&configpolicy_spec).expect("to value"),
            ),
            configpolicy_spec
                .validate()
                .expect_err("rejecting ConfigPolicy spec"),
        ),
        (
            "ClusterConfigPolicy",
            doc_for(
                "ClusterConfigPolicy",
                serde_json::to_value(&clusterconfigpolicy_spec).expect("to value"),
            ),
            clusterconfigpolicy_spec
                .validate()
                .expect_err("rejecting ClusterConfigPolicy spec"),
        ),
        (
            "DriftAlert",
            doc_for(
                "DriftAlert",
                serde_json::to_value(&driftalert_spec).expect("to value"),
            ),
            driftalert_spec
                .validate()
                .expect_err("rejecting DriftAlert spec"),
        ),
    ];

    for (kind, doc, webhook_errors) in document_cases {
        let cli_result = validate_document(&doc);
        assert!(
            !cli_result.valid,
            "{kind} rejecting spec must be invalid via the registry, got: {cli_result:?}"
        );
        assert_eq!(
            cli_result.errors, webhook_errors,
            "{kind}: CLI-path errors must equal webhook-path errors (no fork)"
        );
    }

    // CRD Module: reach the CRD validator through the registry's `crd` Module
    // entry (the local Module shadows it in `validate_document`).
    let module_doc = doc_for(
        "Module",
        serde_json::to_value(&module_spec).expect("to value"),
    );
    let module_webhook_errors = module_spec.validate().expect_err("rejecting Module spec");
    let module_entry = KIND_REGISTRY
        .iter()
        .find(|e| e.kind == "Module" && e.crd)
        .expect("registry carries a CRD Module entry");
    let module_registry_errors = (module_entry.validate_fn)(&module_doc)
        .expect_err("registry CRD Module validate_fn must reject the spec");
    assert_eq!(
        module_registry_errors, module_webhook_errors,
        "Module: registry CRD-path errors must equal webhook-path errors (no fork)"
    );
}

#[test]
fn valid_crd_documents_pass_for_all_five_kinds() {
    for (label, doc) in [
        ("MachineConfig", VALID_MACHINECONFIG),
        ("ConfigPolicy", VALID_CONFIGPOLICY),
        ("ClusterConfigPolicy", VALID_CLUSTERCONFIGPOLICY),
        ("DriftAlert", VALID_DRIFTALERT),
        ("Module", VALID_MODULE_CRD),
    ] {
        let result = validate_document(doc);
        assert!(
            result.valid,
            "minimal valid {label} must pass, got errors: {:?}",
            result.errors
        );
    }
}
