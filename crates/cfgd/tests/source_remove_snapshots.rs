//! Snapshot tests for `cfgd source remove`.
//!
//! Cases:
//!   - `source_remove/happy.{txt,json}` — `--remove-all` branch removes the
//!     source without prompting and emits the success Doc.
//!   - `source_remove/keep_all.txt` — `--keep-all` branch transfers managed
//!     resources to local management.
//!   - `source_remove/cancelled.txt` — interactive Cancel option: the source
//!     manages at least one resource so `cmd_source_remove` opens the
//!     three-option prompt; the `Cancel` answer aborts removal without
//!     mutating config or state.
//!   - `source_remove/not_found.txt` — error-path Doc for missing source.
//!   - `source_remove/conflicting_flags.txt` — error-path Doc when both
//!     `--keep-all` and `--remove-all` are passed.
//!
//! Goldens live under `tests/output_snapshots/source_remove/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_remove_snapshots

mod common;

use std::path::Path;

use cfgd::cli::source::cmd_source_remove;
use cfgd_core::output::{Printer, PromptAnswer};
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;

use common::{cli_for, source_test_config_with_source_setup};

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
fn source_remove_happy_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_remove(&cli, &printer, "team-config", false, true).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_remove/happy.txt",
        &stripped,
    );
}

#[test]
fn source_remove_happy_json() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_remove(&cli, &printer, "team-config", false, true).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "team-config");
    assert_eq!(json["cancelled"], false);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_remove/happy.json");
}

#[test]
fn source_remove_keep_all_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_remove(&cli, &printer, "team-config", true, false).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_remove/keep_all.txt",
        &stripped,
    );
}

#[test]
fn source_remove_cancelled_human() {
    // Seed the source AND a managed resource owned by it so the
    // three-option prompt opens. PromptAnswer::Select("Cancel...") routes
    // to the Cancel branch; the command emits "Cancelled — source not
    // removed" and returns Ok(()) without mutating config or state.
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let store = cfgd_core::state::StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();
    store
        .upsert_managed_resource("package", "brew/curl", "team-config", Some("hash1"), None)
        .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Select(
        "Cancel (abort remove)".into(),
    )]);

    cmd_source_remove(&cli, &printer, "team-config", false, false).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_remove/cancelled.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "team-config");
    assert_eq!(json["cancelled"], true);

    // Cancel must NOT mutate config (source still subscribed).
    let cfg = cfgd_core::config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert!(cfg.spec.sources.iter().any(|s| s.name == "team-config"));
}

#[test]
fn source_remove_not_found_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = cmd_source_remove(&cli, &printer, "missing", false, true);
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_remove/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["name"], "missing");
}

#[test]
fn source_remove_conflicting_flags_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = cmd_source_remove(&cli, &printer, "team-config", true, true);
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_remove/conflicting_flags.txt",
        &stripped,
    );
}
