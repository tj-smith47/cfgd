//! Snapshot tests for `cfgd source update`.
//!
//! Cases:
//!   - `source_update/no_sources.txt` — `cmd_source_update` against an empty
//!     `cfgd.yaml` emits Role::Info "No sources configured".
//!   - `source_update/not_found.txt` — error-path Doc when the named source
//!     isn't in `cfgd.yaml`.
//!   - `source_update/happy.{txt,json}` — real `cmd_source_update` after a
//!     successful `cmd_source_add` against a local bare repo; no permission
//!     changes, takes the no-prompt success branch.
//!   - `source_update/accept.{txt,json}` — Accept-confirm-then-success
//!     pattern: a v2 manifest with expanded permissions is published to the
//!     bare; the prompt receives `Confirm(true)` and `cmd_source_update`
//!     emits the canonical Updated line nested under the per-source
//!     section. The JSON snapshot normalises the non-deterministic
//!     `commit` SHA to `<SHA>` so the golden stays stable across runs.
//!   - `source_update/rejection.txt` — same fixture, prompt receives
//!     `Confirm(false)`; emits the "permission changes rejected" skip line.
//!   - `source_update/bridge.txt` — streaming-to-buffered bridge invariant.
//!   - `source_update/knob_on_failed_fetch.txt` — a failed fetch and a
//!     `--require-signed-commits` write in one invocation: both rows under ONE
//!     `source:<name>` owner section, and the knob lands on disk anyway.
//!
//! Goldens live under `tests/output_snapshots/source_update/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_update_snapshots

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::source::{cmd_source_add, cmd_source_update};
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::{Printer, PromptAnswer};
use serial_test::serial;

use common::{
    cli_for, make_bare_source_repo, push_replacement_manifest_to_bare, source_add_args,
    source_test_config_setup, source_test_config_with_source_setup,
};

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

fn normalize_paths(
    raw: &str,
    bare: &std::path::Path,
    bare_root: &std::path::Path,
    config_dir: &std::path::Path,
    state_dir: &std::path::Path,
) -> String {
    let normalized = cfgd_core::normalize_for_snapshot(
        raw,
        &[
            (bare, "<BARE>"),
            (bare_root, "<BARE_ROOT>"),
            (config_dir, "<CONFIG_DIR>"),
            (state_dir, "<STATE_DIR>"),
        ],
    );
    // `to_file_url` emits `file:///<absolute-posix-path>` on every OS; on
    // Windows the substituted path lacks a leading `/`, leaving the URL
    // prefix's third slash visible (`file:///<PLACEHOLDER>`). Fold to the
    // unix shape so a single golden survives both platforms.
    let folded = normalized
        .replace("file:///<BARE>", "file://<BARE>")
        .replace("file:///<BARE_ROOT>", "file://<BARE_ROOT>");
    strip_git_sha_ranges(strip_spinner_duration(folded))
}

/// Normalize git short-SHA ranges like `56f028c..865147c` to `<SHA>..<SHA>` so
/// goldens that include `git fetch` output stay stable across repo regenerations.
fn strip_git_sha_ranges(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(idx) = rest.find("..") {
        // Look back for a run of hex digits (the "from" SHA).
        let prefix = &rest[..idx];
        let from_start = prefix
            .rfind(|c: char| !c.is_ascii_hexdigit())
            .map(|i| i + 1)
            .unwrap_or(0);
        let from_len = prefix.len() - from_start;
        // Look forward for a run of hex digits (the "to" SHA).
        let after = &rest[idx + 2..];
        let to_len = after
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(after.len());
        if (7..=40).contains(&from_len) && (7..=40).contains(&to_len) {
            out.push_str(&prefix[..from_start]);
            out.push_str("<SHA>..<SHA>");
            rest = &after[to_len..];
            continue;
        }
        out.push_str(prefix);
        out.push_str("..");
        rest = after;
    }
    out.push_str(rest);
    out
}

use cfgd_core::output::test_capture::strip_spinner_duration;

