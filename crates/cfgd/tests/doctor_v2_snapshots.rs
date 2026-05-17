//! Snapshot tests for `cfgd doctor`.
//!
//! Goldens live under `tests/output_snapshots/doctor/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test doctor_v2_snapshots
//!
//! The live `cmd_doctor` runs environment-dependent probes (`git`, `sops`,
//! state-store, profiles-dir scan, etc.). To keep snapshots stable across
//! hosts, these tests drive `build_doctor_doc` with hand-crafted fixtures.

use std::path::Path;

use cfgd::cli::doctor::{
    DoctorConfigSource, DoctorExtras, DoctorProfilesDir, DoctorStateStore, build_doctor_doc,
};
use cfgd::cli::output_types::{
    DoctorConfigCheck, DoctorConfiguratorCheck, DoctorManagerCheck, DoctorModuleCheck,
    DoctorOutput, DoctorProviderCheck, DoctorSecretsCheck,
};
use cfgd_core::output_v2::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_fixture() -> (DoctorOutput, DoctorExtras) {
    let output = DoctorOutput {
        config: DoctorConfigCheck {
            valid: true,
            path: "/home/test/.config/cfgd/cfgd.yaml".into(),
            name: Some("test-host".into()),
            profile: Some("default".into()),
            error: None,
        },
        git: true,
        secrets: DoctorSecretsCheck {
            sops_available: true,
            sops_version: Some("3.8.1".into()),
            age_key_exists: true,
            age_key_path: Some("/home/test/.config/sops/age/keys.txt".into()),
            sops_config_exists: true,
            providers: vec![
                DoctorProviderCheck {
                    name: "1password".into(),
                    available: true,
                },
                DoctorProviderCheck {
                    name: "vault".into(),
                    available: false,
                },
            ],
        },
        package_managers: vec![
            DoctorManagerCheck {
                name: "cargo".into(),
                available: true,
                declared: true,
                can_bootstrap: false,
            },
            DoctorManagerCheck {
                name: "brew".into(),
                available: true,
                declared: false,
                can_bootstrap: false,
            },
        ],
        modules: vec![DoctorModuleCheck {
            name: "dotfiles".into(),
            valid: true,
            error: None,
        }],
        system_configurators: vec![DoctorConfiguratorCheck {
            name: "shell".into(),
            available: true,
        }],
    };
    let extras = DoctorExtras {
        state_store: Some(DoctorStateStore {
            accessible: true,
            message: None,
        }),
        profiles_dir: Some(DoctorProfilesDir {
            path: "/home/test/.config/cfgd/profiles".into(),
            exists: true,
            profile_count: 2,
        }),
        config_sources: Vec::new(),
    };
    (output, extras)
}

fn one_warn_fixture() -> (DoctorOutput, DoctorExtras) {
    let (mut output, mut extras) = happy_fixture();
    output.secrets.sops_available = false;
    output.secrets.sops_version = None;
    extras.config_sources = vec![DoctorConfigSource {
        name: "team-base".into(),
        cached_path: None,
    }];
    (output, extras)
}

fn one_fail_fixture() -> (DoctorOutput, DoctorExtras) {
    let (mut output, extras) = happy_fixture();
    output.git = false;
    (output, extras)
}

#[test]
fn doctor_happy_human() {
    let (output, extras) = happy_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_doctor_doc(&output, &extras));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "doctor/happy.txt");
}

#[test]
fn doctor_happy_json() {
    let (output, extras) = happy_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_doctor_doc(&output, &extras));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "doctor -o json must serialize exactly DoctorOutput (regression anchor)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "doctor/happy.json");
}

#[test]
fn doctor_one_warn_human() {
    let (output, extras) = one_warn_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_doctor_doc(&output, &extras));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "doctor/one_warn.txt");
}

#[test]
fn doctor_one_fail_human() {
    let (output, extras) = one_fail_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_doctor_doc(&output, &extras));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "doctor/one_fail.txt");
}
