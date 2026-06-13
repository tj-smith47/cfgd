//! Snapshot tests for `cfgd source show`.
//!
//! Cases:
//!   - `source_show/happy.{txt,json}` — populated source with state, managed
//!     resources, and a manifest carrying locked/required/recommended policy
//!     items (exercises every section + the nested Policy Summary subsections).
//!   - `source_show/empty.txt` — minimal source (no state, no resources, no
//!     manifest). Exercises `section_if_nonempty` skipping the Managed
//!     Resources block and the absence of optional kvs.
//!   - `source_show/locked_ref.txt` — State section with `lockedRef` and
//!     `lockedCommit` kvs rendered (lines 67-73 in show.rs).
//!   - `source_show/locked_policy.txt` — Manifest with a non-empty `locked`
//!     policy section (lines 114-123): verifies the Locked subsection renders.
//!   - `source_show/all_package_managers.txt` — `append_policy_items` with
//!     brew formulae, brew casks, apt, cargo, pipx, dnf, and npm packages
//!     (lines 155-183).
//!   - `source_show/not_found.txt` — source not found error path.
//!
//! Goldens live under `tests/output_snapshots/source_show/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_show_snapshots

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cfgd::cli::error::render_cli_error;
use cfgd::cli::output_types::{SourceResourceEntry, SourceShowOutput, SourceStateInfo};
use cfgd::cli::source::show::{build_source_not_found_error, build_source_show_doc};
use cfgd_core::config::{
    AptSpec, BrewSpec, CargoSpec, ConfigSourceDocument, ConfigSourceMetadata, ConfigSourcePolicy,
    ConfigSourceProvides, ConfigSourceSpec, EnvVar, ManagedFileSpec, NpmSpec, PackagesSpec,
    PolicyItems, SourceConstraints,
};
use cfgd_core::output::Printer;
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_output() -> SourceShowOutput {
    SourceShowOutput {
        name: "team-config".into(),
        url: "https://github.com/team/config".into(),
        branch: "main".into(),
        priority: 100,
        accept_recommended: true,
        profile: Some("team".into()),
        sync_interval: "1h".into(),
        auto_apply: false,
        pin_version: Some("v1.2.3".into()),
        state: Some(SourceStateInfo {
            status: "synced".into(),
            last_fetched: Some("2026-05-14T10:00:00Z".into()),
            last_commit: Some("deadbeef1234567890abcdef".into()),
            version: Some("3.1.0".into()),
            locked_ref: None,
            locked_commit: None,
        }),
        managed_resources: vec![
            SourceResourceEntry {
                resource_type: "package".into(),
                resource_id: "brew/curl".into(),
            },
            SourceResourceEntry {
                resource_type: "file".into(),
                resource_id: "~/.bashrc".into(),
            },
            SourceResourceEntry {
                resource_type: "env".into(),
                resource_id: "EDITOR".into(),
            },
        ],
        modules: vec!["dev-tools".into(), "shell".into()],
    }
}

fn happy_manifest() -> ConfigSourceDocument {
    ConfigSourceDocument {
        api_version: "cfgd.io/v1alpha1".into(),
        kind: "ConfigSource".into(),
        metadata: ConfigSourceMetadata {
            name: "team-config".into(),
            version: Some("3.1.0".into()),
            description: Some("Team-wide baseline".into()),
        },
        spec: ConfigSourceSpec {
            provides: ConfigSourceProvides {
                modules: vec!["dev-tools".into(), "shell".into()],
                ..ConfigSourceProvides::default()
            },
            policy: ConfigSourcePolicy {
                required: PolicyItems {
                    files: vec![ManagedFileSpec {
                        source: "bashrc".into(),
                        target: PathBuf::from("~/.bashrc"),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    env: vec![EnvVar {
                        name: "EDITOR".into(),
                        value: "nvim".into(),
                    }],
                    ..PolicyItems::default()
                },
                recommended: PolicyItems {
                    system: {
                        let mut m = HashMap::new();
                        m.insert(
                            "shellAliases".into(),
                            serde_yaml::Value::String("default".into()),
                        );
                        m
                    },
                    ..PolicyItems::default()
                },
                locked: PolicyItems::default(),
                optional: PolicyItems::default(),
                constraints: SourceConstraints::default(),
            },
        },
    }
}

fn empty_output() -> SourceShowOutput {
    SourceShowOutput {
        name: "minimal".into(),
        url: "https://github.com/team/minimal".into(),
        branch: "main".into(),
        priority: 500,
        accept_recommended: false,
        profile: None,
        sync_interval: "1h".into(),
        auto_apply: false,
        pin_version: None,
        state: None,
        managed_resources: Vec::new(),
        modules: Vec::new(),
    }
}

#[test]
fn source_show_happy_human() {
    let output = happy_output();
    let manifest = happy_manifest();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, Some(&manifest)));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_show/happy.txt");
}

#[test]
fn source_show_happy_json() {
    let output = happy_output();
    let manifest = happy_manifest();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, Some(&manifest)));
    drop(printer);
    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(output)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_show/happy.json");
}

