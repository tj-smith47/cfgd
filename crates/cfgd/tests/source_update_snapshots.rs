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
//!
//! Goldens live under `tests/output_snapshots/source_update/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_update_snapshots

mod common;

use std::path::Path;

use cfgd::cli::source::{cmd_source_add, cmd_source_update};
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

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();

    // Normalize CRLF→LF: windows captured output has \r\n; committed snapshot is LF.

    let actual_norm = actual.replace("\r\n", "\n");

    let expected_norm = expected.replace("\r\n", "\n");

    pretty_assertions::assert_eq!(actual_norm, expected_norm, "snapshot mismatch: {name}");
}

fn normalize_paths(
    raw: &str,
    bare: &std::path::Path,
    bare_root: &std::path::Path,
    config_dir: &std::path::Path,
    state_dir: &std::path::Path,
) -> String {
    let mut out = raw.to_string();
    out = out.replace(&bare.to_string_lossy().to_string(), "<BARE>");
    out = out.replace(&bare_root.to_string_lossy().to_string(), "<BARE_ROOT>");
    out = out.replace(&config_dir.to_string_lossy().to_string(), "<CONFIG_DIR>");
    out = out.replace(&state_dir.to_string_lossy().to_string(), "<STATE_DIR>");
    strip_git_sha_ranges(strip_spinner_duration(out))
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

    cmd_source_update(&cli, &printer, None).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_update/no_sources.txt",
        &stripped,
    );
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

    let result = cmd_source_update(&cli, &printer, Some("missing"));
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_update/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["name"], "missing");
}

#[test]
#[serial]
fn source_update_happy_human() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), "upd-src", None);
    let url = format!("file://{}", bare.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("upd-src".into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    let (printer, cap) = Printer::for_test_doc();
    cmd_source_update(&cli, &printer, Some("upd-src")).unwrap();
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot(
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
    let url = format!("file://{}", bare.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("upd-src".into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    let (printer, cap) = Printer::for_test_doc();
    cmd_source_update(&cli, &printer, Some("upd-src")).unwrap();
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
    let url = format!("file://{}", bare.display());

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
    cmd_source_update(&cli, &printer, Some("accept-src")).unwrap();
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot(
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
    assert_snapshot(
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
    cmd_source_update(&cli, &printer, Some("reject-src")).unwrap();
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot(
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
    let url = format!("file://{}", bare.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("bridge-upd".into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    let (printer, cap) = Printer::for_test_doc();
    cmd_source_update(&cli, &printer, Some("bridge-upd")).unwrap();
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
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_update/bridge.txt",
        &stripped,
    );
}
