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
//!   - `module_delete/purge.json` — --purge removes target files.
//!   - `module_delete/lockfile.json` — lockfile entry is cleaned on delete.
//!   - `module_edit/valid.txt` — `/bin/true` editor + valid manifest →
//!     success Doc.
//!   - `module_edit/not_found.txt` — error-path Doc when module absent.
//!   - `module_update/remove_existing.json` — removing existing items decrements changes.
//!   - `module_update/remove_nonexistent.json` — removing absent items emits warn, changes==0.
//!   - `module_update/add_duplicate.json` — adding already-present pkg emits info, changes==0.

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::module;
use cfgd_core::config::{ModuleLockEntry, ModuleLockfile};
use cfgd_core::output::{Printer, PromptAnswer};
#[cfg(unix)]
use cfgd_core::test_helpers::EditorGuard;
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;
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
    cfgd_core::normalize_for_snapshot(raw, &[(config_dir, "<CONFIG_DIR>")])
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
    let err = module::cmd_module_create(&cli, &printer, &args)
        .expect_err("duplicate module must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_create/already_exists.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "already_exists");
    assert_eq!(meta.name, "dup-mod");
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
    let err = module::cmd_module_update_local(&cli, &printer, &args)
        .expect_err("missing module must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_update/not_found.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    assert_eq!(meta.name, "ghost");
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

    module::cmd_module_delete(&cli, &printer, "del-mod", true, false, false).unwrap();
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

    module::cmd_module_delete(&cli, &printer, "cancel-mod", false, false, false).unwrap();
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

    let err = module::cmd_module_delete(&cli, &printer, "ghost", true, false, false)
        .expect_err("missing module must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_delete/not_found.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
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

    let err = module::cmd_module_edit(&cli, &printer, "ghost")
        .expect_err("missing module must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped = normalize(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_edit/not_found.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
}

// ---------------------------------------------------------------------------
// TARGET A — cmd_module_delete: --purge path
// ---------------------------------------------------------------------------

/// `--purge` path: a module whose `files:` entry points at a real file inside the
/// test HOME.  After `delete(yes=true, purge=true)` the target file must be gone
/// and the JSON payload must carry `filesProcessed >= 1` and `purge: true`.
#[test]
#[serial]
fn module_delete_purge_removes_target_file() {
    let home = tempfile::tempdir().unwrap();
    let _home_guard = cfgd_core::with_test_home_guard(home.path());

    let (config_dir, state_dir) = module_test_config_setup();

    // Create the target file inside the test HOME so expand_tilde resolves there.
    let target_path = home.path().join("dotfiles").join("purge-target.cfg");
    std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    std::fs::write(&target_path, "contents").unwrap();
    assert!(
        target_path.exists(),
        "precondition: target must exist before delete"
    );

    // Build module YAML whose target uses the absolute path (no tilde needed —
    // expand_tilde is a no-op on already-absolute paths).
    let target_str = target_path.display().to_string();
    let module_yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: purge-mod\nspec:\n  files:\n    - source: files/purge-target.cfg\n      target: {}\n",
        target_str
    );
    write_module(config_dir.path(), "purge-mod", &module_yaml);

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_delete(&cli, &printer, "purge-mod", true, true, false)
        .expect("purge delete must succeed");
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("Purged"),
        "human output must mention 'Purged'; got:\n{human}"
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "purge-mod");
    assert_eq!(json["purge"], true);
    assert_eq!(json["cancelled"], false);
    assert!(
        json["filesProcessed"].as_u64().unwrap_or(0) >= 1,
        "filesProcessed must be >= 1; got {:#?}",
        json["filesProcessed"]
    );

    assert!(
        !target_path.exists(),
        "purge must delete the target file at {:?}",
        target_path
    );
}

// ---------------------------------------------------------------------------
// TARGET A — cmd_module_delete: lockfile cleanup
// ---------------------------------------------------------------------------

/// Seed `modules.lock` with an entry for the module under test, delete it,
/// then assert the payload carries `removedFromLockfile: true` and the human
/// output contains the "Removed '…' from modules.lock" line.
#[test]
fn module_delete_cleans_lockfile_entry() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(
        config_dir.path(),
        "lock-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: lock-mod\nspec: {}\n",
    );

    // Seed a lockfile entry for the module.
    let lockfile = ModuleLockfile {
        modules: vec![ModuleLockEntry {
            name: "lock-mod".to_string(),
            url: "https://example.com/repo.git".to_string(),
            pinned_ref: "v9.9.0".to_string(),
            commit: "abc123".to_string(),
            integrity: "sha256:deadbeef".to_string(),
            subdir: None,
        }],
    };
    cfgd_core::modules::save_lockfile(config_dir.path(), &lockfile)
        .expect("seeding lockfile must succeed");

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    module::cmd_module_delete(&cli, &printer, "lock-mod", true, false, false)
        .expect("delete with lockfile entry must succeed");
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("Removed 'lock-mod' from modules.lock"),
        "human output must contain lockfile removal message; got:\n{human}"
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "lock-mod");
    assert_eq!(json["removedFromLockfile"], true);
    assert_eq!(json["cancelled"], false);

    // Confirm the lockfile entry is actually gone from disk.
    let reloaded = cfgd_core::modules::load_lockfile(config_dir.path())
        .expect("lockfile must be loadable after delete");
    assert!(
        reloaded.modules.iter().all(|e| e.name != "lock-mod"),
        "lockfile must not contain 'lock-mod' after delete"
    );
}