#[test]
fn source_show_lists_delivered_modules_human_and_json() {
    // A source with `provides.modules` surfaces a Modules section in human
    // output and a `modules` array in the structured payload.
    let output = happy_output();
    let manifest = happy_manifest();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, Some(&manifest)));
    drop(printer);

    let human = cap.human();
    assert!(human.contains("Modules"), "human output: {human}");
    assert!(human.contains("dev-tools"), "human output: {human}");
    assert!(human.contains("shell"), "human output: {human}");

    let json = serde_json::to_value(&output).unwrap();
    assert_eq!(
        json["modules"],
        serde_json::json!(["dev-tools", "shell"]),
        "structured payload must list delivered modules: {json}"
    );
}

#[test]
fn source_show_no_modules_omits_field() {
    // Regression: a source delivering no modules omits the `modules` key entirely
    // (serde skip_serializing_if) and renders no Modules section.
    let output = empty_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, None));
    drop(printer);

    let human = cap.human();
    assert!(
        !human.contains("Modules"),
        "no Modules section expected: {human}"
    );
    let json = serde_json::to_value(&output).unwrap();
    assert!(
        json.get("modules").is_none(),
        "modules key must be omitted when empty: {json}"
    );
}

#[test]
fn source_show_empty_human() {
    let output = empty_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, None));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_show/empty.txt");
}

#[test]
fn source_show_not_found_human() {
    // `source show` of a missing source returns a not-found error; the central sink
    // (render_cli_error) renders the one ✗ line + the "Available sources" hint. Drive
    // the real sink so this golden pins exactly what a user sees on the failure path.
    let available = vec!["alpha".to_string(), "beta".to_string()];
    let (printer, cap) = Printer::for_test_doc();
    let err = build_source_not_found_error("missing", &available);
    render_cli_error(&printer, &err);
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_show/not_found.txt");
}

// ---------------------------------------------------------------------------
// locked_ref / locked_commit in State section (show.rs lines 67-73)
// ---------------------------------------------------------------------------

#[test]
fn source_show_state_with_locked_ref_and_commit() {
    // Exercises the `locked_ref` and `locked_commit` optional kv branches that
    // fire when a sources.lock entry is present. The happy fixture above always
    // passes `None` for both fields; this fixture provides real values.
    let output = SourceShowOutput {
        name: "pinned-source".into(),
        url: "https://github.com/team/pinned".into(),
        branch: "main".into(),
        priority: 200,
        accept_recommended: false,
        profile: None,
        sync_interval: "30m".into(),
        auto_apply: false,
        pin_version: Some("v2.0.0".into()),
        state: Some(SourceStateInfo {
            status: "synced".into(),
            last_fetched: Some("2026-06-01T12:00:00Z".into()),
            last_commit: Some("aabbccddeeff00112233445566778899aabbccdd".into()),
            version: Some("2.0.0".into()),
            locked_ref: Some("v2.0.0".into()),
            locked_commit: Some("aabbccddeeff00112233445566778899aabbccdd".into()),
        }),
        managed_resources: Vec::new(),
        modules: Vec::new(),
    };

    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, None));
    drop(printer);

    let human = cap.human();
    assert!(
        human.contains("Locked Ref"),
        "Locked Ref kv must appear: {human}"
    );
    assert!(
        human.contains("v2.0.0"),
        "locked ref value must appear: {human}"
    );
    assert!(
        human.contains("Locked Commit"),
        "Locked Commit kv must appear: {human}"
    );
    // Commit is truncated to SHORT_COMMIT_LEN (12); check the prefix.
    assert!(
        human.contains("aabbccddeeff"),
        "truncated locked commit must appear: {human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_show/locked_ref.txt");
}

// ---------------------------------------------------------------------------
// Locked policy section (show.rs lines 114-123)
// ---------------------------------------------------------------------------

fn manifest_with_locked_policy() -> ConfigSourceDocument {
    ConfigSourceDocument {
        api_version: "cfgd.io/v1alpha1".into(),
        kind: "ConfigSource".into(),
        metadata: ConfigSourceMetadata {
            name: "locked-source".into(),
            version: Some("1.0.0".into()),
            description: None,
        },
        spec: ConfigSourceSpec {
            provides: ConfigSourceProvides {
                modules: Vec::new(),
                ..ConfigSourceProvides::default()
            },
            policy: ConfigSourcePolicy {
                locked: PolicyItems {
                    env: vec![EnvVar {
                        name: "CORP_PROXY".into(),
                        value: "http://proxy.corp.example.com:8080".into(),
                    }],
                    ..PolicyItems::default()
                },
                required: PolicyItems::default(),
                recommended: PolicyItems::default(),
                optional: PolicyItems::default(),
                constraints: SourceConstraints::default(),
            },
        },
    }
}

