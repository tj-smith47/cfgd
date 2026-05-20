//! Snapshot tests for `kubectl cfgd <subcommand>` (the plugin subcommands).
//!
//! Coverage strategy:
//!   The plugin's "happy" surfaces all require a live Kubernetes API server
//!   (`kube::Client::try_default()` + `pods.patch_ephemeral_containers`,
//!   `kube::Api::list`) plus `kubectl exec` / `kubectl patch` subprocesses.
//!   These are intractable from an integration test that runs under
//!   `cargo test` on a developer workstation or CI runner that doesn't have
//!   kubeconfig or `kubectl`. Per the F3 README's "Shimmed surfaces"
//!   carve-out, the happy paths are covered E2E by `tests/cli_integration.rs`
//!   and the cluster smoke tests.
//!
//!   What we DO snapshot here are the deterministic validation/error
//!   branches that never touch kube/kubectl: missing-module, command-required,
//!   and invalid-resource-format. These all exit via the `error_doc(...)`
//!   path so the JSON payload carries a stable `error` kind even on failure.

mod common;

use std::path::Path;

use cfgd::cli::plugin;
use cfgd_core::output::{OutputFormat, Printer};
use cfgd_core::test_helpers::EnvVarGuard;
use serial_test::serial;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    pretty_assertions::assert_eq!(actual, expected, "snapshot mismatch: {name}");
}

// --- cmd_debug error branches ---

#[test]
fn plugin_debug_module_required_human() {
    let (v2_printer, cap) = Printer::for_test_doc();

    let err = plugin::cmd_debug(&v2_printer, "mypod", &[], "default", "ubuntu:22.04");
    assert!(err.is_err());
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "plugin_debug/module_required.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "module_required");
    assert_eq!(json["pod"], "mypod");
}

#[test]
fn plugin_debug_module_required_json() {
    let (v2_printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

    let err = plugin::cmd_debug(&v2_printer, "mypod", &[], "default", "ubuntu:22.04");
    assert!(err.is_err());
    drop(v2_printer);

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "module_required");
    assert_eq!(json["pod"], "mypod");
    assert_eq!(json["namespace"], "default");
}

// --- cmd_exec error branches ---

#[test]
fn plugin_exec_module_required_human() {
    let (v2_printer, cap) = Printer::for_test_doc();

    let err = plugin::cmd_exec(&v2_printer, "mypod", &[], "default", &["ls".into()]);
    assert!(err.is_err());
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "plugin_exec/module_required.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "module_required");
}

#[test]
fn plugin_exec_command_required_human() {
    let (v2_printer, cap) = Printer::for_test_doc();

    let modules = vec!["nettools:1.0.0".to_string()];
    let err = plugin::cmd_exec(&v2_printer, "mypod", &modules, "default", &[]);
    assert!(err.is_err());
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "plugin_exec/command_required.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "command_required");
}

// --- cmd_inject error branches ---

#[test]
fn plugin_inject_module_required_human() {
    let (v2_printer, cap) = Printer::for_test_doc();

    let err = plugin::cmd_inject(&v2_printer, "deployment/myapp", &[], "default");
    assert!(err.is_err());
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "plugin_inject/module_required.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "module_required");
    assert_eq!(json["resource"], "deployment/myapp");
}

#[test]
fn plugin_inject_invalid_resource_human() {
    let (v2_printer, cap) = Printer::for_test_doc();

    let modules = vec!["tool:1.0".to_string()];
    let err = plugin::cmd_inject(&v2_printer, "bad-format-no-slash", &modules, "default");
    assert!(err.is_err());
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "plugin_inject/invalid_resource.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "invalid_resource");
}

// --- cmd_version disconnected branch ---

/// `cmd_version` falls back to `"not connected"` when `kube::Client::try_default()`
/// or `apiserver_version()` fails. Point `KUBECONFIG` at a nonexistent path
/// so the kube client construction short-circuits — deterministic, no live
/// cluster required.
#[test]
#[serial]
fn plugin_version_disconnected_human() {
    let _kubeconfig = EnvVarGuard::set("KUBECONFIG", "/tmp/nonexistent-kubeconfig");
    let (v2_printer, cap) = Printer::for_test_doc();

    plugin::cmd_version(&v2_printer).unwrap();
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "plugin_version/disconnected.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["kubectl"], "not connected");
    assert!(json["version"].as_str().is_some());
    assert!(json["cfgd"].as_str().is_some());
}