#[test]
fn source_update_no_sources_human() {
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_update(&cli, &printer, None, Default::default()).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/no_sources.txt",
        &stripped,
    );
}

/// A source that cannot be fetched settles ONE row under its own
/// `source:<name>` owner heading, and the row's detail states the cause once —
/// no `source error:` category prefix, and no copy of the name the heading
/// directly above it already carries. The retry hint below the row is the one
/// place the name comes back, because it is a command the operator runs.
#[test]
#[serial]
fn source_update_source_failure_human() {
    let _disallow = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_ALLOW_LOCAL_SOURCES");
    let (config_dir, state_dir) = common::unreachable_source_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let errors = cfgd::cli::source::run_source_update(
        &cli,
        &printer,
        Some("missing-team"),
        Default::default(),
    )
    .expect("a fetch failure is reported, never bubbled");
    drop(printer);
    assert_eq!(errors, 1, "the failed source must be counted");

    let stripped = strip_ansi(&cfgd_core::normalize_for_snapshot(
        &cap.human(),
        &[
            (config_dir.path(), "<CONFIG_DIR>"),
            (state_dir.path(), "<STATE_DIR>"),
        ],
    ));
    assert!(
        !stripped.contains("source error:"),
        "the category prefix must be gone: {stripped}"
    );
    let failure_row = stripped
        .lines()
        .find(|l| l.contains("Update failed"))
        .unwrap_or_default();
    assert!(
        !failure_row.contains("missing-team"),
        "the failure row states the cause only; the heading above it names the source: {stripped}"
    );
    // The retry hint is a command the operator runs, so it carries the name
    // even though the heading already did.
    assert_eq!(
        stripped.matches("missing-team").count(),
        2,
        "the source is named by its owner heading and by the retry command: {stripped}"
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/source_failure.txt",
        &stripped,
    );
}

/// A fetch failure and a subscription write in ONE invocation: both rows
/// belong to the same `source:<name>` owner section, and the knob is written
/// even though the fetch failed — it records a demand on FUTURE fetches.
#[test]
#[serial]
fn source_update_failed_fetch_still_writes_the_knob_human() {
    let _disallow = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_ALLOW_LOCAL_SOURCES");
    let (config_dir, state_dir) = common::unreachable_source_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let errors = cfgd::cli::source::run_source_update(
        &cli,
        &printer,
        Some("missing-team"),
        cfgd::cli::source::SubscriptionEdits {
            require_signed_commits: Some(true),
            allow_scripts: None,
        },
    )
    .expect("a fetch failure is reported, never bubbled");
    drop(printer);
    assert_eq!(errors, 1);

    let stripped = strip_ansi(&cfgd_core::normalize_for_snapshot(
        &cap.human(),
        &[
            (config_dir.path(), "<CONFIG_DIR>"),
            (state_dir.path(), "<STATE_DIR>"),
        ],
    ));
    assert_eq!(
        stripped.matches("source:missing-team").count(),
        1,
        "both rows hang off ONE owner section: {stripped}"
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/knob_on_failed_fetch.txt",
        &stripped,
    );

    let raw = std::fs::read_to_string(config_dir.path().join("cfgd.yaml")).expect("read config");
    let written: serde_yaml::Value = serde_yaml::from_str(&raw).expect("parse config");
    assert_eq!(
        written["spec"]["sources"][0]["subscription"]["requireSignedCommits"],
        serde_yaml::Value::Bool(true),
        "the knob must be on disk: {raw}"
    );
}

#[test]
#[serial]
fn source_update_failed_fetch_still_writes_the_knob_json() {
    let _disallow = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_ALLOW_LOCAL_SOURCES");
    let (config_dir, state_dir) = common::unreachable_source_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cfgd::cli::source::run_source_update(
        &cli,
        &printer,
        Some("missing-team"),
        cfgd::cli::source::SubscriptionEdits {
            require_signed_commits: Some(true),
            allow_scripts: None,
        },
    )
    .expect("a fetch failure is reported, never bubbled");
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["errors"], 1);
    assert_eq!(json["sources"][0]["name"], "missing-team");
    assert_eq!(json["sources"][0]["status"], "error");
    assert_eq!(json["subscription"]["requireSignedCommits"], true);
}

