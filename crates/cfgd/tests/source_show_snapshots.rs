//! Snapshot tests for `cfgd source show`.
//!
//! Three cases:
//!   - `source_show/happy.{txt,json}` — populated source with state, managed
//!     resources, and a manifest carrying locked/required/recommended policy
//!     items (exercises every section + the nested Policy Summary subsections).
//!   - `source_show/empty.txt` — minimal source (no state, no resources, no
//!     manifest). Exercises `section_if_nonempty` skipping the Managed
//!     Resources block and the absence of optional kvs.
//!
//! Goldens live under `tests/output_snapshots/source_show/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_show_snapshots

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cfgd::cli::error::render_cli_error;
use cfgd::cli::output_types::{SourceResourceEntry, SourceShowOutput, SourceStateInfo};
use cfgd::cli::source::show::{build_source_not_found_error, build_source_show_doc};
use cfgd_core::config::{
    ConfigSourceDocument, ConfigSourceMetadata, ConfigSourcePolicy, ConfigSourceProvides,
    ConfigSourceSpec, EnvVar, ManagedFileSpec, PolicyItems, SourceConstraints,
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
