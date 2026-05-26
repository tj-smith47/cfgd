//! Snapshot tests for `cfgd source edit`.
//!
//! Cases:
//!   - `source_edit/no_config.txt` — error-path Doc when `cfgd-source.yaml`
//!     doesn't exist.
//!   - `source_edit/valid.txt` — `cmd_source_edit` against a valid pre-staged
//!     manifest + `EDITOR=/bin/true` (no-op editor) emits the "Source
//!     manifest is valid" success Doc.
//!   - `source_edit/validation_error_accept_retry.txt` — pre-stage an invalid
//!     manifest, route through a shell-editor that rewrites the file to a
//!     valid manifest on the second pass; the prompt receives `Confirm(true)`
//!     to retry, and the second validation passes.
//!   - `source_edit/validation_error_decline.txt` — pre-stage an invalid
//!     manifest + `EDITOR=/bin/true`; the prompt receives `Confirm(false)` so
//!     `cmd_source_edit` emits "Saved with validation errors".
//!
//! Unix-only: the editor-driving cases shell out to `/usr/bin/true` /
//! `/bin/sh -c '...'`. Goldens live under `tests/output_snapshots/source_edit/`.
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_edit_snapshots

#![cfg(unix)]

mod common;

use std::path::Path;

use cfgd::cli::source::cmd_source_edit;
use cfgd_core::output::{Printer, PromptAnswer};
#[cfg(unix)]
use cfgd_core::test_helpers::EditorGuard;
#[cfg(unix)]
use serial_test::serial;

use common::{cli_for, normalize_profile_paths, source_test_config_setup};

const VALID_MANIFEST: &str = "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: edit-src\nspec:\n  provides:\n    profiles:\n      - default\n";

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

#[cfg(unix)]
#[test]
#[serial]
fn source_edit_valid_human() {
    // EDITOR=/bin/true exits 0 without touching the file, so the post-edit
    // validation reads the same valid manifest we wrote and lands in the
    // "Source manifest is valid" success arm.
    let (config_dir, state_dir) = source_test_config_setup();
    std::fs::write(config_dir.path().join("cfgd-source.yaml"), VALID_MANIFEST).unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let _editor = EditorGuard::set("/usr/bin/true");
    cmd_source_edit(&cli, &printer).expect("valid manifest must succeed");
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "source_edit/valid.txt", &stripped);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["valid"], true);
}

#[cfg(unix)]
#[test]
#[serial]
fn source_edit_validation_error_accept_retry_human() {
    // Pre-stage an invalid manifest. EDITOR is a /bin/sh script that
    // overwrites the file with a valid manifest on every invocation. The
    // first validation pass fails; the prompt receives Confirm(true), the
    // editor runs again and rewrites the file; the second pass succeeds.
    let (config_dir, state_dir) = source_test_config_setup();
    let source_path = config_dir.path().join("cfgd-source.yaml");
    std::fs::write(&source_path, "not a ConfigSource document").unwrap();

    // Editor shim: write the valid manifest into "$1" (the file path that
    // open_in_editor passes to $EDITOR). Repeated invocations are
    // idempotent — the second pass writes the same valid content and
    // validation succeeds.
    let editor_script = config_dir.path().join("editor.sh");
    let script_body = format!("#!/bin/sh\ncat > \"$1\" <<'EOF'\n{VALID_MANIFEST}EOF\n");
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

    cmd_source_edit(&cli, &printer).expect("retry-accept path must succeed");
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_edit/validation_error_accept_retry.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["valid"], true);
}

#[cfg(unix)]
#[test]
#[serial]
fn source_edit_validation_error_decline_human() {
    // Pre-stage an invalid manifest + EDITOR=/bin/true → editor is a no-op,
    // validation fails, prompt receives Confirm(false), command exits via
    // the "Saved with validation errors" Doc.
    let (config_dir, state_dir) = source_test_config_setup();
    std::fs::write(
        config_dir.path().join("cfgd-source.yaml"),
        "not a ConfigSource document",
    )
    .unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);
    let _editor = EditorGuard::set("/usr/bin/true");

    cmd_source_edit(&cli, &printer).expect("save-with-errors must return Ok");
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_edit/validation_error_decline.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["valid"], false);
}

#[test]
fn source_edit_no_config_human() {
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = cmd_source_edit(&cli, &printer);
    assert!(result.is_err());
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_edit/no_config.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "no_config");
}