// ---------------------------------------------------------------------------
// TARGET B — cmd_module_update_local: remove existing items
// ---------------------------------------------------------------------------

/// A module pre-populated with one of every removable item type. Removing
/// each via a leading `-` must emit `Removed …` lines and increment changes
/// by the number of distinct items removed.
#[test]
fn module_update_remove_existing_items() {
    let (config_dir, state_dir) = module_test_config_setup();

    // Pre-populate the module with one dep, one package, one env var,
    // one alias, and one post-apply script.
    let module_yaml = concat!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: rm-mod\nspec:\n",
        "  depends:\n    - base-dep\n",
        "  packages:\n    - name: ripgrep\n",
        "  env:\n    - name: MY_VAR\n      value: hello\n",
        "  aliases:\n    - name: rg\n      command: ripgrep\n",
        "  scripts:\n    postApply:\n      - run: echo done\n",
    );
    write_module(config_dir.path(), "rm-mod", module_yaml);

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleUpdateArgs {
        name: "rm-mod".to_string(),
        depends: vec!["-base-dep".to_string()],
        packages: vec!["-ripgrep".to_string()],
        env: vec!["-MY_VAR".to_string()],
        aliases: vec!["-rg".to_string()],
        post_apply: vec!["-echo done".to_string()],
        files: vec![],
        private: false,
        description: None,
        sets: vec![],
    };
    module::cmd_module_update_local(&cli, &printer, &args)
        .expect("removing existing items must succeed");
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("Removed dependency: base-dep"),
        "must confirm dep removal; got:\n{human}"
    );
    assert!(
        human.contains("Removed package: ripgrep"),
        "must confirm pkg removal; got:\n{human}"
    );
    assert!(
        human.contains("Removed env: MY_VAR"),
        "must confirm env removal; got:\n{human}"
    );
    assert!(
        human.contains("Removed alias: rg"),
        "must confirm alias removal; got:\n{human}"
    );
    assert!(
        human.contains("Removed post-apply script: echo done"),
        "must confirm script removal; got:\n{human}"
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "rm-mod");
    assert_eq!(
        json["changes"].as_u64().unwrap_or(0),
        5,
        "exactly 5 changes expected (dep + pkg + env + alias + script)"
    );
}

// ---------------------------------------------------------------------------
// TARGET B — cmd_module_update_local: remove nonexistent items emits warnings
// ---------------------------------------------------------------------------

/// Removing dep/pkg/env/alias/script items that do not exist in the module
/// emits Role::Warn "not found" lines (not hard errors) and leaves
/// `changes == 0`. The module here carries no `scripts` block at all, so the
/// script-removal warning also exercises the no-scripts-block path (which must
/// warn like every other remove, not silently no-op).
#[test]
fn module_update_remove_nonexistent_items_warns() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(
        config_dir.path(),
        "warn-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: warn-mod\nspec: {}\n",
    );

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleUpdateArgs {
        name: "warn-mod".to_string(),
        depends: vec!["-ghost-dep".to_string()],
        packages: vec!["-ghostpkg".to_string()],
        env: vec!["-GHOST_ENV".to_string()],
        aliases: vec!["-ghost-alias".to_string()],
        post_apply: vec!["-ghost-script".to_string()],
        files: vec![],
        private: false,
        description: None,
        sets: vec![],
    };
    module::cmd_module_update_local(&cli, &printer, &args)
        .expect("removing absent items must not error");
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("Dependency 'ghost-dep' not found"),
        "must warn about missing dep; got:\n{human}"
    );
    assert!(
        human.contains("Package 'ghostpkg' not found in module"),
        "must warn about missing pkg; got:\n{human}"
    );
    assert!(
        human.contains("Env var 'GHOST_ENV' not found"),
        "must warn about missing env var; got:\n{human}"
    );
    assert!(
        human.contains("Alias 'ghost-alias' not found"),
        "must warn about missing alias; got:\n{human}"
    );
    assert!(
        human.contains("Script 'ghost-script' not found"),
        "must warn about missing script even with no scripts block; got:\n{human}"
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(
        json["changes"].as_u64().unwrap_or(99),
        0,
        "no changes must be recorded when all removes target absent items"
    );
}

// ---------------------------------------------------------------------------
// TARGET B — cmd_module_update_local: add already-present package emits info
// ---------------------------------------------------------------------------

/// Adding a package that is already in the module must emit an info line
/// ("already in module") and leave `changes == 0`.
#[test]
fn module_update_add_duplicate_package_is_no_op() {
    let (config_dir, state_dir) = module_test_config_setup();
    write_module(
        config_dir.path(),
        "dup-pkg-mod",
        concat!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dup-pkg-mod\nspec:\n",
            "  packages:\n    - name: ripgrep\n",
        ),
    );

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let args = cfgd::cli::ModuleUpdateArgs {
        name: "dup-pkg-mod".to_string(),
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
    module::cmd_module_update_local(&cli, &printer, &args).expect("duplicate add must not error");
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("Package 'ripgrep' already in module"),
        "must emit already-in-module info; got:\n{human}"
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(
        json["changes"].as_u64().unwrap_or(99),
        0,
        "duplicate add must not increment changes"
    );
}
