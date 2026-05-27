//! Snapshot tests for `cfgd module create/update/edit/delete`.
//!
//! Cases:
//!   - `module_create/happy.{txt,json}` — non-interactive create with packages
//!     emits the summary section + post-create hints.
//!   - `module_create/already_exists.txt` — error-path Doc.
//!   - `module_update/happy.{txt,json}` — local update adds a package.
//!   - `module_update/no_changes.txt` — empty args reports "No changes specified".
//!   - `module_update/not_found.txt` — error-path Doc.
//!   - `module_delete/happy.{txt,json}` — delete with --yes succeeds.
//!   - `module_delete/cancelled.txt` — prompt-Confirm(false) path.
//!   - `module_delete/not_found.txt` — error-path Doc.
//!   - `module_edit/valid.txt` — `/bin/true` editor + valid manifest →
//!     success Doc.
//!   - `module_edit/not_found.txt` — error-path Doc when module absent.

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output::{Printer, PromptAnswer};
#[cfg(unix)]
use cfgd_core::test_helpers::EditorGuard;
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;
#[cfg(unix)]
use serial_test::serial;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

const VALID_MODULE: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: edit-mod\n  description: edit fixture\nspec: {}\n";

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

fn module_test_config_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
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
    (config_dir, state_dir)
}

fn write_module(config_dir: &Path, name: &str, yaml: &str) {
    let mod_dir = config_dir.join("modules").join(name);
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(mod_dir.join("module.yaml"), yaml).unwrap();
}

fn cli_for(config_dir: &Path, state_dir: &Path) -> cfgd::cli::Cli {
    common::cli_for(config_dir, state_dir)
}

fn normalize(raw: &str, config_dir: &Path) -> String {
    raw.replace(&config_dir.display().to_string(), "<CONFIG_DIR>")
        .replace('\\', "/")
}

#[test]
fn module_create_happy_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleCreateArgs {
        name: "happy-mod".to_string(),
        description: Some("happy-fixture".to_string()),
        depends: vec![],
        packages: vec!["curl".to_string()],
        files: vec![],
        env: vec![],
        aliases: vec![],
        private: false,
        post_apply: vec![],
        sets: vec![],
        apply: false,
        yes: true,
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_create/happy.txt",
        &stripped,
    );
}

#[test]
fn module_create_happy_json() {
    let (config_dir, state_dir) = module_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleCreateArgs {
        name: "json-mod".to_string(),
        description: Some("json-fixture".to_string()),
        depends: vec![],
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        private: false,
        post_apply: vec![],
        sets: vec![],
        apply: false,
        yes: true,
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "json-mod");
    assert_eq!(json["applied"], false);
}

#[test]
fn module_create_already_exists_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(config_dir.path(), "dup-mod", VALID_MODULE);
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleCreateArgs {
        name: "dup-mod".to_string(),
        description: None,
        depends: vec![],
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        private: false,
        post_apply: vec![],
        sets: vec![],
        apply: false,
        yes: true,
    };
    let result = module::cmd_module_create(&cli, &printer, &args);
    assert!(result.is_err());
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_create/already_exists.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "already_exists");
    assert_eq!(json["name"], "dup-mod");
}

#[test]
fn module_update_happy_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(
        config_dir.path(),
        "upd-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: upd-mod\nspec: {}\n",
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleUpdateArgs {
        name: "upd-mod".to_string(),
        packages: vec!["ripgrep".to_string()],
        files: vec![],
        env: vec![],
        aliases: vec![],
        depends: vec![],
        post_apply: vec![],
        private: false,
        description: None,
        sets: vec![],
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_update/happy.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "upd-mod");
    assert_eq!(json["changes"], 1);
}

#[test]
fn module_update_no_changes_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(
        config_dir.path(),
        "noop-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: noop-mod\nspec: {}\n",
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleUpdateArgs {
        name: "noop-mod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        depends: vec![],
        post_apply: vec![],
        private: false,
        description: None,
        sets: vec![],
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_update/no_changes.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["changes"], 0);
}

#[test]
fn module_update_not_found_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleUpdateArgs {
        name: "ghost".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        depends: vec![],
        post_apply: vec![],
        private: false,
        description: None,
        sets: vec![],
    };
    let result = module::cmd_module_update_local(&cli, &printer, &args);
    assert!(result.is_err());
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_update/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["name"], "ghost");
}

#[test]
fn module_delete_happy_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(
        config_dir.path(),
        "del-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: del-mod\nspec: {}\n",
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_delete(&cli, &printer, "del-mod", true, false).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_delete/happy.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "del-mod");
    assert_eq!(json["cancelled"], false);
}

#[test]
fn module_delete_cancelled_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(
        config_dir.path(),
        "cancel-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: cancel-mod\nspec: {}\n",
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);

    module::cmd_module_delete(&cli, &printer, "cancel-mod", false, false).unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_delete/cancelled.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["cancelled"], true);
}

#[test]
fn module_delete_not_found_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = module::cmd_module_delete(&cli, &printer, "ghost", true, false);
    assert!(result.is_err());
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_delete/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
}

#[cfg(unix)]
#[test]
#[serial]
fn module_edit_valid_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(config_dir.path(), "edit-mod", VALID_MODULE);
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let _editor = EditorGuard::set("/usr/bin/true");
    module::cmd_module_edit(&cli, &printer, "edit-mod").unwrap();
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "module_edit/valid.txt", &stripped);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["valid"], true);
    assert_eq!(json["name"], "edit-mod");
}

#[test]
fn module_edit_not_found_human() {
    let (config_dir, state_dir) = module_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = module::cmd_module_edit(&cli, &printer, "ghost");
    assert!(result.is_err());
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_edit/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
}
