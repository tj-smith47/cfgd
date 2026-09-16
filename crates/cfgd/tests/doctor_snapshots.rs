//! Snapshot tests for `cfgd doctor`.
//!
//! Goldens live under `tests/output_snapshots/doctor/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test doctor_snapshots
//!
//! The live `cmd_doctor` runs environment-dependent probes (`git`, `sops`,
//! state-store, profiles-dir scan, etc.). To keep snapshots stable across
//! hosts, these tests drive `build_doctor_doc` with hand-crafted fixtures.

use std::path::Path;

use cfgd::cli::doctor::{
    DoctorConfigSource, DoctorExtras, DoctorProfilesDir, DoctorStateStore, build_doctor_doc,
};
use cfgd::cli::output_types::{
    DoctorConfigCheck, DoctorConfigState, DoctorConfiguratorCheck, DoctorManagerCheck,
    DoctorModuleCheck, DoctorModuleManagerRoute, DoctorOutput, DoctorProviderCheck,
    DoctorSecretsCheck,
};
use cfgd_core::output::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_fixture() -> (DoctorOutput, DoctorExtras) {
    let output = DoctorOutput {
        config: DoctorConfigCheck {
            valid: true,
            path: "/home/test/.config/cfgd/cfgd.yaml".into(),
            name: Some("test-host".into()),
            profile: Some("default".into()),
            error: None,
            legacy_output_keys: Vec::new(),
            state: DoctorConfigState::Valid,
        },
        git: true,
        secrets: DoctorSecretsCheck {
            sops_available: true,
            sops_version: Some("3.8.1".into()),
            age_key_exists: true,
            age_key_path: Some("/home/test/.config/sops/age/keys.txt".into()),
            sops_config_exists: true,
            sops_config_path: Some("/home/test/.config/cfgd/.sops.yaml".into()),
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
                bootstrap_method: None,
                used_by_modules: 1,
            },
            DoctorManagerCheck {
                name: "brew".into(),
                available: true,
                declared: false,
                can_bootstrap: false,
                bootstrap_method: None,
                used_by_modules: 0,
            },
        ],
        modules: vec![DoctorModuleCheck {
            name: "dotfiles".into(),
            valid: true,
            error: None,
            managers: vec![DoctorModuleManagerRoute {
                name: "cargo".into(),
                available: true,
                package_count: 2,
            }],
            unresolved: vec![],
        }],
        system_configurators: vec![DoctorConfiguratorCheck {
            name: "shell".into(),
            available: true,
        }],
        profiles: Vec::new(),
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
            error: None,
        }),
        config_sources: Vec::new(),
        update_optout: None,
    };
    (output, extras)
}

fn one_warn_fixture() -> (DoctorOutput, DoctorExtras) {
    let (mut output, mut extras) = happy_fixture();
    output.secrets.sops_available = false;
    output.secrets.sops_version = None;
    // Exercise the auto-bootstrap-with-method path.
    output.package_managers.push(DoctorManagerCheck {
        name: "rustup".into(),
        available: false,
        declared: true,
        can_bootstrap: true,
        bootstrap_method: Some("curl".into()),
        used_by_modules: 0,
    });
    extras.config_sources = vec![DoctorConfigSource {
        name: "team-base".into(),
        cached_path: None,
    }];
    (output, extras)
}

fn one_fail_fixture() -> (DoctorOutput, DoctorExtras) {
    let (mut output, extras) = happy_fixture();
    output.git = false;
    // A module whose packages route to a manager this host does not have
    // should drive the overall failure summary, not just `git: not found`.
    output.modules[0].managers.push(DoctorModuleManagerRoute {
        name: "brew".into(),
        available: false,
        package_count: 3,
    });
    (output, extras)
}

fn bare_fixture() -> (DoctorOutput, DoctorExtras) {
    let output = DoctorOutput {
        config: DoctorConfigCheck {
            valid: true,
            path: "/home/test/.config/cfgd/cfgd.yaml".into(),
            name: Some("test-host".into()),
            profile: Some("default".into()),
            error: None,
            legacy_output_keys: Vec::new(),
            state: DoctorConfigState::Valid,
        },
        git: true,
        secrets: DoctorSecretsCheck {
            sops_available: true,
            sops_version: Some("3.8.1".into()),
            age_key_exists: true,
            age_key_path: Some("/home/test/.config/sops/age/keys.txt".into()),
            sops_config_exists: true,
            sops_config_path: Some("/home/test/.config/cfgd/.sops.yaml".into()),
            providers: vec![],
        },
        package_managers: vec![],
        modules: vec![],
        system_configurators: vec![],
        profiles: Vec::new(),
    };
    let extras = DoctorExtras {
        state_store: Some(DoctorStateStore {
            accessible: true,
            message: None,
        }),
        profiles_dir: Some(DoctorProfilesDir {
            path: "/home/test/.config/cfgd/profiles".into(),
            exists: true,
            profile_count: 0,
            error: None,
        }),
        config_sources: vec![],
        update_optout: None,
    };
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

/// A document still carrying a pre-`spec.output` flat key gets one Warn row
/// per key, naming the nested key that replaced it and the command that
/// writes it. A migrated document renders none of them.
#[test]
fn doctor_reports_each_legacy_presentation_key_as_a_warn_row() {
    let (mut output, extras) = happy_fixture();
    output.config.legacy_output_keys = vec!["spec.theme".into(), "spec.usageHints".into()];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_doctor_doc(&output, &extras));
    drop(printer);
    let human = cap.human();
    for (old, new) in cfgd_core::config::LEGACY_OUTPUT_KEYS {
        assert!(
            human.contains(old) && human.contains(new),
            "expected a row naming {old} and {new}, got:\n{human}"
        );
    }
    assert!(
        human.contains("cfgd config set output.theme"),
        "the row must name the command that writes the new key, got:\n{human}"
    );

    let (clean, extras) = happy_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_doctor_doc(&clean, &extras));
    drop(printer);
    assert!(
        !cap.human().contains("spec.theme"),
        "a migrated document names no legacy key"
    );
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

#[test]
fn doctor_bare_human() {
    let (output, extras) = bare_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_doctor_doc(&output, &extras));
    drop(printer);
    let human = cap.human();
    // The bare fixture has empty package_managers / modules / config_sources,
    // so none of those section headers should appear in the rendered output.
    assert!(
        !human.contains("Package Managers"),
        "bare output must omit Package Managers section, got:\n{human}"
    );
    assert!(
        !human.contains("Modules"),
        "bare output must omit Modules section, got:\n{human}"
    );
    assert!(
        !human.contains("Config Sources"),
        "bare output must omit Config Sources section, got:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "doctor/bare.txt");
}
