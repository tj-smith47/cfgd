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
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_update_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::profile::cmd_profile_update;
use cfgd_core::output::Printer as PrinterV1;
use cfgd_core::output_v2::Printer;
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

#[test]
fn profile_update_happy_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let old_printer = PrinterV1::new(cfgd_core::output::Verbosity::Quiet);
    let (v2_printer, cap) = Printer::for_test_doc();
    let mut args = profile_update_args();
    args.env = vec!["EDITOR=nvim".to_string()];

    cmd_profile_update(&cli, &old_printer, &v2_printer, "default", &args).unwrap();
    drop(v2_printer);

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
    let old_printer = PrinterV1::new(cfgd_core::output::Verbosity::Quiet);
    let (v2_printer, cap) = Printer::for_test_doc();
    let mut args = profile_update_args();
    args.env = vec!["EDITOR=nvim".to_string()];

    cmd_profile_update(&cli, &old_printer, &v2_printer, "default", &args).unwrap();
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "default");
    assert_eq!(json["changes"], 1);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/happy.json");
}

#[test]
fn profile_update_no_changes_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let old_printer = PrinterV1::new(cfgd_core::output::Verbosity::Quiet);
    let (v2_printer, cap) = Printer::for_test_doc();
    let args = profile_update_args();

    cmd_profile_update(&cli, &old_printer, &v2_printer, "default", &args).unwrap();
    drop(v2_printer);

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
    let old_printer = PrinterV1::new(cfgd_core::output::Verbosity::Quiet);
    let (v2_printer, cap) = Printer::for_test_doc();
    let args = profile_update_args();

    cmd_profile_update(&cli, &old_printer, &v2_printer, "default", &args).unwrap();
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["changes"], 0);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/no_changes.json");
}

#[test]
fn profile_update_add_remove_mixed_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let old_printer = PrinterV1::new(cfgd_core::output::Verbosity::Quiet);
    let (v2_printer, cap) = Printer::for_test_doc();
    let mut args = profile_update_args();
    args.modules = vec!["nvim".to_string(), "-missing".to_string()];
    args.env = vec!["-EDITOR".to_string()];

    cmd_profile_update(&cli, &old_printer, &v2_printer, "default", &args).unwrap();
    drop(v2_printer);

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
    // T1→T3 hybrid: `cmd_profile_update --module <file://...>` saves the
    // current Doc, then delegates to `module::cmd_module_add_remote(cli,
    // printer, ...)` — passing the v1 `&Printer`. T3 closes both ends.
    let (config_dir, state_dir) = profile_test_config_setup();
    let _home = cfgd_core::with_test_home_guard(config_dir.path());
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_module_repo(bare_root.path(), "mymod", "v1.0.0");
    let module_url = format!("file://{}@v1.0.0", bare.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    // The v1 printer carries the prompt queue for `module::cmd_module_add_remote`'s
    // "Add this remote module?" / signature-policy prompts; `yes=false` is
    // hard-coded in the hybrid call site, so the queued Confirm(true) drives
    // past them. T3 will switch this to v2 prompts when registry migrates.
    let (old_printer, _v1_buf) = PrinterV1::for_test_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(true),
        cfgd_core::output::PromptAnswer::Confirm(true),
        cfgd_core::output::PromptAnswer::Confirm(true),
    ]);
    let (v2_printer, cap) = Printer::for_test_doc();
    let mut args = profile_update_args();
    args.modules = vec![module_url.clone()];

    cmd_profile_update(&cli, &old_printer, &v2_printer, "default", &args).unwrap();
    drop(v2_printer);

    let mut stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    // Strip the bare-repo path so the golden is host-stable.
    stripped = stripped.replace(&bare.to_string_lossy().to_string(), "<BARE>");
    stripped = stripped.replace(
        &bare_root.path().to_string_lossy().to_string(),
        "<BARE_ROOT>",
    );
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_update/add_module_remote_hybrid.txt",
        &stripped,
    );
}