#[test]
fn source_show_locked_policy_section_renders() {
    // Verifies the Locked subsection (show.rs lines 114-123) fires when the
    // manifest has a non-empty `policy.locked`. The happy fixture always passes
    // an empty locked block, so this branch was previously uncovered.
    let output = SourceShowOutput {
        name: "locked-source".into(),
        url: "https://github.com/corp/locked".into(),
        branch: "main".into(),
        priority: 50,
        accept_recommended: false,
        profile: None,
        sync_interval: "1h".into(),
        auto_apply: false,
        pin_version: None,
        state: None,
        managed_resources: Vec::new(),
        modules: Vec::new(),
    };
    let manifest = manifest_with_locked_policy();

    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, Some(&manifest)));
    drop(printer);

    let human = cap.human();
    assert!(
        human.contains("Locked"),
        "Locked subsection must render: {human}"
    );
    assert!(
        human.contains("env: CORP_PROXY"),
        "locked env entry must render: {human}"
    );
    // The Count kv must appear inside the Locked subsection.
    assert!(human.contains("Count"), "Count kv must appear: {human}");
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_show/locked_policy.txt");
}

// ---------------------------------------------------------------------------
// append_policy_items — all package manager branches (show.rs lines 155-183)
// ---------------------------------------------------------------------------

fn manifest_with_all_package_managers() -> ConfigSourceDocument {
    ConfigSourceDocument {
        api_version: "cfgd.io/v1alpha1".into(),
        kind: "ConfigSource".into(),
        metadata: ConfigSourceMetadata {
            name: "full-pkg-source".into(),
            version: Some("1.0.0".into()),
            description: Some("All package manager types".into()),
        },
        spec: ConfigSourceSpec {
            provides: ConfigSourceProvides {
                modules: Vec::new(),
                ..ConfigSourceProvides::default()
            },
            policy: ConfigSourcePolicy {
                required: PolicyItems {
                    packages: Some(PackagesSpec {
                        brew: Some(BrewSpec {
                            formulae: vec!["git".into(), "ripgrep".into()],
                            casks: vec!["iterm2".into()],
                            ..BrewSpec::default()
                        }),
                        apt: Some(AptSpec {
                            packages: vec!["build-essential".into()],
                            ..AptSpec::default()
                        }),
                        cargo: Some(CargoSpec {
                            packages: vec!["cargo-edit".into()],
                            ..CargoSpec::default()
                        }),
                        npm: Some(NpmSpec {
                            global: vec!["typescript".into()],
                            ..NpmSpec::default()
                        }),
                        pipx: vec!["black".into()],
                        dnf: vec!["gcc".into()],
                        ..PackagesSpec::default()
                    }),
                    ..PolicyItems::default()
                },
                locked: PolicyItems::default(),
                recommended: PolicyItems::default(),
                optional: PolicyItems::default(),
                constraints: SourceConstraints::default(),
            },
        },
    }
}

#[test]
fn source_show_all_package_manager_types_render() {
    // Exercises all branches of `append_policy_items` (show.rs lines 155-183):
    // brew formulae, brew casks, apt, cargo, pipx, dnf, and npm. Each branch was
    // previously uncovered because the happy fixture only exercises env + files +
    // system entries.
    let output = SourceShowOutput {
        name: "full-pkg-source".into(),
        url: "https://github.com/team/full-pkg".into(),
        branch: "main".into(),
        priority: 300,
        accept_recommended: true,
        profile: None,
        sync_interval: "1h".into(),
        auto_apply: false,
        pin_version: None,
        state: None,
        managed_resources: Vec::new(),
        modules: Vec::new(),
    };
    let manifest = manifest_with_all_package_managers();

    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_show_doc(&output, Some(&manifest)));
    drop(printer);

    let human = cap.human();

    // brew formulae (lines 157-159)
    assert!(
        human.contains("brew formula: git"),
        "brew formula 'git' must render: {human}"
    );
    assert!(
        human.contains("brew formula: ripgrep"),
        "brew formula 'ripgrep' must render: {human}"
    );
    // brew casks (lines 160-162)
    assert!(
        human.contains("brew cask: iterm2"),
        "brew cask must render: {human}"
    );
    // apt (lines 163-166)
    assert!(
        human.contains("apt: build-essential"),
        "apt package must render: {human}"
    );
    // cargo (lines 167-170)
    assert!(
        human.contains("cargo: cargo-edit"),
        "cargo package must render: {human}"
    );
    // pipx (lines 173-175)
    assert!(
        human.contains("pipx: black"),
        "pipx package must render: {human}"
    );
    // dnf (lines 176-178)
    assert!(
        human.contains("dnf: gcc"),
        "dnf package must render: {human}"
    );
    // npm (lines 179-183)
    assert!(
        human.contains("npm: typescript"),
        "npm package must render: {human}"
    );

    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "source_show/all_package_managers.txt",
    );
}

#[test]
fn source_show_not_found_empty_available_list() {
    // Edge case: not-found with no available sources (empty `available` slice).
    // The hint line "Available sources: ..." must be suppressed entirely.
    let available: Vec<String> = Vec::new();
    let (printer, cap) = Printer::for_test_doc();
    let err = build_source_not_found_error("ghost", &available);
    render_cli_error(&printer, &err);
    drop(printer);

    let human = cap.human();
    assert!(
        human.contains("ghost"),
        "source name must appear in error: {human}"
    );
    assert!(
        !human.contains("Available sources"),
        "Available sources hint must be suppressed when list is empty: {human}"
    );
}
