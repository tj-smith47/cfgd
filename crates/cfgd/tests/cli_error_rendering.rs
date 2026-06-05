#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! End-to-end proof of the central CLI error sink (`render_cli_error`).
//!
//! These tests drive the REAL `cfgd` binary on failing commands — the only path
//! that traverses `main`'s dispatch and `render_cli_error`. Unit tests call `cmd_*`
//! handlers directly and never reach the sink, which is exactly why the systemic
//! double-print / json-stray-line / json-silent bug survived "completed" + reviews.
//! The invariants pinned here:
//!   - human mode: exactly ONE `✗` failure line on stderr (never two), with the
//!     error text;
//!   - structured mode (`-o json`): stdout is exactly ONE JSON object (parsing the
//!     whole stdout as a single value fails if a second was concatenated), NEVER
//!     silent on failure, and no `✗` human line leaks onto stdout;
//!   - the structured payload carries the expected `error` kind.

use assert_cmd::Command;

/// Minimal valid config dir (so a command reaches its own not-found logic rather
/// than failing earlier on missing config).
fn create_valid_config(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: base\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();
}

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::cargo_bin("cfgd")
        .unwrap()
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// stdout in structured mode must be exactly ONE JSON value — concatenated objects
/// (the duplication signature) make `from_str` fail on trailing input.
fn parse_single_json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "structured failure stdout must be exactly one JSON value (got error {e}): {stdout:?}"
        )
    })
}

#[test]
fn module_show_not_found_human_emits_exactly_one_fail_line() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let cfg = dir.path().join("cfgd.yaml");

    let (stdout, stderr, code) = run(&[
        "module",
        "show",
        "does-not-exist",
        "--config",
        cfg.to_str().unwrap(),
    ]);

    assert_ne!(code, Some(0), "missing module must fail");
    assert_eq!(
        stderr.matches('✗').count(),
        1,
        "exactly one ✗ failure line on stderr — stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("not found"),
        "failure text must say not found — stderr: {stderr:?}"
    );
    assert!(
        !stdout.contains('✗'),
        "no failure line on stdout in human mode — stdout: {stdout:?}"
    );
}

#[test]
fn module_show_not_found_json_emits_one_payload_never_silent() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let cfg = dir.path().join("cfgd.yaml");

    let (stdout, _stderr, code) = run(&[
        "module",
        "show",
        "does-not-exist",
        "--config",
        cfg.to_str().unwrap(),
        "-o",
        "json",
    ]);

    assert_ne!(code, Some(0), "missing module must fail");
    assert!(
        !stdout.trim().is_empty(),
        "structured failure must NOT be silent on stdout"
    );
    assert!(
        !stdout.contains('✗'),
        "no human ✗ line beside the json payload — stdout: {stdout:?}"
    );
    let v = parse_single_json(&stdout);
    assert_eq!(v["error"], "not_found", "payload error kind — got {v}");
    assert_eq!(v["name"], "does-not-exist");
}

#[test]
fn missing_config_human_emits_one_fail_line_and_init_hint() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nope").join("cfgd.yaml");

    let (stdout, stderr, code) = run(&["status", "--config", nonexistent.to_str().unwrap()]);

    // Exit 3 = NoConfig — proves cli_error_ctx's exit-code preservation survives the
    // real `std::process::exit`, end to end, not just at the unit level.
    assert_eq!(code, Some(3), "missing config must exit NoConfig(3)");
    assert_eq!(
        stderr.matches('✗').count(),
        1,
        "exactly one ✗ failure line on stderr — stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("cfgd init"),
        "missing-config failure must surface the `cfgd init` remediation hint — stderr: {stderr:?}"
    );
    assert!(
        !stdout.contains('✗'),
        "stdout clean in human mode: {stdout:?}"
    );
}

#[test]
fn missing_config_json_emits_one_payload_never_silent() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nope").join("cfgd.yaml");

    let (stdout, _stderr, code) = run(&[
        "status",
        "--config",
        nonexistent.to_str().unwrap(),
        "-o",
        "json",
    ]);

    assert_eq!(code, Some(3), "missing config must exit NoConfig(3)");
    assert!(
        !stdout.trim().is_empty(),
        "structured failure must NOT be silent on stdout"
    );
    let v = parse_single_json(&stdout);
    assert!(
        v.get("error").is_some(),
        "structured failure payload must carry an `error` key — got {v}"
    );
}
