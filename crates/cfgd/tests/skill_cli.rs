#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

/// `cfgd skill install --help` must list the author kinds (so the kind is
/// discoverable) and carry an `Examples:` block (the cfgd top-level-command
/// convention, regression-guarded by the ux-consistency audit).
#[test]
fn skill_help_lists_kinds_and_examples() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("module").and(predicate::str::contains("Examples:")));
}

/// `cfgd skill update` with neither a kind nor `--all` is an incoherent request
/// (nothing to update), constrained at the clap layer via `required_unless_present`.
#[test]
fn skill_update_requires_kind_or_all() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "update"])
        .assert()
        .failure();
}

/// `cfgd skill update --all` is the coherent "update everything" request and parses.
#[test]
fn skill_update_all_parses() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "update", "--all"])
        .assert()
        .success();
}

/// `kind` and `--all` together is contradictory and rejected at the clap layer
/// via `conflicts_with`.
#[test]
fn skill_update_kind_and_all_conflict() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "update", "module", "--all"])
        .assert()
        .failure();
}

/// The `-y` short for `--yes` works on `skill rm`, matching every other
/// destructive cfgd verb (`cfgd module rm -y`, `cfgd source rm -y`).
#[test]
fn skill_remove_accepts_short_yes() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "rm", "module", "-y"])
        .assert()
        .success();
}

/// Spawn `cfgd skill install <args>` with a hermetic HOME (off the real $HOME)
/// and CWD pinned to the given repo dir, returning the assertion handle.
fn install_in(
    repo: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
) -> assert_cmd::Command {
    let mut cmd = Command::cargo_bin("cfgd").unwrap();
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .current_dir(repo)
        .args(["skill", "install"])
        .args(args);
    cmd
}

/// Find one `results[]` entry by provider id in the structured install payload.
fn result_for<'a>(payload: &'a Value, provider: &str) -> &'a Value {
    payload["results"]
        .as_array()
        .expect("results is an array")
        .iter()
        .find(|r| r["provider"] == provider)
        .unwrap_or_else(|| panic!("no result row for provider {provider} in {payload}"))
}

/// Project-scope auto-detect installs only into the detected providers and
/// reports the undetected ones as skipped (`not detected`), exiting 0.
#[test]
fn install_project_scope_writes_detected_providers_only() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    // claude-code present at project scope (.claude/), gemini absent (no .gemini/).
    std::fs::create_dir_all(repo.path().join(".claude")).expect("mk .claude");

    let assert = install_in(repo.path(), home.path(), &["module", "-o", "json"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    // `kind` matches the canonical PascalCase resource-kind token (`kind:` in
    // YAML, and the `skill list` payload), not the lowercase command token.
    assert_eq!(payload["kind"], "Module");
    assert_eq!(payload["scope"], "project");

    let claude = result_for(&payload, "claude-code");
    assert_eq!(claude["status"], "installed", "claude-code should install");
    assert!(
        claude["path"]
            .as_str()
            .expect("path string")
            .ends_with(".claude/skills/cfgd-module/SKILL.md"),
        "unexpected claude path: {claude}"
    );
    assert!(
        repo.path()
            .join(".claude/skills/cfgd-module/SKILL.md")
            .exists(),
        "SKILL.md must be on disk"
    );

    let gemini = result_for(&payload, "gemini");
    assert_eq!(gemini["status"], "skipped", "gemini undetected → skipped");
    assert!(
        gemini["reason"]
            .as_str()
            .expect("reason string")
            .contains("not detected"),
        "unexpected gemini reason: {gemini}"
    );
}

/// A partial failure (one targeted provider's install fails while another
/// succeeds) reports per-provider status AND exits non-zero. Failure is
/// injected root-safely by occupying the failing provider's parent-dir path
/// with a regular FILE so `create_dir_all` fails with `NotADirectory` even as
/// uid 0.
#[test]
fn install_reports_failure_and_exits_nonzero_on_partial() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    // Both providers detected at project scope.
    std::fs::create_dir_all(repo.path().join(".claude")).expect("mk .claude");
    std::fs::create_dir_all(repo.path().join(".gemini")).expect("mk .gemini");

    // Sabotage claude-code: its target is .claude/skills/cfgd-module/SKILL.md, so
    // make .claude/skills a regular FILE — create_dir_all(parent) then fails.
    let skills_path = repo.path().join(".claude/skills");
    std::fs::write(&skills_path, b"not a directory").expect("write blocker file");

    let assert = install_in(repo.path(), home.path(), &["module", "-o", "json"])
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    let claude = result_for(&payload, "claude-code");
    assert_eq!(claude["status"], "failed", "sabotaged claude must fail");
    assert!(
        !claude["reason"].as_str().expect("reason").is_empty(),
        "failed row must carry a reason: {claude}"
    );

    let gemini = result_for(&payload, "gemini");
    assert_eq!(gemini["status"], "installed", "gemini must still succeed");
}
