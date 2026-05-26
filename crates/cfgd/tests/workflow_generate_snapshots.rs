//! Snapshot tests for `cfgd workflow generate`.
//!
//! Cases:
//!   - `workflow_generate/happy.{txt,json}` — generates a release workflow
//!     with one profile + one module; Doc carries `path`, `profiles`,
//!     `modules`.
//!   - `workflow_generate/no_profiles_warning.txt` — empty repo (no
//!     profiles or modules) → Role::Warn Doc with `skipped: true`.
//!   - `workflow_generate/skipped.txt` — workflow exists, --force not set,
//!     prompt declines → Role::Info Doc with `skipped: true`. The
//!     `cli_for` helper uses Quiet+NoColor; `prompt_confirm` on a
//!     Quiet-mode Printer with no queued responses returns Err which
//!     `unwrap_or(false)` maps to "do not overwrite".

mod common;

use std::path::Path;

use cfgd::cli::workflow;
use cfgd_core::output::{OutputFormat, Printer};

use common::{cli_for, workflow_empty_test_setup, workflow_test_setup};

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

fn normalize(raw: &str, config_dir: &Path) -> String {
    // After replacing the tempdir with <CONFIG_DIR>, fold Windows-style `\`
    // into `/` so the snapshot is platform-stable. `path.display()` emits
    // native separators on Windows; the snapshot fixture uses POSIX style.
    raw.replace(&config_dir.display().to_string(), "<CONFIG_DIR>")
        .replace('\\', "/")
}

#[test]
fn workflow_generate_happy_human() {
    let (config_dir, state_dir) = workflow_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    workflow::cmd_workflow_generate(&cli, &printer, true).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "workflow_generate/happy.txt",
        &stripped,
    );
}

#[test]
fn workflow_generate_happy_json() {
    let (config_dir, state_dir) = workflow_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

    workflow::cmd_workflow_generate(&cli, &printer, true).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["profiles"], serde_json::json!(["default"]));
    assert_eq!(json["modules"], serde_json::json!(["neovim"]));
    let path_normalized = json["path"].as_str().unwrap().replace('\\', "/");
    assert!(
        path_normalized.ends_with(".github/workflows/cfgd-release.yml"),
        "path key should point at the generated workflow file: {}",
        json["path"]
    );
}

#[test]
fn workflow_generate_no_profiles_warning_human() {
    let (config_dir, state_dir) = workflow_empty_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    workflow::cmd_workflow_generate(&cli, &printer, false).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "workflow_generate/no_profiles_warning.txt",
        &stripped,
    );

    let json = cap.json().expect("warn Doc carries with_data");
    assert_eq!(json["skipped"], true);
    assert!(json["profiles"].as_array().unwrap().is_empty());
    assert!(json["modules"].as_array().unwrap().is_empty());
}

#[test]
fn workflow_generate_skipped_human() {
    // Pre-create the workflow file so the prompt branch fires. With Quiet
    // Cli + no queued prompt response, prompt_confirm returns Err which
    // unwrap_or(false) maps to "do not overwrite" → Skipped doc.
    let (config_dir, state_dir) = workflow_test_setup();
    let workflow_path = config_dir
        .path()
        .join(".github")
        .join("workflows")
        .join("cfgd-release.yml");
    std::fs::create_dir_all(workflow_path.parent().unwrap()).unwrap();
    std::fs::write(&workflow_path, "# on-disk content\n").unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    workflow::cmd_workflow_generate(&cli, &printer, false).unwrap();
    drop(printer);

    // File still has on-disk content (the skip path didn't overwrite).
    let after = std::fs::read_to_string(&workflow_path).unwrap();
    assert_eq!(after, "# on-disk content\n");

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "workflow_generate/skipped.txt",
        &stripped,
    );

    let json = cap.json().expect("skip Doc carries with_data");
    assert_eq!(json["skipped"], true);
}
