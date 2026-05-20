//! Snapshot tests for `cfgd generate`.
//!
//! Coverage strategy:
//!   The full AI-driven generation surface (`cmd_generate` non-scan_only
//!   branch) drives a multi-turn Anthropic Messages API conversation,
//!   dispatches tool calls into the system scanner, and writes generated
//!   profile/module YAML to the config directory. The mocked-API tests
//!   for that surface live in `crates/cfgd/src/cli/generate/tests.rs::cmd_generate_mockito`
//!   and exercise text-only, multi-turn-with-tool-use, present_yaml
//!   auto-accept, consent-decline, and missing-API-key branches —
//!   intractable to snapshot here since the conversation transcript itself
//!   gets included in the buffered Doc and Anthropic chooses non-deterministic
//!   IDs and timing.
//!
//!   What we DO snapshot is the deterministic `--scan-only` branch which
//!   walks the home directory looking for dotfiles + shell config and
//!   emits a buffered Doc with stable key names (`toolsScanned`,
//!   `settingsCaptured`, `dotfileEntries`, …). The home dir is a tempdir
//!   so the snapshot is reproducible.

mod common;

use std::path::Path;

use cfgd::cli::generate::{self, GenerateArgs};
use cfgd_core::output::{Doc, OutputFormat, Printer, Role};

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

fn cli_for(config_dir: &Path) -> cfgd::cli::Cli {
    cfgd::cli::Cli {
        config: config_dir.join("cfgd.yaml"),
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: true,
        output: cfgd::cli::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        jsonpath: None,
        state_dir: None,
        command: Some(cfgd::cli::Command::Status {
            module: None,
            exit_code: false,
        }),
    }
}

fn scan_only_args(home: &Path) -> GenerateArgs {
    GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: true,
        scan_only: true,
        shell: Some("zsh".into()),
        home: Some(home.to_string_lossy().to_string()),
    }
}

#[test]
fn generate_scan_only_empty_home_human() {
    let home = tempfile::tempdir().unwrap();
    let cli = cli_for(home.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    generate::cmd_generate(&cli, &v2_printer, &scan_only_args(home.path())).unwrap();
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "generate/scan_only.txt",
        &stripped,
    );
}

#[test]
fn generate_scan_only_json_shape() {
    let home = tempfile::tempdir().unwrap();
    // Plant one dotfile to populate `dotfileEntries`.
    std::fs::write(home.path().join(".vimrc"), "set number\n").unwrap();

    let cli = cli_for(home.path());
    let (v2_printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

    generate::cmd_generate(&cli, &v2_printer, &scan_only_args(home.path())).unwrap();
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["target"], "scan_only");
    assert_eq!(json["shell"], "zsh");
    assert!(json["dotfileEntries"].as_u64().is_some());
    assert!(json["settingsCaptured"].as_u64().is_some());
    assert!(json["toolsScanned"].is_array());
}

/// Bridge invariant: streaming per-turn status lines drop, then a buffered
/// summary Doc emits — combined human surface contains exactly one blank
/// line at the transition. Production `cmd_generate`'s non-scan_only branch
/// drives a multi-turn Anthropic conversation whose transcript content and
/// timing are non-deterministic; per the F3 README bridge-synthetic
/// exception we hand-roll the minimal streaming-then-buffered shape here.
/// The streaming-side content is deterministic and may diverge from any
/// specific real invocation; what's locked is the §17.2 invariant.
#[test]
fn generate_bridge_one_blank_line() {
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.heading("Generate");
    v2_printer.status_simple(Role::Info, "Scanning system for installed tools");
    v2_printer.emit(
        Doc::new()
            .status(Role::Ok, "Generation complete")
            .with_data(serde_json::json!({
                "target": "profile",
                "outputPath": "profiles/dev.yaml",
                "scanned": 12,
                "modulesGenerated": 3,
            })),
    );
    drop(v2_printer);

    let captured = strip_ansi(&cap.human());
    assert!(
        captured.contains("\n\n"),
        "bridge missing blank line:\n{captured}"
    );
    assert!(
        !captured.contains("\n\n\n"),
        "bridge has duplicate blank line:\n{captured}"
    );
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "generate/bridge.txt", &captured);
}
