//! Snapshot tests for `cfgd config edit`.
//!
//! Cases:
//!   - `config_edit/valid.txt` — `EDITOR=/bin/true` leaves the valid config
//!     in place; post-edit validation lands in the "Configuration is valid"
//!     success arm.
//!   - `config_edit/validation_error_accept_retry.txt` — pre-stage an invalid
//!     config; an `/bin/sh` editor rewrites it to valid on each invocation;
//!     the prompt receives `Confirm(true)` so the retry pass succeeds.
//!   - `config_edit/validation_error_decline.txt` — pre-stage an invalid
//!     config + `EDITOR=/bin/true`; the prompt receives `Confirm(false)` so
//!     the command emits "Saved with validation errors".
//!
//! Unix-only: the editor-driving cases shell out to `/usr/bin/true` /
//! `/bin/sh -c '...'`. Goldens live under `tests/output_snapshots/config_edit/`.

#![cfg(unix)]

mod common;

use std::path::Path;

use cfgd::cli::config_cmd;
use cfgd_core::output::{Printer, PromptAnswer};
#[cfg(unix)]
use cfgd_core::test_helpers::EditorGuard;
#[cfg(unix)]
use serial_test::serial;

use common::{cli_for, config_test_setup};

const VALID_CONFIG: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n";

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

fn normalize_paths(raw: &str, config_dir: &Path) -> String {
    raw.replace(&config_dir.display().to_string(), "<CONFIG_DIR>")
}

#[cfg(unix)]
#[test]
#[serial]
fn config_edit_valid_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let _editor = EditorGuard::set("/usr/bin/true");
    config_cmd::cmd_config_edit(&cli, &printer).expect("valid config must succeed");
    drop(printer);

    let stripped = normalize_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "config_edit/valid.txt", &stripped);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["valid"], true);
}

#[cfg(unix)]
#[test]
#[serial]
fn config_edit_validation_error_accept_retry_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cfgd_yaml = config_dir.path().join("cfgd.yaml");
    std::fs::write(&cfgd_yaml, "not a Config document").unwrap();

    // Editor shim: write the valid config into "$1". Idempotent — invoked
    // twice (first opens with invalid contents; prompt accepts retry; second
    // pass writes valid content).
    let editor_script = config_dir.path().join("editor.sh");
    let script_body = format!("#!/bin/sh\ncat > \"$1\" <<'EOF'\n{VALID_CONFIG}EOF\n");
    std::fs::write(&editor_script, script_body).unwrap();
    let mut perms = std::fs::metadata(&editor_script).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&editor_script, perms).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    let _editor = EditorGuard::set(editor_script.to_str().unwrap());

    config_cmd::cmd_config_edit(&cli, &printer).expect("retry-accept path must succeed");
    drop(printer);

    let stripped = normalize_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "config_edit/validation_error_accept_retry.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["valid"], true);
}

#[cfg(unix)]
#[test]
#[serial]
fn config_edit_validation_error_decline_human() {
    let (config_dir, state_dir) = config_test_setup();
    std::fs::write(config_dir.path().join("cfgd.yaml"), "not a Config document").unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);
    let _editor = EditorGuard::set("/usr/bin/true");

    config_cmd::cmd_config_edit(&cli, &printer).expect("save-with-errors must return Ok");
    drop(printer);

    let stripped = normalize_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "config_edit/validation_error_decline.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["valid"], false);
}
