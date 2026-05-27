//! Snapshot tests for `cfgd profile update`.
//!
//! Cases:
//!   - `profile_update/happy.{txt,json}` — real `cmd_profile_update` adds
//!     one env var; status_simple stream + final buffered Doc.
//!   - `profile_update/no_changes.{txt,json}` — empty args list emits
//!     `Role::Info "No changes specified"`.
//!   - `profile_update/add_remove_mixed.txt` — add module + remove env +
//!     remove missing module (warning); final Doc summarizes.
//!   - `profile_update/add_module_remote_hybrid.txt` — adding a `file://`
//!     remote module URL exercises the T1→T3 hybrid pass-through to
//!     `module::cmd_module_add_remote`. Will refresh in T3 once registry
//!     migrates; pinning the current shape catches accidental drift in the
//!     hybrid bridge.
//!
//! Goldens live under `tests/output_snapshots/profile_update/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_update_snapshots

mod common;

use std::path::Path;

use cfgd::cli::profile::cmd_profile_update;
use cfgd_core::output::{Printer, PromptAnswer};
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;
use serial_test::serial;

use common::{
    cli_for, make_bare_module_repo, normalize_profile_paths, profile_test_config_setup,
    profile_update_args,
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

#[test]
fn profile_update_happy_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_update_args();
    args.env = vec!["EDITOR=nvim".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_update/happy.txt",
        &stripped,
    );
}

#[test]
fn profile_update_happy_json() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_update_args();
    args.env = vec!["EDITOR=nvim".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "default");
    assert_eq!(json["changes"], 1);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/happy.json");
}

#[test]
fn profile_update_no_changes_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = profile_update_args();

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_update/no_changes.txt",
        &stripped,
    );
}

#[test]
fn profile_update_no_changes_json() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = profile_update_args();

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["changes"], 0);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/no_changes.json");
}

#[test]
fn profile_update_add_remove_mixed_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_update_args();
    args.modules = vec!["nvim".to_string(), "-missing".to_string()];
    args.env = vec!["-EDITOR".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_update/add_remove_mixed.txt",
        &stripped,
    );
}

#[test]
#[serial]
fn profile_update_add_module_remote_hybrid_human() {
    // T1→T3 closed: `cmd_profile_update --module <file://...>` delegates to
    // `module::cmd_module_add_remote(cli, printer, ...)`.
    // The prompt queue drives the "Add this remote module?" / signature
    // confirmations through the unified Printer surface.
    let (config_dir, state_dir) = profile_test_config_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_module_repo(bare_root.path(), "mymod", "v1.0.0");
    let module_url = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    let mut args = profile_update_args();
    args.modules = vec![module_url.clone()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let cfg_file = config_dir.path().join("cfgd.yaml");
    let stripped = cfgd_core::normalize_for_snapshot(
        &strip_ansi(&cap.human()),
        &[
            (&bare, "<BARE>"),
            (bare_root.path(), "<BARE_ROOT>"),
            (&cfg_file, "<CONFIG_DIR>/cfgd.yaml"),
            (config_dir.path(), "<CONFIG_DIR>"),
        ],
    );
    // `to_file_url` emits `file:///<absolute-posix-path>` on every OS; on
    // Windows the bare path lacks a leading `/`, leaving the URL prefix's
    // third slash visible. Fold to the unix shape so one golden survives both.
    let stripped = stripped.replace("file:///<BARE>", "file://<BARE>");
    // Mask the 40-char hex commit SHA — git2 generates a new one each test run.
    let stripped = mask_commit_sha(&stripped);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_update/add_module_remote_hybrid.txt",
        &stripped,
    );
}

/// Replace any 40-char run of lowercase hex (a git commit SHA) with the literal
/// placeholder `<COMMIT_SHA>` so the snapshot is stable across runs. Walks
/// chars (not bytes) so multi-byte UTF-8 glyphs (✓, →) survive intact.
fn mask_commit_sha(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if i + 40 <= chars.len()
            && chars[i..i + 40]
                .iter()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric())
            && (i + 40 == chars.len() || !chars[i + 40].is_ascii_alphanumeric())
        {
            out.push_str("<COMMIT_SHA>");
            i += 40;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}
