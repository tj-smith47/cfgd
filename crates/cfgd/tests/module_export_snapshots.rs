//! Snapshot tests for `cfgd module export`.
//!
//! Cases:
//!   - `module_export/happy.{txt,json}` — exports a module to DevContainer
//!     Feature format and emits the buffered Doc with the resulting paths.
//!   - `module_export/not_found.txt` — error-path Doc for missing module.

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output::Printer;

use common::cli_for;

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

fn normalize(raw: &str, config_dir: &Path, output_dir: &Path) -> String {
    raw.replace(&config_dir.display().to_string(), "<CONFIG_DIR>")
        .replace(&output_dir.display().to_string(), "<OUTPUT_DIR>")
        .replace('\\', "/")
}

fn module_export_setup() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.path().join("profiles/default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let mod_dir = config_dir.path().join("modules").join("export-mod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: export-mod\n  description: exportable\nspec:\n  packages:\n    - name: curl\n",
    )
    .unwrap();

    (config_dir, state_dir, output_dir)
}

#[test]
fn module_export_happy_human() {
    let (config_dir, state_dir, output_dir) = module_export_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_export(
        &cli,
        &printer,
        "export-mod",
        &cfgd::cli::ExportFormat::Devcontainer,
        Some(output_dir.path().to_str().unwrap()),
    )
    .unwrap();
    drop(printer);

    let stripped = normalize(
        &strip_ansi(&cap.human()),
        config_dir.path(),
        output_dir.path(),
    );
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_export/happy.txt",
        &stripped,
    );
}

#[test]
fn module_export_happy_json() {
    let (config_dir, state_dir, output_dir) = module_export_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_export(
        &cli,
        &printer,
        "export-mod",
        &cfgd::cli::ExportFormat::Devcontainer,
        Some(output_dir.path().to_str().unwrap()),
    )
    .unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "export-mod");
    assert_eq!(json["format"], "devcontainer");
    assert!(
        json["installScript"]
            .as_str()
            .unwrap()
            .ends_with("install.sh")
    );
}

#[test]
fn module_export_not_found_human() {
    let (config_dir, state_dir, output_dir) = module_export_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = module::cmd_module_export(
        &cli,
        &printer,
        "ghost",
        &cfgd::cli::ExportFormat::Devcontainer,
        Some(output_dir.path().to_str().unwrap()),
    );
    assert!(result.is_err());
    drop(printer);

    let stripped = normalize(
        &strip_ansi(&cap.human()),
        config_dir.path(),
        output_dir.path(),
    );
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_export/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["name"], "ghost");
}
