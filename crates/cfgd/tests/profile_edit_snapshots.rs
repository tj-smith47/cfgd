//! Snapshot tests for `cfgd profile edit`.
//!
//! Cases:
//!   - `profile_edit/valid.{txt,json}` — happy path: real
//!     `cmd_profile_edit` against a tempdir, `$EDITOR=/bin/true` so the
//!     editor is a no-op, the seeded YAML is valid, and the final Doc emits
//!     `Role::Ok "Profile 'X' is valid"` with `{valid: true, errors: []}`.
//!   - `profile_edit/validation_error_decline.txt` — seeded YAML is invalid,
//!     queued `Confirm(false)` declines the re-edit prompt, final Doc emits
//!     `Role::Warn "Saved with validation errors"`.
//!   - `profile_edit/validation_error_accept_retry.txt` — first edit leaves
//!     invalid YAML, queued `Confirm(true)` accepts the retry, the second
//!     `$EDITOR` invocation re-writes valid YAML via a one-shot wrapper
//!     script, and the final Doc emits the success status.
//!
//! Goldens live under `tests/output_snapshots/profile_edit/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_edit_snapshots

#![cfg(unix)]

mod common;

use std::path::Path;

use cfgd::cli::profile::cmd_profile_edit;
use cfgd_core::output::{Printer, PromptAnswer};
use cfgd_core::test_helpers::EnvVarGuard;
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;
use serial_test::serial;

use common::{cli_for, normalize_profile_paths, profile_test_config_setup};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

const VALID_PROFILE_YAML: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n";

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
#[serial]
fn profile_edit_valid_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    // Overwrite default.yaml with a known-valid minimal Profile so the
    // validate loop's Ok arm fires immediately.
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        VALID_PROFILE_YAML,
    )
    .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let _editor = EnvVarGuard::set("EDITOR", "/usr/bin/true");

    cmd_profile_edit(&cli, &printer, "default").unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_edit/valid.txt",
        &stripped,
    );
}

#[test]
#[serial]
fn profile_edit_valid_json() {
    let (config_dir, state_dir) = profile_test_config_setup();
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        VALID_PROFILE_YAML,
    )
    .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let _editor = EnvVarGuard::set("EDITOR", "/usr/bin/true");

    cmd_profile_edit(&cli, &printer, "default").unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "default");
    assert_eq!(json["valid"], true);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_edit/valid.json");
}

#[test]
#[serial]
fn profile_edit_validation_error_decline_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        "this is not a valid Profile document",
    )
    .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);
    let _editor = EnvVarGuard::set("EDITOR", "/usr/bin/true");

    cmd_profile_edit(&cli, &printer, "default").expect("edit must Ok even on Save-with-errors");
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_edit/validation_error_decline.txt",
        &stripped,
    );
}

#[test]
#[serial]
fn profile_edit_validation_error_accept_retry_human() {
    // Seed invalid YAML at default.yaml. The first `$EDITOR` invocation is
    // `/bin/true` (no-op, leaves invalid content). The validate loop's Err
    // arm fires, queued Confirm(true) accepts the retry, and the second
    // `$EDITOR` is a wrapper script that rewrites the file with valid YAML
    // — so the second validate pass succeeds and the success Doc is emitted.
    let (config_dir, state_dir) = profile_test_config_setup();
    let profile_path = config_dir.path().join("profiles").join("default.yaml");
    std::fs::write(&profile_path, "not valid").unwrap();

    // Build a per-call EDITOR script: first call leaves the file as-is,
    // second call writes VALID_PROFILE_YAML over it. State is tracked via
    // a counter file inside the tempdir.
    let counter_path = config_dir.path().join("editor_calls.txt");
    let script_path = config_dir.path().join("fake_editor.sh");
    let script_body = format!(
        "#!/bin/sh\nN=$(cat {counter} 2>/dev/null || echo 0)\nN=$((N + 1))\necho $N > {counter}\nif [ $N -ge 2 ]; then\n  cat > $1 <<'EOF'\n{valid}EOF\nfi\n",
        counter = counter_path.display(),
        valid = VALID_PROFILE_YAML,
    );
    std::fs::write(&script_path, &script_body).unwrap();
    let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    let _editor = EnvVarGuard::set("EDITOR", script_path.to_str().unwrap());

    cmd_profile_edit(&cli, &printer, "default")
        .expect("edit must Ok on the second-pass valid YAML");
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_edit/validation_error_accept_retry.txt",
        &stripped,
    );
}
