//! Round-trip tests: `cli::output_types` payloads emitted through a
//! JSON-format `output::Printer` deserialize back to the same JSON shape.
//!
//! Tests three payload shapes:
//!   * snapshot-shape (`DoctorOutput`): nested struct of typed checks
//!   * mixed-shape (`PlanOutput`): nested struct with a Vec of children
//!   * streaming-shape (`LogOutput`): Vec of records from cfgd-core
//!
//! The output_types only derive `Serialize`, so round-trip equality is asserted
//! at the `serde_json::Value` level (parse stdout → compare to freshly-serialized
//! payload).

use cfgd::cli::output_types::{
    DoctorConfigCheck, DoctorConfiguratorCheck, DoctorManagerCheck, DoctorModuleCheck,
    DoctorOutput, DoctorProviderCheck, DoctorSecretsCheck, LogOutput, PlanActionOutput, PlanOutput,
    PlanPhaseOutput,
};
use cfgd_core::output::{Doc, OutputFormat, Printer};
use cfgd_core::state::{ApplyRecord, ApplyStatus};
use pretty_assertions::assert_eq;

/// Emit `payload` through a JSON-format Printer and parse stdout back into a
/// `serde_json::Value`. The captured buffer is asserted non-empty.
fn emit_and_parse<T: serde::Serialize>(payload: &T) -> serde_json::Value {
    let (p, buf) = Printer::for_test_with_format(OutputFormat::Json);
    let doc = Doc::new().with_data(payload);
    p.emit(doc);
    drop(p);
    let raw = buf.lock().unwrap().clone();
    assert!(!raw.trim().is_empty(), "emit produced no stdout");
    serde_json::from_str(&raw).expect("emitted stdout is valid JSON")
}

#[test]
fn doctor_output_roundtrips_through_emit() {
    let payload = DoctorOutput {
        config: DoctorConfigCheck {
            valid: true,
            path: "/etc/cfgd/cfgd.yaml".into(),
            name: Some("test-host".into()),
            profile: Some("base".into()),
            error: None,
        },
        git: true,
        secrets: DoctorSecretsCheck {
            sops_available: true,
            sops_version: Some("3.8.1".into()),
            age_key_exists: true,
            age_key_path: Some("~/.config/sops/age/keys.txt".into()),
            sops_config_exists: false,
            sops_config_path: None,
            providers: vec![DoctorProviderCheck {
                name: "age".into(),
                available: true,
            }],
        },
        package_managers: vec![DoctorManagerCheck {
            name: "brew".into(),
            available: true,
            declared: true,
            can_bootstrap: false,
            bootstrap_method: None,
        }],
        modules: vec![DoctorModuleCheck {
            name: "base".into(),
            valid: true,
            error: None,
            packages: Vec::new(),
        }],
        system_configurators: vec![DoctorConfiguratorCheck {
            name: "shell".into(),
            available: true,
        }],
        profiles: Vec::new(),
    };

    let actual = emit_and_parse(&payload);
    let expected = serde_json::to_value(&payload).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn plan_output_roundtrips_through_emit() {
    let payload = PlanOutput {
        context: "base".into(),
        phases: vec![
            PlanPhaseOutput {
                phase: "packages".into(),
                actions: vec![
                    PlanActionOutput {
                        description: "install ripgrep via brew".into(),
                        action_type: "package_install".into(),
                        targets: vec![],
                        origin: None,
                    },
                    PlanActionOutput {
                        description: "install fd via brew".into(),
                        action_type: "package_install".into(),
                        targets: vec![],
                        origin: None,
                    },
                ],
            },
            PlanPhaseOutput {
                phase: "files".into(),
                actions: vec![PlanActionOutput {
                    description: "write ~/.gitconfig".into(),
                    action_type: "file_write".into(),
                    targets: vec!["/home/u/.gitconfig".into()],
                    origin: None,
                }],
            },
        ],
        total_actions: 3,
        warnings: vec!["module 'foo' has no provider".into()],
    };

    let actual = emit_and_parse(&payload);
    let expected = serde_json::to_value(&payload).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn log_output_roundtrips_through_emit() {
    let payload = LogOutput {
        entries: vec![
            ApplyRecord {
                id: 1,
                timestamp: "2026-05-17T12:00:00Z".into(),
                profile: "base".into(),
                plan_hash: "abc123".into(),
                status: ApplyStatus::Success,
                summary: Some("3 packages installed".into()),
            },
            ApplyRecord {
                id: 2,
                timestamp: "2026-05-17T12:05:00Z".into(),
                profile: "base".into(),
                plan_hash: "def456".into(),
                status: ApplyStatus::Partial,
                summary: Some("1 module failed".into()),
            },
        ],
    };

    let actual = emit_and_parse(&payload);
    let expected = serde_json::to_value(&payload).unwrap();
    assert_eq!(actual, expected);
}
