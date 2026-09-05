//! Snapshot tests for `kubectl cfgd deploy`.
//!
//! Goldens live under `tests/output_snapshots/plugin_deploy/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test plugin_deploy_snapshots
//!
//! Print mode touches nothing outside the process, so it is captured directly.
//! `--apply` shells out to `kubectl`, which `plugin::kubectl` invokes by name,
//! so those cases run against a `kubectl` shim on PATH — the same seam
//! `cli/plugin/tests.rs` drives `inject` and `exec` through.

use std::path::Path;

use cfgd::cli::plugin;
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::{Printer, strip_ansi};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";
const PINNED: &str = "registry.example.com/myapp/server@sha256:3a7b9c4d0000000000000000000000000000000000000000000000000000abcd";

/// A pod manifest whose image volume carries the mutable tag the lockfile
/// below pins, plus a container that must survive the rewrite untouched.
const POD_YAML: &str = r#"apiVersion: v1
kind: Pod
metadata:
  name: myapp
spec:
  containers:
    - name: app
      image: registry.example.com/base:v1
  volumes:
    - name: app
      image:
        reference: registry.example.com/myapp/server:abc123
"#;

fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pod.yaml"), POD_YAML).expect("write manifest");
    std::fs::write(
        dir.path().join("cfgd-images.lock"),
        format!(
            "images:\n  - reference: registry.example.com/myapp/server:abc123\n    \
             digest: sha256:3a7b9c4d0000000000000000000000000000000000000000000000000000abcd\n    \
             pinned: {PINNED}\n    lockedAt: 2026-06-13T12:00:00Z\n"
        ),
    )
    .expect("write lockfile");
    dir
}

fn manifest_and_lock(dir: &Path) -> (Vec<String>, String) {
    (
        vec![dir.join("pod.yaml").to_string_lossy().into_owned()],
        dir.join("cfgd-images.lock").to_string_lossy().into_owned(),
    )
}

/// Print mode: stdout is the rewritten manifest, and the pin report goes to
/// stderr so the stdout side stays a clean pipe into `kubectl apply -f -`.
#[test]
fn plugin_deploy_print_mode_human() {
    let dir = fixture_dir();
    let (files, lock) = manifest_and_lock(dir.path());

    let (printer, cap) = Printer::for_test_doc();
    plugin::cmd_deploy(&printer, &files, &lock, false, "default").expect("deploy must succeed");
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plugin_deploy/print.txt",
        &strip_ansi(&cap.human()),
    );
}

#[test]
fn plugin_deploy_print_mode_json() {
    let dir = fixture_dir();
    let (files, lock) = manifest_and_lock(dir.path());

    // Print mode's payload only exists under a structured format: in human
    // mode stdout carries the manifest itself, which is the point of the mode.
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    plugin::cmd_deploy(&printer, &files, &lock, false, "default").expect("deploy must succeed");
    drop(printer);

    let mut json = cap.json().expect("deploy emits a data payload");
    // The manifest path is this run's tempdir; the rewrite it reports is not.
    json["files"] = serde_json::json!(["<DIR>/pod.yaml"]);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plugin_deploy/print.json",
        &serde_json::to_string_pretty(&json).expect("payload serializes"),
    );
}

/// `--apply` closes on the count of documents applied and references pinned —
/// the line `docs/image-pack.md` quotes as the end of the pack-then-deploy
/// walkthrough.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn plugin_deploy_apply_mode_human() {
    let (_bin_dir, _path_guard) =
        cfgd_core::test_helpers::install_named_path_shim("kubectl", 0, "", "");
    let dir = fixture_dir();
    let (files, lock) = manifest_and_lock(dir.path());

    let (printer, cap) = Printer::for_test_doc();
    plugin::cmd_deploy(&printer, &files, &lock, true, "prod").expect("deploy must succeed");
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plugin_deploy/applied.txt",
        &strip_ansi(&cap.human()),
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn plugin_deploy_apply_mode_json() {
    let (_bin_dir, _path_guard) =
        cfgd_core::test_helpers::install_named_path_shim("kubectl", 0, "", "");
    let dir = fixture_dir();
    let (files, lock) = manifest_and_lock(dir.path());

    let (printer, cap) = Printer::for_test_doc();
    plugin::cmd_deploy(&printer, &files, &lock, true, "prod").expect("deploy must succeed");
    drop(printer);

    let mut json = cap.json().expect("deploy emits a data payload");
    json["files"] = serde_json::json!(["<DIR>/pod.yaml"]);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plugin_deploy/applied.json",
        &serde_json::to_string_pretty(&json).expect("payload serializes"),
    );
}
