//! Snapshot tests for `cfgd module export`.
//!
//! Cases:
//!   - `module_export/happy.{txt,json}` — exports a module to DevContainer
//!     Feature format and emits the buffered Doc with the resulting paths.
//!   - `module_export/not_found.txt` — error-path Doc for missing module.

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::module;
use cfgd_core::output::Printer;
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;

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

fn normalize(raw: &str, config_dir: &Path, output_dir: &Path) -> String {
    cfgd_core::normalize_for_snapshot(
        raw,
        &[(config_dir, "<CONFIG_DIR>"), (output_dir, "<OUTPUT_DIR>")],
    )
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

    let err = module::cmd_module_export(
        &cli,
        &printer,
        "ghost",
        &cfgd::cli::ExportFormat::Devcontainer,
        Some(output_dir.path().to_str().unwrap()),
    )
    .expect_err("missing module must return Err");
    render_cli_error(&printer, &err);
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

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    assert_eq!(meta.name, "ghost");
}