#[test]
fn source_update_not_found_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_source_update(&cli, &printer, Some("missing"), Default::default())
        .expect_err("missing source must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/not_found.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    assert_eq!(meta.name, "missing");
}

#[test]
#[serial]
fn source_update_happy_human() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), "upd-src", None);
    let url = cfgd_core::to_file_url(&bare);

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("upd-src".into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    let (printer, cap) = Printer::for_test_doc();
    cmd_source_update(&cli, &printer, Some("upd-src"), Default::default()).unwrap();
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/happy.txt",
        &stripped,
    );
}

#[test]
#[serial]
fn source_update_happy_json() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), "upd-src", None);
    let url = cfgd_core::to_file_url(&bare);

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("upd-src".into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    let (printer, cap) = Printer::for_test_doc();
    cmd_source_update(&cli, &printer, Some("upd-src"), Default::default()).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["updated"], 1);
    assert_eq!(json["errors"], 0);
}

/// Stage a bare source, subscribe, then publish a v2 manifest that expands
/// `policy.required.modules` from 0 to 2 items. Returns the configured
/// fixture so per-test prompt-response wiring drives the perm-change arm.
fn perm_change_fixture(
    source_name: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
) {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), source_name, None);
    let url = cfgd_core::to_file_url(&bare);

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some(source_name.into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    // Publish a v2 manifest with expanded policy. required.modules grows
    // from 0 → 2 — detect_permission_changes will flag this.
    let v2 = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: {source_name}\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n  policy:\n    required:\n      modules:\n        - mod-a\n        - mod-b\n"
    );
    push_replacement_manifest_to_bare(bare_root.path(), &bare, &v2);

    (config_dir, state_dir, bare_root, bare)
}

#[test]
#[serial]
fn source_update_accept_human() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir, bare_root, bare) = perm_change_fixture("accept-src");

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    cmd_source_update(&cli, &printer, Some("accept-src"), Default::default()).unwrap();
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/accept.txt",
        &stripped,
    );

    let mut json = cap.json().expect("doc captured json");
    assert_eq!(json["updated"], 1);
    assert_eq!(json["skipped"], 0);
    assert_eq!(json["errors"], 0);
    // Normalise the non-deterministic per-source commit SHA so the golden
    // is stable across fixture runs.
    for src in json["sources"].as_array_mut().expect("sources array") {
        if src["commit"].is_string() {
            src["commit"] = serde_json::Value::String("<SHA>".into());
        }
    }
    let json_pretty = serde_json::to_string_pretty(&json).unwrap();
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/accept.json",
        &json_pretty,
    );
}

#[test]
#[serial]
fn source_update_rejection_human() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir, bare_root, bare) = perm_change_fixture("reject-src");

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);
    cmd_source_update(&cli, &printer, Some("reject-src"), Default::default()).unwrap();
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/rejection.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["updated"], 0);
    assert_eq!(json["skipped"], 1);
}

#[test]
#[serial]
fn source_update_bridge_one_blank_line() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), "bridge-upd", None);
    let url = cfgd_core::to_file_url(&bare);

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("bridge-upd".into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    let (printer, cap) = Printer::for_test_doc();
    cmd_source_update(&cli, &printer, Some("bridge-upd"), Default::default()).unwrap();
    drop(printer);

    let combined = cap.human();
    assert!(
        combined.contains("\n\n"),
        "bridge missing blank line: {combined}"
    );
    assert!(
        !combined.contains("\n\n\n"),
        "bridge has duplicate blank line: {combined}"
    );

    let stripped = normalize_paths(
        &strip_ansi(&combined),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "source_update/bridge.txt",
        &stripped,
    );
}
