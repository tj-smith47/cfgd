//! Snapshot tests for `cfgd source add`.
//!
//! Cases:
//!   - `source_add/happy.{txt,json}` — real `cmd_source_add` against a bare
//!     local source repo (file:// URL gated by `CFGD_ALLOW_LOCAL_SOURCES=1`).
//!   - `source_add/already_exists.txt` — error-path Doc when the source name
//!     is already subscribed in `cfgd.yaml`.
//!   - `source_add/clone_failure.txt` — error-path Doc when the upstream URL
//!     points to a non-existent local bare repo; `SourceManager::load_source`
//!     fails and `cmd_source_add` bails after emitting the load-failure Doc.
//!   - `source_add/bridge.txt` — streaming-to-buffered bridge assertion
//!     (single blank line between the streaming clone surface and the buffered
//!     "Subscribed" Doc).
//!
//! Goldens live under `tests/output_snapshots/source_add/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_add_snapshots

mod common;

use std::path::Path;

use cfgd::cli::source::cmd_source_add;
use cfgd_core::output::Printer;
use serial_test::serial;

use common::{
    cli_for, make_bare_source_repo, source_add_args, source_test_config_setup,
    source_test_config_with_source_setup,
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
    strip_spinner_duration(out)
}

use cfgd_core::output::test_capture::strip_spinner_duration;

#[test]
#[serial]
fn source_add_happy_human() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), "team-config", None);
    let url = format!("file://{}", bare.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("team-config".into());

    cmd_source_add(&cli, &printer, &args).unwrap();
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bare,
        bare_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "source_add/happy.txt", &stripped);
}

#[test]
#[serial]
fn source_add_happy_json() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), "team-config", None);
    let url = format!("file://{}", bare.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("team-config".into());

    cmd_source_add(&cli, &printer, &args).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "team-config");
    assert!(json["commit"].is_string());
}

#[test]
#[serial]
fn source_add_already_exists_human() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = source_add_args("https://github.com/team/config");
    args.name = Some("team-config".into());

    let result = cmd_source_add(&cli, &printer, &args);
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_add/already_exists.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "already_exists");
    assert_eq!(json["name"], "team-config");
}

#[test]
#[serial]
fn source_add_clone_failure_human() {
    // Point cmd_source_add at a non-existent bare repo. SourceManager::load_source
    // fails (git can't open the URL), and cmd_source_add bails after emitting
    // the `load_failed` Doc.
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bogus_root = tempfile::tempdir().unwrap();
    let bogus = bogus_root.path().join("nonexistent-bare.git");
    let url = format!("file://{}", bogus.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("doomed-src".into());

    let result = cmd_source_add(&cli, &printer, &args);
    assert!(result.is_err(), "cmd_source_add must fail on bogus URL");
    drop(printer);

    let stripped = normalize_paths(
        &strip_ansi(&cap.human()),
        &bogus,
        bogus_root.path(),
        config_dir.path(),
        state_dir.path(),
    );
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_add/clone_failure.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "load_failed");
    assert_eq!(json["name"], "doomed-src");
}

#[test]
#[serial]
fn source_add_bridge_one_blank_line() {
    // Bridge invariant: the streaming clone surface (spinners emitted
    // by `SourceManager::load_source`) → buffered "Subscribed" Doc transition
    // has exactly one blank line. Hand-rolled because the Printer captures all
    // human-surface output via the test capture; we only assert on that
    // captured surface here.
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare = make_bare_source_repo(bare_root.path(), "bridge-src", None);
    let url = format!("file://{}", bare.display());

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = source_add_args(url);
    args.name = Some("bridge-src".into());

    cmd_source_add(&cli, &printer, &args).unwrap();
    drop(printer);

    let combined = cap.human();
    // The capture always carries the heading + manifest sections + final
    // Doc — single-blank-line rule applies between the streamed manifest
    // section and the final Subscribed status.
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
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "source_add/bridge.txt", &stripped);
}
