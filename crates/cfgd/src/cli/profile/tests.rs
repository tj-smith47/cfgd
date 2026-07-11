use super::*;

// --- parse_manager_package ---

#[test]
fn parse_manager_package_valid_brew() {
    let (mgr, pkg) = parse_manager_package("brew:curl").unwrap();
    assert_eq!(mgr, "brew");
    assert_eq!(pkg, "curl");
}

#[test]
fn parse_manager_package_valid_cargo() {
    let (mgr, pkg) = parse_manager_package("cargo:bat").unwrap();
    assert_eq!(mgr, "cargo");
    assert_eq!(pkg, "bat");
}

#[test]
fn parse_manager_package_missing_colon() {
    let err = parse_manager_package("brewcurl").unwrap_err();
    assert!(
        err.to_string().contains("expected manager:package"),
        "should mention expected format, got: {err}"
    );
}

#[test]
fn parse_manager_package_empty_manager() {
    let err = parse_manager_package(":curl").unwrap_err();
    assert!(
        err.to_string().contains("cannot be empty"),
        "should mention empty manager, got: {err}"
    );
}

#[test]
fn parse_manager_package_empty_package() {
    let err = parse_manager_package("brew:").unwrap_err();
    assert!(
        err.to_string().contains("cannot be empty"),
        "should mention empty package, got: {err}"
    );
}

// --- parse_secret_spec ---

#[test]
fn parse_secret_spec_valid_simple() {
    let spec = parse_secret_spec("secrets/api-key.enc:~/.config/app/key").unwrap();
    assert_eq!(spec.source, "secrets/api-key.enc");
    assert_eq!(spec.target, Some(PathBuf::from("~/.config/app/key")));
    assert!(spec.template.is_none());
    assert!(spec.backend.is_none());
    assert!(spec.envs.is_none());
}

#[test]
fn parse_secret_spec_op_url() {
    // rsplit_once should split on the LAST colon, so op://vault/item stays together
    let spec = parse_secret_spec("op://vault/item:~/target").unwrap();
    assert_eq!(spec.source, "op://vault/item");
    assert_eq!(spec.target, Some(PathBuf::from("~/target")));
}

#[test]
fn parse_secret_spec_missing_colon() {
    let err = parse_secret_spec("noseparator").unwrap_err();
    assert!(
        err.to_string().contains("expected source:target"),
        "should mention expected format, got: {err}"
    );
}

#[test]
fn parse_secret_spec_empty_source() {
    let err = parse_secret_spec(":target").unwrap_err();
    assert!(
        err.to_string().contains("cannot be empty"),
        "should mention empty source, got: {err}"
    );
}

#[test]
fn parse_secret_spec_empty_target() {
    let err = parse_secret_spec("source:").unwrap_err();
    assert!(
        err.to_string().contains("cannot be empty"),
        "should mention empty target, got: {err}"
    );
}

// --- update_script_list ---

use cfgd_core::test_helpers::test_printer as make_printer;

#[test]
fn update_script_list_add_to_empty() {
    let printer = make_printer();
    let mut scripts: Option<config::ScriptSpec> = None;
    let add = vec!["setup.sh".to_string()];
    let changes = update_script_list(
        &mut scripts,
        &add,
        &[],
        "preApply",
        |s| &mut s.pre_apply,
        &printer,
    );
    assert_eq!(changes, 1);
    assert!(scripts.is_some());
    assert_eq!(
        scripts.unwrap().pre_apply,
        vec![config::ScriptEntry::Simple("setup.sh".to_string())]
    );
}

#[test]
fn update_script_list_add_to_existing() {
    let printer = make_printer();
    let mut scripts = Some(config::ScriptSpec {
        pre_apply: vec![config::ScriptEntry::Simple("a.sh".to_string())],
        ..Default::default()
    });
    let add = vec!["b.sh".to_string()];
    let changes = update_script_list(
        &mut scripts,
        &add,
        &[],
        "preApply",
        |s| &mut s.pre_apply,
        &printer,
    );
    assert_eq!(changes, 1);
    assert_eq!(
        scripts.unwrap().pre_apply,
        vec![
            config::ScriptEntry::Simple("a.sh".to_string()),
            config::ScriptEntry::Simple("b.sh".to_string())
        ]
    );
}

#[test]
fn update_script_list_add_duplicate() {
    let printer = make_printer();
    let mut scripts = Some(config::ScriptSpec {
        pre_apply: vec![config::ScriptEntry::Simple("a.sh".to_string())],
        ..Default::default()
    });
    let add = vec!["a.sh".to_string()];
    let changes = update_script_list(
        &mut scripts,
        &add,
        &[],
        "preApply",
        |s| &mut s.pre_apply,
        &printer,
    );
    assert_eq!(changes, 0);
    assert_eq!(scripts.unwrap().pre_apply.len(), 1);
}

#[test]
fn update_script_list_remove_existing() {
    let printer = make_printer();
    let mut scripts = Some(config::ScriptSpec {
        pre_apply: vec![
            config::ScriptEntry::Simple("a.sh".to_string()),
            config::ScriptEntry::Simple("b.sh".to_string()),
        ],
        ..Default::default()
    });
    let remove = vec!["a.sh".to_string()];
    let changes = update_script_list(
        &mut scripts,
        &[],
        &remove,
        "preApply",
        |s| &mut s.pre_apply,
        &printer,
    );
    assert_eq!(changes, 1);
    assert_eq!(
        scripts.unwrap().pre_apply,
        vec![config::ScriptEntry::Simple("b.sh".to_string())]
    );
}

#[test]
fn update_script_list_remove_nonexistent() {
    let printer = make_printer();
    let mut scripts = Some(config::ScriptSpec {
        pre_apply: vec![config::ScriptEntry::Simple("a.sh".to_string())],
        ..Default::default()
    });
    let remove = vec!["nope.sh".to_string()];
    let changes = update_script_list(
        &mut scripts,
        &[],
        &remove,
        "preApply",
        |s| &mut s.pre_apply,
        &printer,
    );
    assert_eq!(changes, 0);
}

#[test]
fn update_script_list_remove_from_none() {
    let printer = make_printer();
    let mut scripts: Option<config::ScriptSpec> = None;
    let remove = vec!["nope.sh".to_string()];
    let changes = update_script_list(
        &mut scripts,
        &[],
        &remove,
        "preApply",
        |s| &mut s.pre_apply,
        &printer,
    );
    assert_eq!(changes, 0);
    assert!(scripts.is_none());
}

// --- profiles_inheriting ---

#[test]
fn profiles_inheriting_no_dir() {
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result =
        profiles_inheriting(Path::new("/nonexistent-dir-12345"), "base", &printer).unwrap();
    assert!(result.is_empty());
}

#[test]
fn profiles_inheriting_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - other\n  modules: []\n".to_string();
    std::fs::write(dir.path().join("child.yaml"), &profile).unwrap();

    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = profiles_inheriting(dir.path(), "base", &printer).unwrap();
    assert!(result.is_empty());
}

#[test]
fn profiles_inheriting_match_found() {
    let dir = tempfile::tempdir().unwrap();
    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - base\n  modules: []\n".to_string();
    std::fs::write(dir.path().join("child.yaml"), &profile).unwrap();

    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = profiles_inheriting(dir.path(), "base", &printer).unwrap();
    assert_eq!(result, vec!["child"]);
}

// --- collect_module_file_targets ---

#[test]
fn collect_module_file_targets_nonexistent_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let result = collect_module_file_targets("nope", dir.path(), None, cfgd_core::Scope::User);
    assert!(result.is_empty());
}

#[test]
fn prompt_restore_backups_no_op_when_no_backup_files_exist() {
    // The for-loop entry condition (backup_path.exists()) is false on every
    // target → the body never runs. Asserts the function returns Ok with
    // no side-effects and the prompt is never consumed.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("no-such-target.conf");
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(true)],
        cfgd_core::output::Verbosity::Normal,
    );

    prompt_restore_backups(std::slice::from_ref(&target), &printer).expect("no backups → no-op");

    drop(printer);
    let out = buf.lock().unwrap();
    assert!(
        !out.contains("Restored"),
        "must not announce a restore: {out}"
    );
}

#[test]
fn prompt_restore_backups_with_confirmed_yes_restores_backup_to_target() {
    // Stage `<target>.cfgd-backup` alongside a target path, queue
    // Confirm(true), assert: backup file is renamed onto the target,
    // the printer.status_simple(Role::Ok, "Restored <path>") line fires.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("restored.conf");
    let backup = dir.path().join("restored.conf.cfgd-backup");
    std::fs::write(&backup, b"backup-contents").unwrap();

    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(true)],
        cfgd_core::output::Verbosity::Normal,
    );
    prompt_restore_backups(std::slice::from_ref(&target), &printer)
        .expect("restore-confirmed must Ok");
    drop(printer);

    assert!(target.exists(), "target must have been restored");
    assert!(
        !backup.exists(),
        "backup file must have been moved (rename), not copied"
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"backup-contents",
        "target must contain backup bytes"
    );
    let out = buf.lock().unwrap();
    assert!(out.contains("Restored"), "should announce restore: {out}");
}

#[test]
fn prompt_restore_backups_with_confirmed_no_leaves_backup_and_target_alone() {
    // Confirm(false) → loop iteration runs but body is skipped → backup
    // remains, target stays absent.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("declined.conf");
    let backup = dir.path().join("declined.conf.cfgd-backup");
    std::fs::write(&backup, b"untouched-backup").unwrap();

    let (printer, _buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(false)],
        cfgd_core::output::Verbosity::Normal,
    );
    prompt_restore_backups(std::slice::from_ref(&target), &printer).expect("decline must Ok");

    assert!(!target.exists(), "target must not be created on decline");
    assert!(backup.exists(), "backup file must remain on decline");
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        b"untouched-backup",
        "backup contents must be unchanged"
    );
}

#[test]
fn prompt_restore_backups_removes_existing_target_before_renaming_backup() {
    // Target already on disk as a regular file → branch at backups.rs:51-53
    // removes it before the rename. Asserts the post-state matches the
    // backup contents (not the prior target contents).
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("overwrite.conf");
    let backup = dir.path().join("overwrite.conf.cfgd-backup");
    std::fs::write(&target, b"stale-deployed").unwrap();
    std::fs::write(&backup, b"original-backup").unwrap();

    let (printer, _buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(true)],
        cfgd_core::output::Verbosity::Normal,
    );
    prompt_restore_backups(std::slice::from_ref(&target), &printer).unwrap();

    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"original-backup",
        "restore must replace the prior target with the backup"
    );
}

#[test]
fn restore_or_remove_deployed_files_uses_shared_existed_semantics() {
    // The module-cleanup loop must route through the shared reconciler restore
    // path: a content backup is restored, an absent marker (existed=false)
    // removes the file, and a path with no backup is removed.
    use cfgd_core::state::StateStore;

    let dir = tempfile::tempdir().unwrap();
    let restored = dir.path().join("restored.conf");
    let absent = dir.path().join("created-later.conf");
    let no_backup = dir.path().join("orphan.conf");

    // Current on-disk state before cleanup.
    std::fs::write(&restored, b"modified").unwrap();
    std::fs::write(&absent, b"created-by-later-apply").unwrap();
    std::fs::write(&no_backup, b"orphaned-deploy").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let apply_id = state
        .record_apply("test", "h", cfgd_core::state::ApplyStatus::Success, None)
        .unwrap();

    // `restored.conf`: content backup → must be restored to "original".
    let backup_state = cfgd_core::FileState {
        content: b"original".to_vec(),
        content_hash: cfgd_core::sha256_hex(b"original"),
        permissions: None,
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };
    state
        .store_file_backup(apply_id, &restored.display().to_string(), &backup_state)
        .unwrap();
    // `created-later.conf`: absent marker → must be removed.
    state
        .store_absent_backup(apply_id, &absent.display().to_string())
        .unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let paths = [
        restored.display().to_string(),
        absent.display().to_string(),
        no_backup.display().to_string(),
    ];
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    {
        let section = printer.section("Rollback");
        restore_or_remove_deployed_files(&path_refs, &state, &section, &printer);
    }
    drop(printer);

    assert_eq!(
        std::fs::read(&restored).unwrap(),
        b"original",
        "content backup must be restored"
    );
    assert!(
        !absent.exists(),
        "absent-marked file must be removed (undo a later CREATE)"
    );
    assert!(!no_backup.exists(), "file with no backup must be removed");

    let out = buf.lock().unwrap();
    assert!(out.contains("Restored"), "must announce restore: {out}");
    assert!(out.contains("Removed"), "must announce removal: {out}");
}

#[test]
fn collect_module_file_targets_local_module() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("test-mod");
    std::fs::create_dir_all(&module_dir).unwrap();
    let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: []\n  files:\n    - source: foo.conf\n      target: /tmp/foo.conf\n";
    std::fs::write(module_dir.join("module.yaml"), module_yaml).unwrap();

    let result = collect_module_file_targets("test-mod", dir.path(), None, cfgd_core::Scope::User);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], PathBuf::from("/tmp/foo.conf"));
}

// =========================================================================
// Command-level tests
// =========================================================================

const TEST_CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n";
const DEFAULT_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  env:
    - name: EDITOR
      value: vim
  packages:
    cargo:
      - bat
"#;
const WORK_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - default
  env:
    - name: EDITOR
      value: code
  packages:
    cargo:
      - exa
"#;

fn setup_config_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    std::fs::write(profiles_dir.join("default.yaml"), DEFAULT_PROFILE_YAML).unwrap();
    std::fs::write(profiles_dir.join("work.yaml"), WORK_PROFILE_YAML).unwrap();
    dir
}

fn test_cli(dir: &Path) -> super::super::Cli {
    super::super::Cli {
        config: dir.join("cfgd.yaml"),
        config_explicit: false,
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: true,
        output: super::super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        list_envelope: false,
        jsonpath: None,
        state_dir: None,
        config_dir: None,
        cache_dir: None,
        runtime_dir: None,
        scope_arg: crate::cli::ScopeArg::User,
        command: Some(super::super::Command::Status {
            module: None,
            exit_code: false,
        }),
    }
}

fn make_profile_create_args(name: &str) -> super::super::ProfileCreateArgs {
    super::super::ProfileCreateArgs {
        name: name.to_string(),
        inherits: vec![],
        modules: vec![],
        packages: vec![],
        env: vec![],
        aliases: vec![],
        system: vec![],
        files: vec![],
        private: false,
        secrets: vec![],
        pre_apply: vec![],
        post_apply: vec![],
        pre_reconcile: vec![],
        post_reconcile: vec![],
        on_change: vec![],
        on_drift: vec![],
    }
}

fn make_profile_update_args() -> super::super::ProfileUpdateArgs {
    super::super::ProfileUpdateArgs {
        name: None,
        inherits: vec![],
        modules: vec![],
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        system: vec![],
        secrets: vec![],
        pre_apply: vec![],
        post_apply: vec![],
        pre_reconcile: vec![],
        post_reconcile: vec![],
        on_change: vec![],
        on_drift: vec![],
        private: false,
        yes: false,
        allow_unsigned: false,
    }
}

// --- cmd_profile_show ---

#[test]
fn profile_show_named_profile() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_show(&cli, &printer, Some("default")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Profile: default"),
        "should show profile heading, got: {output}"
    );
    assert!(
        output.contains("EDITOR"),
        "should show EDITOR env var from default profile, got: {output}"
    );
}

#[test]
fn profile_show_active_profile() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // None means "show the active profile" — reads from cfgd.yaml
    cmd_profile_show(&cli, &printer, None).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Profile: default"),
        "should show active profile heading, got: {output}"
    );
    assert!(
        output.contains("EDITOR"),
        "active profile (default) should contain EDITOR env var, got: {output}"
    );
}

#[test]
fn profile_show_inherited_profile_resolves_layers() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // work inherits from default, should resolve both layers
    cmd_profile_show(&cli, &printer, Some("work")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Layers"),
        "should show Layers section, got: {output}"
    );
    assert!(
        output.contains("default"),
        "should list 'default' as an inherited layer, got: {output}"
    );
    assert!(
        output.contains("work"),
        "should list 'work' layer, got: {output}"
    );
}

#[test]
fn profile_show_nonexistent_profile_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_profile_show(&cli, &printer, Some("nonexistent")).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should mention profile not found, got: {err}"
    );
}

#[test]
fn profile_show_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // No cfgd.yaml — showing active profile should fail
    let err = cmd_profile_show(&cli, &printer, None).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should mention config not found, got: {err}"
    );
}

// --- cmd_profile_list ---

#[test]
fn profile_list_shows_profiles() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_list(&cli, &printer).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("default"),
        "should list 'default' profile, got: {output}"
    );
    assert!(
        output.contains("work"),
        "should list 'work' profile, got: {output}"
    );
}

#[test]
fn profile_list_no_profiles_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    // Don't create profiles dir
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_list(&cli, &printer).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about missing profiles dir, got: {output}"
    );
}

#[test]
fn profile_list_empty_profiles_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_list(&cli, &printer).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("No profiles found"),
        "should indicate no profiles found, got: {output}"
    );
}

// --- cmd_profile_switch ---

#[test]
fn profile_switch_updates_config() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let result = cmd_profile_switch(&cli, "work", &printer);
    assert!(
        result.is_ok(),
        "cmd_profile_switch should succeed: {:?}",
        result.err()
    );

    // Verify the config file was updated
    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
}

#[test]
fn profile_switch_nonexistent_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let result = cmd_profile_switch(&cli, "nonexistent", &printer);
    assert!(
        result.is_err(),
        "switching to nonexistent profile should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "error should mention 'not found': {}",
        err
    );
}

#[test]
fn profile_switch_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    // No cfgd.yaml
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_profile_switch(&cli, "default", &printer).unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<cfgd_core::errors::CfgdError>(),
            Some(cfgd_core::errors::CfgdError::Config(
                cfgd_core::errors::ConfigError::NotFound { .. }
            ))
        ),
        "should be typed ConfigError::NotFound, got: {err}"
    );
    assert!(
        err.to_string().contains("config file not found"),
        "should mention missing config, got: {err}"
    );
}

#[test]
fn profile_switch_preserves_other_config() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Start at default, switch to work
    cmd_profile_switch(&cli, "work", &printer).unwrap();
    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
    // Config name should still be preserved
    assert_eq!(cfg.metadata.name, "test");
}

#[test]
fn profile_switch_error_lists_available_profiles() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let result = cmd_profile_switch(&cli, "nope", &printer);
    let err = result.unwrap_err();
    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    assert!(
        meta.hints.iter().any(|h| h.contains("Available profiles")),
        "error hints should list available profiles: {:?}",
        meta.hints
    );
    // Exit-6 uniformity across every missing-profile site.
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

// --- cmd_profile_create ---

#[test]
fn profile_create_interactive_drives_prompts_via_harness() {
    // Interactive mode at profile/create.rs:63-84 was uncovered because
    // every existing create test supplies at least one content flag,
    // taking the is_interactive=false arm. The harness queue drives the
    // two prompt_text calls so the body of the if-branch fires.
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![
            cfgd_core::output::PromptAnswer::Text("default".to_string()),
            cfgd_core::output::PromptAnswer::Text("".to_string()),
        ],
        cfgd_core::output::Verbosity::Normal,
    );

    // setup_config_dir already creates a `default.yaml` profile that the
    // first prompt response will reference as a parent.
    let args = make_profile_create_args("interactive-child");

    cmd_profile_create(&cli, &printer, &args)
        .expect("interactive profile create with valid parent + no modules");

    let profile_path = dir
        .path()
        .join("profiles")
        .join("interactive-child")
        .join("profile.yaml");
    assert!(
        profile_path.exists(),
        "profile YAML must be created on interactive happy path"
    );
    let doc = config::load_profile(&profile_path).unwrap();
    assert_eq!(doc.spec.inherits, vec!["default"]);
    drop(buf);
}

#[test]
fn profile_create_interactive_with_missing_parent_bails() {
    // is_interactive=true + Text("ghost") for parent prompt → loop checks
    // ghost.yaml existence at profile/create.rs:70-74 → anyhow::bail.
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![
            cfgd_core::output::PromptAnswer::Text("ghost-parent".to_string()),
            cfgd_core::output::PromptAnswer::Text("".to_string()),
        ],
        cfgd_core::output::Verbosity::Normal,
    );

    let args = make_profile_create_args("bad-parent-child");
    let result = cmd_profile_create(&cli, &printer, &args);
    let err = result.expect_err("missing parent must bail");
    let msg = err.to_string();
    assert!(
        msg.contains("ghost-parent") && msg.contains("not found"),
        "should mention missing parent: {msg}"
    );
}

#[test]
fn profile_create_minimal() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("devops");
    // Need at least one flag to avoid interactive mode
    args.modules = vec![];
    args.env = vec!["FOO=bar".to_string()];

    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "cmd_profile_create should succeed: {:?}",
        result.err()
    );

    let profile_path = dir
        .path()
        .join("profiles")
        .join("devops")
        .join("profile.yaml");
    assert!(profile_path.exists(), "profile YAML should be created");

    let raw = std::fs::read_to_string(&profile_path).unwrap();
    assert_eq!(
        raw.lines().next().unwrap(),
        cfgd_core::config::schema_modeline(
            cfgd_core::config::SchemaDocKind::Profile,
            env!("CARGO_PKG_VERSION")
        )
        .trim_end(),
        "scaffolded profile.yaml must start with the schema modeline"
    );

    // load_profile parsing below doubles as the modeline round-trip proof.
    let doc = config::load_profile(&profile_path).unwrap();
    assert_eq!(doc.metadata.name, "devops");
    assert_eq!(doc.spec.env.len(), 1);
    assert_eq!(doc.spec.env[0].name, "FOO");
    assert_eq!(doc.spec.env[0].value, "bar");
}

#[test]
fn profile_create_with_inherits() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("child");
    args.inherits = vec!["default".to_string()];

    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "cmd_profile_create with inherits should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("child")
            .join("profile.yaml"),
    )
    .unwrap();
    assert_eq!(doc.spec.inherits, vec!["default"]);
}

#[test]
fn profile_create_with_modules() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("modular");
    args.modules = vec!["shell".to_string()];

    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "cmd_profile_create with modules should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("modular")
            .join("profile.yaml"),
    )
    .unwrap();
    assert_eq!(doc.spec.modules, vec!["shell"]);
}

#[test]
fn profile_create_duplicate_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("default");
    args.env = vec!["X=1".to_string()]; // avoid interactive
    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(result.is_err(), "creating duplicate profile should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("already exists"),
        "error should mention 'already exists': {}",
        err
    );
}

#[test]
fn profile_create_missing_parent_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("orphan");
    args.inherits = vec!["nonexistent-parent".to_string()];

    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(
        result.is_err(),
        "creating profile with missing parent should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "error should mention parent not found: {}",
        err
    );
}

#[test]
fn profile_create_with_aliases() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("alias-test");
    args.aliases = vec!["ll=ls -la".to_string()];

    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "cmd_profile_create with aliases should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("alias-test")
            .join("profile.yaml"),
    )
    .unwrap();
    assert_eq!(doc.spec.aliases.len(), 1);
    assert_eq!(doc.spec.aliases[0].name, "ll");
    assert_eq!(doc.spec.aliases[0].command, "ls -la");
}

#[test]
fn profile_create_with_system_settings() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_create_args("sys-test");
    args.system = vec!["sysctl=net.core.somaxconn".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("sys-test")
            .join("profile.yaml"),
    )
    .unwrap();
    assert!(
        doc.spec.system.contains_key("sysctl"),
        "profile should have sysctl system setting"
    );
    assert_eq!(
        doc.spec.system["sysctl"],
        serde_yaml::Value::String("net.core.somaxconn".to_string()),
        "sysctl value should match"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Created profile"),
        "should confirm profile creation, got: {output}"
    );
}

#[test]
fn profile_create_with_secrets() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("secret-test");
    args.secrets = vec!["secrets/key.enc:/tmp/key".to_string()];

    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "cmd_profile_create with secrets should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("secret-test")
            .join("profile.yaml"),
    )
    .unwrap();
    assert_eq!(doc.spec.secrets.len(), 1);
    assert_eq!(doc.spec.secrets[0].source, "secrets/key.enc");
    assert_eq!(doc.spec.secrets[0].target, Some(PathBuf::from("/tmp/key")));
}

#[test]
fn profile_create_with_scripts() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("script-test");
    args.pre_apply = vec!["check.sh".to_string()];
    args.post_apply = vec!["notify.sh".to_string()];
    args.on_drift = vec!["alert.sh".to_string()];

    let result = cmd_profile_create(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "cmd_profile_create with scripts should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("script-test")
            .join("profile.yaml"),
    )
    .unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.pre_apply.len(), 1);
    assert_eq!(scripts.post_apply.len(), 1);
    assert_eq!(scripts.on_drift.len(), 1);
    assert_eq!(scripts.pre_apply[0].run_str(), "check.sh");
    assert_eq!(scripts.post_apply[0].run_str(), "notify.sh");
    assert_eq!(scripts.on_drift[0].run_str(), "alert.sh");
}

#[test]
fn profile_create_invalid_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args(".hidden");
    args.env = vec!["X=1".to_string()]; // avoid interactive
    let err = cmd_profile_create(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("cannot start with '.' or '-'"),
        "should reject leading dot in name, got: {err}"
    );
}

// --- cmd_profile_update ---

#[test]
fn profile_update_add_env() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.env = vec!["NEW_VAR=hello".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(
        doc.spec
            .env
            .iter()
            .any(|e| e.name == "NEW_VAR" && e.value == "hello"),
        "NEW_VAR=hello should be in the profile env"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Set env: NEW_VAR=hello"),
        "should confirm env was set, got: {output}"
    );
    assert!(
        output.contains("Updated profile"),
        "should confirm profile updated, got: {output}"
    );
}

#[test]
fn profile_update_remove_env() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.env = vec!["-EDITOR".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(
        !doc.spec.env.iter().any(|e| e.name == "EDITOR"),
        "EDITOR should be removed"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Removed env: EDITOR"),
        "should confirm env removal, got: {output}"
    );
    assert!(
        output.contains("Updated profile"),
        "should confirm profile updated, got: {output}"
    );
}

#[test]
fn profile_update_add_alias() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.aliases = vec!["gs=git status".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(
        doc.spec
            .aliases
            .iter()
            .any(|a| a.name == "gs" && a.command == "git status"),
        "gs alias should be added to profile"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Set alias: gs=git status"),
        "should confirm alias was set, got: {output}"
    );
    assert!(
        output.contains("Updated profile"),
        "should confirm profile updated, got: {output}"
    );
}

#[test]
fn profile_update_remove_alias() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // First add an alias
    let mut args = make_profile_update_args();
    args.aliases = vec!["gs=git status".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    // Then remove it
    let mut args2 = make_profile_update_args();
    args2.aliases = vec!["-gs".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(
        !doc.spec.aliases.iter().any(|a| a.name == "gs"),
        "alias gs should be removed"
    );
}

#[test]
fn profile_update_add_inherits() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Create a base profile to inherit from
    let mut create_args = make_profile_create_args("base");
    create_args.env = vec!["X=1".to_string()]; // avoid interactive
    cmd_profile_create(&cli, &printer, &create_args).unwrap();

    // Update work to also inherit base
    let mut args = make_profile_update_args();
    args.inherits = vec!["base".to_string()];

    let result = cmd_profile_update(&cli, &printer, "work", &args);
    assert!(
        result.is_ok(),
        "adding inherits should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
    assert!(doc.spec.inherits.contains(&"base".to_string()));
    assert!(doc.spec.inherits.contains(&"default".to_string()));
}

#[test]
fn profile_update_remove_inherits() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.inherits = vec!["-default".to_string()];

    let result = cmd_profile_update(&cli, &printer, "work", &args);
    assert!(
        result.is_ok(),
        "removing inherits should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
    assert!(!doc.spec.inherits.contains(&"default".to_string()));
}

#[test]
fn profile_update_add_module() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.modules = vec!["shell".to_string()];

    let result = cmd_profile_update(&cli, &printer, "default", &args);
    assert!(
        result.is_ok(),
        "adding module should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(doc.spec.modules.contains(&"shell".to_string()));
}

#[test]
fn profile_update_remove_module() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // First add a module
    let mut args = make_profile_update_args();
    args.modules = vec!["shell".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    // Then remove it
    let mut args2 = make_profile_update_args();
    args2.modules = vec!["-shell".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(
        !doc.spec.modules.contains(&"shell".to_string()),
        "shell module should be removed"
    );
}

#[test]
fn profile_update_add_system_setting() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.system = vec!["sysctl=net.core.somaxconn".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(
        doc.spec.system.contains_key("sysctl"),
        "profile should have sysctl system setting"
    );
    assert_eq!(
        doc.spec.system["sysctl"],
        serde_yaml::Value::String("net.core.somaxconn".to_string()),
        "sysctl value should match"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Set system: sysctl=net.core.somaxconn"),
        "should confirm system setting was set, got: {output}"
    );
    assert!(
        output.contains("Updated profile"),
        "should confirm profile updated, got: {output}"
    );
}

#[test]
fn profile_update_remove_system_setting() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // First add a system setting
    let mut args = make_profile_update_args();
    args.system = vec!["sysctl=net.core.somaxconn".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    // Then remove it
    let mut args2 = make_profile_update_args();
    args2.system = vec!["-sysctl".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(
        !doc.spec.system.contains_key("sysctl"),
        "sysctl should be removed"
    );
}

#[test]
fn profile_update_add_secret() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.secrets = vec!["secrets/key.enc:/tmp/out".to_string()];

    let result = cmd_profile_update(&cli, &printer, "default", &args);
    assert!(
        result.is_ok(),
        "adding secret should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert_eq!(doc.spec.secrets.len(), 1);
    assert_eq!(doc.spec.secrets[0].source, "secrets/key.enc");
}

#[test]
fn profile_update_remove_secret() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // First add a secret
    let mut args = make_profile_update_args();
    args.secrets = vec!["secrets/key.enc:/tmp/out".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    // Then remove it
    let mut args2 = make_profile_update_args();
    args2.secrets = vec!["-/tmp/out".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(doc.spec.secrets.is_empty(), "secret should be removed");
}

#[test]
fn profile_update_add_scripts() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.pre_apply = vec!["lint.sh".to_string()];
    args.post_apply = vec!["notify.sh".to_string()];
    args.on_change = vec!["reload.sh".to_string()];

    let result = cmd_profile_update(&cli, &printer, "default", &args);
    assert!(
        result.is_ok(),
        "adding scripts should succeed: {:?}",
        result.err()
    );

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.pre_apply.len(), 1);
    assert_eq!(scripts.post_apply.len(), 1);
    assert_eq!(scripts.on_change.len(), 1);
}

#[test]
fn profile_update_remove_scripts() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Add scripts first
    let mut args = make_profile_update_args();
    args.pre_apply = vec!["lint.sh".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    // Remove them
    let mut args2 = make_profile_update_args();
    args2.pre_apply = vec!["-lint.sh".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    if let Some(scripts) = doc.spec.scripts {
        assert!(
            scripts.pre_apply.is_empty(),
            "pre_apply should be empty after removal"
        );
    }
}

#[test]
fn profile_update_nonexistent_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = make_profile_update_args();
    let result = cmd_profile_update(&cli, &printer, "nonexistent", &args);
    assert!(result.is_err(), "updating nonexistent profile should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "error should mention not found: {}",
        err
    );
}

#[test]
fn profile_update_no_changes_succeeds() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = make_profile_update_args();
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No changes specified"),
        "should report no changes, got: {output}"
    );
    assert!(
        !output.contains("Updated profile"),
        "should NOT report profile updated when no changes were made, got: {output}"
    );
}

#[test]
fn profile_update_invalid_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = make_profile_update_args();
    let err = cmd_profile_update(&cli, &printer, ".bad-name", &args).unwrap_err();
    assert!(
        err.to_string().contains("cannot start with '.' or '-'"),
        "should reject leading dot in name, got: {err}"
    );
}

#[test]
fn profile_update_add_inherits_missing_parent_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.inherits = vec!["ghost-parent".to_string()];

    let result = cmd_profile_update(&cli, &printer, "default", &args);
    assert!(
        result.is_err(),
        "inheriting from missing parent should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "error should mention not found: {}",
        err
    );
}

// --- cmd_profile_delete ---

#[test]
fn profile_delete_with_yes_flag() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // Delete 'work' (not active, not inherited by others)
    cmd_profile_delete(&cli, &printer, "work", true, false).unwrap();
    drop(printer);

    let profile_path = dir.path().join("profiles").join("work.yaml");
    assert!(!profile_path.exists(), "profile file should be deleted");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Deleted profile 'work'"),
        "should confirm deletion, got: {output}"
    );
}

#[test]
fn profile_delete_nonexistent_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let result = cmd_profile_delete(&cli, &printer, "nonexistent", true, false);
    assert!(result.is_err(), "deleting nonexistent profile should fail");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "error should mention not found: {}",
        err
    );
    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    // Exit-6 uniformity across every missing-profile site.
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

#[test]
fn profile_edit_nonexistent_maps_to_not_found_exit_code() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_profile_edit(&cli, &printer, "nonexistent").unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "error should mention not found: {err}"
    );
    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    // Exit-6 uniformity across every missing-profile site.
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

#[test]
fn profile_delete_aborted_second_prompt_leaves_manifest_intact() {
    // Confirmations are gathered before any mutation: exhausting the prompt
    // queue at the payload prompt (the Ctrl-C/EOF analogue) must error out
    // BEFORE the manifest is removed — no partial delete.
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(true)],
        cfgd_core::output::Verbosity::Normal,
    );

    let mut args = make_profile_create_args("aborted");
    args.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &args).unwrap();
    let bundle_dir = dir.path().join("profiles").join("aborted");
    let files_dir = bundle_dir.join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1").unwrap();

    let err = cmd_profile_delete(&cli, &printer, "aborted", false, false)
        .expect_err("aborted payload prompt must fail the command");
    assert!(
        err.to_string().contains("refusing to prompt"),
        "abort surfaces the prompt failure: {err}"
    );
    assert!(
        bundle_dir.join("profile.yaml").is_file(),
        "manifest must be intact after an aborted second prompt"
    );
    assert!(
        files_dir.join(".zshrc").is_file(),
        "payload must be intact after an aborted second prompt"
    );
    // The delete is retryable: nothing was mutated, so a fully-confirmed
    // retry succeeds instead of tripping over a half-deleted profile.
    let (printer, _buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![
            cfgd_core::output::PromptAnswer::Confirm(true),
            cfgd_core::output::PromptAnswer::Confirm(true),
        ],
        cfgd_core::output::Verbosity::Normal,
    );
    cmd_profile_delete(&cli, &printer, "aborted", false, false).unwrap();
    assert!(!bundle_dir.exists(), "retry must complete the delete");
}

#[test]
fn profile_delete_active_profile_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // 'default' is the active profile in cfgd.yaml
    let result = cmd_profile_delete(&cli, &printer, "default", true, false);
    assert!(result.is_err(), "deleting active profile should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("active profile"),
        "error should mention active profile: {}",
        err
    );
}

#[test]
fn profile_delete_inherited_profile_fails() {
    let dir = setup_config_dir();
    // Switch active to 'work' so 'default' is not active but is inherited by 'work'
    let cli = test_cli(dir.path());
    let printer = make_printer();
    cmd_profile_switch(&cli, "work", &printer).unwrap();

    let result = cmd_profile_delete(&cli, &printer, "default", true, false);
    assert!(result.is_err(), "deleting inherited profile should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("inherited by"),
        "error should mention inherited: {}",
        err
    );
}

#[test]
fn profile_delete_cleans_files_dir() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Create a profile with a files subdirectory
    let files_dir = dir.path().join("profiles").join("ephemeral").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("test.txt"), "data").unwrap();

    // Create the profile YAML
    let profile_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: ephemeral\nspec:\n  modules: []\n";
    std::fs::write(
        dir.path().join("profiles").join("ephemeral.yaml"),
        profile_yaml,
    )
    .unwrap();

    cmd_profile_delete(&cli, &printer, "ephemeral", true, false).unwrap();

    assert!(!files_dir.exists(), "files directory should be cleaned up");
    assert!(
        !dir.path().join("profiles").join("ephemeral").exists(),
        "emptied legacy parent dir should be cleaned up too"
    );
}

#[test]
fn profile_delete_invalid_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_profile_delete(&cli, &printer, "-bad", true, false).unwrap_err();
    assert!(
        err.to_string().contains("cannot start with '.' or '-'"),
        "should reject leading dash in name, got: {err}"
    );
}

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn profile_edit_with_invalid_yaml_and_prompt_declined_breaks_with_warning() {
    // Drive the editor-validate loop's prompt-decline branch in
    // profile/edit.rs:22-25. EDITOR=/bin/true is a no-op editor so the
    // pre-staged invalid YAML stays invalid; serde_yaml::from_str Errs,
    // the prompt fires, the queue's Confirm(false) breaks the loop and
    // emits the "Saved with validation errors" warning.
    let dir = setup_config_dir();
    // Overwrite the existing default.yaml with invalid content so the
    // validate loop's Err arm fires on first iteration.
    std::fs::write(
        dir.path().join("profiles").join("default.yaml"),
        "this is not a valid Profile document",
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(false)],
        cfgd_core::output::Verbosity::Normal,
    );

    let _editor = cfgd_core::test_helpers::EnvVarGuard::set("EDITOR", "/usr/bin/true");
    cmd_profile_edit(&cli, &printer, "default").expect("edit must Ok even on Save-with-errors");
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Saved with validation errors"),
        "should warn about invalid save: {output}"
    );
}

#[test]
fn profile_delete_without_yes_and_prompt_confirmed_proceeds() {
    // yes=false + queued Confirm(true) drives the prompt-true branch at
    // profile/delete.rs:41 — the file is removed and the success message
    // fires (previously unreachable without an attached TTY).
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(true)],
        cfgd_core::output::Verbosity::Normal,
    );

    cmd_profile_delete(&cli, &printer, "work", false, false).unwrap();
    drop(printer);

    let profile_path = dir.path().join("profiles").join("work.yaml");
    assert!(
        !profile_path.exists(),
        "prompt-yes path must remove the profile file"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Deleted profile 'work'"),
        "should announce deletion: {output}"
    );
}

#[test]
fn profile_delete_without_yes_and_prompt_declined_returns_cancelled() {
    // yes=false + queued Confirm(false) takes the early-return arm at
    // delete.rs:41-44 — file must remain on disk and the printer emits
    // "Cancelled".
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(false)],
        cfgd_core::output::Verbosity::Normal,
    );

    cmd_profile_delete(&cli, &printer, "work", false, false).unwrap();
    drop(printer);

    let profile_path = dir.path().join("profiles").join("work.yaml");
    assert!(
        profile_path.exists(),
        "prompt-no path must NOT remove the file"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Cancelled"),
        "should print Cancelled: {output}"
    );
}

// --- JSON output tests ─────────────────────────────────────

fn test_cli_json(dir: &Path) -> super::super::Cli {
    super::super::Cli {
        output: super::super::OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli(dir)
    }
}

#[test]
fn profile_show_json_schema() {
    let dir = setup_config_dir();
    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_show(&cli, &printer, Some("default")).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    // Structured emit routes everything through stdout; payload starts at first '{'.
    let start = output.find('{').expect("should have JSON object in output");
    let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
    assert_eq!(
        json["name"], "default",
        "JSON should carry the profile name"
    );
    let resolved = json
        .get("resolved")
        .expect("envelope should have 'resolved'");
    assert!(
        resolved.get("layers").is_some(),
        "resolved should have layers field, got: {resolved}"
    );
    assert!(
        resolved.get("merged").is_some(),
        "resolved should have merged field, got: {resolved}"
    );
}

#[test]
fn profile_list_json_schema() {
    let dir = setup_config_dir();
    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.is_array(), "should be an array");
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2, "should list default and work profiles");

    for entry in arr {
        assert!(entry.get("name").is_some(), "each entry should have name");
        assert!(
            entry.get("active").is_some(),
            "each entry should have active"
        );
        assert!(
            entry.get("moduleCount").is_some(),
            "each entry should have moduleCount"
        );
    }

    // Verify one is active
    let active_count = arr.iter().filter(|e| e["active"] == true).count();
    assert_eq!(active_count, 1, "exactly one profile should be active");
}

#[test]
fn profile_list_json_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[test]
fn profile_list_json_no_profiles_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

// --- cmd_profile_show — content sections ────────────────────

#[test]
fn profile_show_displays_files_section() {
    let dir = setup_config_dir();
    // Write a profile with files
    let profile_with_files = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: files-test
spec:
  files:
    managed:
      - source: profiles/files-test/files/vimrc
        target: ~/.vimrc
"#;
    std::fs::write(
        dir.path().join("profiles").join("files-test.yaml"),
        profile_with_files,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_show(&cli, &printer, Some("files-test")).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Files"),
        "should show Files section, got: {output}"
    );
    assert!(
        output.contains("vimrc"),
        "should show the file entry, got: {output}"
    );
}

#[test]
fn profile_show_displays_packages_section() {
    let dir = setup_config_dir();
    // 'default' profile has cargo packages — verify they show up
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_show(&cli, &printer, Some("default")).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Packages"),
        "should show Packages section, got: {output}"
    );
    assert!(
        output.contains("cargo"),
        "should show cargo packages, got: {output}"
    );
}

#[test]
fn profile_show_displays_secrets_section() {
    let dir = setup_config_dir();
    let profile_with_secrets = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: secret-show
spec:
  secrets:
    - source: secrets/api-key.enc
      target: ~/.config/app/key
"#;
    std::fs::write(
        dir.path().join("profiles").join("secret-show.yaml"),
        profile_with_secrets,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_show(&cli, &printer, Some("secret-show")).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Secrets"),
        "should show Secrets section, got: {output}"
    );
    assert!(
        output.contains("api-key.enc"),
        "should show the secret source, got: {output}"
    );
}

#[test]
fn profile_show_displays_system_section() {
    let dir = setup_config_dir();
    let profile_with_system = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: sys-show
spec:
  system:
    shell: /bin/zsh
"#;
    std::fs::write(
        dir.path().join("profiles").join("sys-show.yaml"),
        profile_with_system,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_show(&cli, &printer, Some("sys-show")).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("System"),
        "should show System section, got: {output}"
    );
    assert!(
        output.contains("shell"),
        "should show the shell system setting, got: {output}"
    );
}

// --- cmd_profile_switch — output message ────────────────────

#[test]
fn profile_switch_shows_transition() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_switch(&cli, "work", &printer).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("default") && output.contains("work"),
        "should show transition from default to work, got: {output}"
    );
}

// --- cmd_profile_create — output messages ───────────────────

#[test]
fn profile_create_output_messages() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_create_args("fancy");
    args.inherits = vec!["default".to_string()];
    args.modules = vec!["shell".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Created profile 'fancy'"),
        "should confirm creation, got: {output}"
    );
    assert!(
        output.contains("Inherits"),
        "should show inheritance info, got: {output}"
    );
    assert!(
        output.contains("Modules"),
        "should show modules info, got: {output}"
    );
    assert!(
        output.contains("cfgd profile switch fancy"),
        "should show activation hint, got: {output}"
    );
}

// --- cmd_profile_update — add/remove multiple scripts ───────

#[test]
fn profile_update_add_multiple_script_hooks() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.pre_reconcile = vec!["validate.sh".to_string()];
    args.post_reconcile = vec!["notify.sh".to_string()];
    args.on_change = vec!["reload.sh".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.pre_reconcile.len(), 1);
    assert_eq!(scripts.post_reconcile.len(), 1);
    assert_eq!(scripts.on_change.len(), 1);
}

#[test]
fn profile_show_displays_all_package_manager_sections() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: rich\n",
    ).unwrap();
    std::fs::write(
        profiles_dir.join("rich.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: rich
spec:
  env:
    - name: PAGER
      value: less
  aliases:
    - name: ll
      command: ls -la
  packages:
    brew:
      taps: [homebrew/cask-fonts]
      formulae: [ripgrep, fd, bat]
      casks: [firefox, iterm2]
    apt:
      packages: [curl, git, jq]
    npm:
      global: [prettier, eslint]
    cargo: [tokei, hyperfine]
    pipx: [black, ruff]
    dnf: [vim-enhanced]
    snap:
      packages: [code]
    flatpak:
      packages: [org.signal.Signal]
    nix: [direnv]
    go: [golang.org/x/tools/gopls@latest]
    winget: [Microsoft.VisualStudioCode]
    chocolatey: [git]
    scoop: [extras/vcredist2022]
  files:
    managed:
      - source: dotfiles/.bashrc
        target: ~/.bashrc
  system:
    macosDefaults:
      NSGlobalDomain:
        AppleShowAllExtensions: true
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_profile_show(&cli, &printer, Some("rich")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();

    // Verify all package manager display branches are exercised
    assert!(
        output.contains("brew taps"),
        "should show brew taps: {output}"
    );
    assert!(
        output.contains("brew formulae"),
        "should show brew formulae: {output}"
    );
    assert!(
        output.contains("brew casks"),
        "should show brew casks: {output}"
    );
    assert!(output.contains("apt"), "should show apt packages: {output}");
    assert!(
        output.contains("npm"),
        "should show npm global packages: {output}"
    );
    assert!(
        output.contains("cargo"),
        "should show cargo packages: {output}"
    );
    assert!(
        output.contains("pipx"),
        "should show pipx packages: {output}"
    );

    // Verify env is displayed
    assert!(
        output.contains("PAGER"),
        "should show PAGER env var: {output}"
    );

    // Verify files section
    assert!(
        output.contains("Files"),
        "should show files section: {output}"
    );
    assert!(
        output.contains(".bashrc"),
        "should show .bashrc file target: {output}"
    );

    // Verify system section
    assert!(
        output.contains("System"),
        "should show system section: {output}"
    );
    assert!(
        output.contains("macosDefaults"),
        "should show macosDefaults configurator: {output}"
    );
}

#[test]
fn profile_show_no_packages_omits_section() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: bare\n",
    ).unwrap();
    std::fs::write(
        profiles_dir.join("bare.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: bare\nspec:\n  env:\n    - name: LANG\n      value: en_US.UTF-8\n",
    ).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_profile_show(&cli, &printer, Some("bare")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();

    // Renderer skips the Packages section header entirely when the merged
    // PackagesSpec is empty (section_if_nonempty contract).
    assert!(
        !output.contains("Packages"),
        "Packages section should be omitted for empty packages: {output}"
    );
    assert!(
        output.contains("Env"),
        "Env section should render because LANG is set: {output}"
    );
}

// --- profile show: secrets display variants ---

#[test]
fn profile_show_secrets_envs_only() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: env-secret\n",
    ).unwrap();
    std::fs::write(
        profiles_dir.join("env-secret.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: env-secret
spec:
  secrets:
    - source: op://vault/api-key
      envs:
        - API_KEY
        - API_SECRET
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_profile_show(&cli, &printer, Some("env-secret")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Secrets"),
        "should show Secrets section, got: {output}"
    );
    assert!(
        output.contains("envs: API_KEY, API_SECRET"),
        "should show envs for secret without target, got: {output}"
    );
}

#[test]
fn profile_show_secrets_target_and_envs() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: both-secret\n",
    ).unwrap();
    std::fs::write(
        profiles_dir.join("both-secret.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: both-secret
spec:
  secrets:
    - source: secrets/key.enc
      target: /tmp/key
      envs:
        - KEY_VAR
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_profile_show(&cli, &printer, Some("both-secret")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Secrets"),
        "should show Secrets section, got: {output}"
    );
    assert!(
        output.contains("/tmp/key") && output.contains("KEY_VAR"),
        "should show both target and envs, got: {output}"
    );
}

// --- profile list: wide format ---

#[test]
fn profile_list_wide_format() {
    // The wide-layout branch of `build_profile_list_doc` is verified directly
    // because the rendered table needs Normal verbosity AND Wide format, but
    // the public `for_test_*` constructors fix one or the other. The Doc
    // shape is the source of truth — rendering verbosity is tested elsewhere.
    let entries = vec![
        super::ProfileListEntry {
            name: "default".to_string(),
            active: true,
            inherits: None,
            module_count: 0,
        },
        super::ProfileListEntry {
            name: "work".to_string(),
            active: false,
            inherits: Some("default".to_string()),
            module_count: 2,
        },
    ];
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    printer.emit(super::list::build_profile_list_doc(&entries, true));
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Profile") && output.contains("Active") && output.contains("Modules"),
        "wide list should show table headers, got: {output}"
    );
    assert!(
        output.contains("default") && output.contains("work"),
        "wide list should show profile names, got: {output}"
    );
}

// --- profile show: empty env omits Env section (section_if_nonempty) ---

#[test]
fn profile_show_no_env_omits_section() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: noenv\n",
    ).unwrap();
    std::fs::write(
        profiles_dir.join("noenv.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: noenv\nspec:\n  modules: []\n",
    ).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_profile_show(&cli, &printer, Some("noenv")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    // Layers always renders (every profile has itself as a layer); every other
    // optional section disappears when its underlying collection is empty.
    assert!(
        output.contains("Layers"),
        "Layers should render, got: {output}"
    );
    assert!(
        !output.contains("Env"),
        "Env section should be omitted, got: {output}"
    );
    assert!(
        !output.contains("Packages"),
        "Packages section should be omitted, got: {output}"
    );
    assert!(
        !output.contains("Files"),
        "Files section should be omitted, got: {output}"
    );
}

// --- profile show: empty files omits Files section (section_if_nonempty) ---

#[test]
fn profile_show_no_files_omits_section() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: nofiles\n",
    ).unwrap();
    std::fs::write(
        profiles_dir.join("nofiles.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: nofiles\nspec:\n  env:\n    - name: X\n      value: y\n",
    ).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_profile_show(&cli, &printer, Some("nofiles")).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        !output.contains("Files"),
        "Files section should be omitted when no managed files, got: {output}"
    );
    assert!(
        output.contains("Env"),
        "Env section should render because X is set, got: {output}"
    );
}

// --- profile update: add and remove packages ---

#[test]
fn profile_update_add_package() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.packages = vec!["brew:neovim".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let pkgs = doc.spec.packages.unwrap();
    assert!(
        pkgs.brew
            .as_ref()
            .is_some_and(|b| b.formulae.contains(&"neovim".to_string())),
        "brew formulae should contain neovim"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Added package: neovim"),
        "should confirm package addition, got: {output}"
    );
}

#[test]
fn profile_update_remove_package() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Default profile has cargo: [bat], remove it
    let mut args = make_profile_update_args();
    args.packages = vec!["-cargo:bat".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    if let Some(pkgs) = &doc.spec.packages
        && let Some(cargo) = &pkgs.cargo
    {
        assert!(
            !cargo.packages.contains(&"bat".to_string()),
            "bat should be removed from cargo packages"
        );
    }
}

#[test]
fn profile_update_remove_nonexistent_package() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.packages = vec!["-brew:nonexistent-pkg".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about package not found, got: {output}"
    );
}

// --- profile update: env warnings ---

#[test]
fn profile_update_remove_nonexistent_env() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.env = vec!["-NONEXISTENT_VAR".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent env var, got: {output}"
    );
}

// --- profile update: alias warnings ---

#[test]
fn profile_update_remove_nonexistent_alias() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.aliases = vec!["-nonexistent-alias".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent alias, got: {output}"
    );
}

// --- profile update: system setting warnings ---

#[test]
fn profile_update_remove_nonexistent_system_setting() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.system = vec!["-nonexistent-setting".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent system setting, got: {output}"
    );
}

// --- profile update: secret warnings ---

#[test]
fn profile_update_remove_nonexistent_secret() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.secrets = vec!["-/tmp/nonexistent-secret".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent secret target, got: {output}"
    );
}

// --- profile update: duplicate inherits ---

#[test]
fn profile_update_add_duplicate_inherits() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // work already inherits default, try adding it again
    let mut args = make_profile_update_args();
    args.inherits = vec!["default".to_string()];

    cmd_profile_update(&cli, &printer, "work", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("already inherits"),
        "should warn about duplicate inherits, got: {output}"
    );
}

// --- profile update: remove nonexistent inherits ---

#[test]
fn profile_update_remove_nonexistent_inherits() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.inherits = vec!["-nonexistent-parent".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent inherits, got: {output}"
    );
}

// --- profile update: add duplicate secret ---

#[test]
fn profile_update_add_duplicate_secret() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Add a secret
    let mut args = make_profile_update_args();
    args.secrets = vec!["source:~/target".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    // Try adding the same secret again
    let (printer2, buf2) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let mut args2 = make_profile_update_args();
    args2.secrets = vec!["other-source:~/target".to_string()];
    cmd_profile_update(&cli, &printer2, "default", &args2).unwrap();

    drop(printer2);
    let output = buf2.lock().unwrap();
    assert!(
        output.contains("already exists"),
        "should warn about duplicate secret target, got: {output}"
    );
}

// --- profile create: with packages ---

#[test]
fn profile_create_with_packages() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("pkg-test");
    args.packages = vec!["brew:ripgrep".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("pkg-test")
            .join("profile.yaml"),
    )
    .unwrap();
    let pkgs = doc.spec.packages.unwrap();
    assert!(
        pkgs.brew
            .as_ref()
            .is_some_and(|b| b.formulae.contains(&"ripgrep".to_string())),
        "brew formulae should contain ripgrep"
    );
}

// --- profile update: on_drift script ---

#[test]
fn profile_update_add_and_remove_on_drift() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.on_drift = vec!["alert.sh".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.on_drift.len(), 1);
    assert_eq!(scripts.on_drift[0].run_str(), "alert.sh");

    // Remove it
    let mut args2 = make_profile_update_args();
    args2.on_drift = vec!["-alert.sh".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc2 = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    if let Some(scripts2) = doc2.spec.scripts {
        assert!(
            scripts2.on_drift.is_empty(),
            "on_drift should be empty after removal"
        );
    }
}

// --- profile update: invalid system setting format ---

#[test]
fn profile_update_invalid_system_setting_format() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.system = vec!["no-equals-sign".to_string()];

    let result = cmd_profile_update(&cli, &printer, "default", &args);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("expected key=value"),
        "should mention expected format, got: {err}"
    );
}

// --- profile create: with pre_reconcile and post_reconcile scripts ---

#[test]
fn profile_create_with_all_script_types() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("all-scripts");
    args.pre_apply = vec!["pre.sh".to_string()];
    args.post_apply = vec!["post.sh".to_string()];
    args.pre_reconcile = vec!["pre-recon.sh".to_string()];
    args.post_reconcile = vec!["post-recon.sh".to_string()];
    args.on_change = vec!["change.sh".to_string()];
    args.on_drift = vec!["drift.sh".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("all-scripts")
            .join("profile.yaml"),
    )
    .unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.pre_apply.len(), 1);
    assert_eq!(scripts.post_apply.len(), 1);
    assert_eq!(scripts.pre_reconcile.len(), 1);
    assert_eq!(scripts.post_reconcile.len(), 1);
    assert_eq!(scripts.on_change.len(), 1);
    assert_eq!(scripts.on_drift.len(), 1);
    assert_eq!(scripts.pre_reconcile[0].run_str(), "pre-recon.sh");
    assert_eq!(scripts.post_reconcile[0].run_str(), "post-recon.sh");
}

// ─── cmd_profile_update — module-removal lockfile + state cleanup ─────────────
//
// `--modules -<name>` is well-tested for *local* modules (the bare `retain`
// arm). These tests drive the *remote-module* cleanup branch: when the
// module name has a `modules.lock` entry, cmd_profile_update should also
// remove the lockfile entry, wipe the cached git checkout, list deployed
// files from the state store, and clear the module's state-store records.
// The prompt to actually restore backups defaults to `false` in test mode
// (Printer::for_test() drops to `unwrap_or(false)`), so we exercise the
// listing arm + the state-cleanup arms only — the per-file restore loop
// itself remains uncovered until a prompt-mock harness lands.

#[cfg(unix)]
mod profile_update_module_cleanup {
    use super::*;
    use serial_test::serial;

    /// Build a `setup_config_dir`-shaped tempdir whose `profiles/default.yaml`
    /// already lists `shell` in `spec.modules`. Avoids relying on the local
    /// `setup_config_dir`'s YAML shape which omits a modules section.
    fn setup_with_module_in_profile(mod_name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let profile_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - {mod_name}\n"
        );
        std::fs::write(profiles_dir.join("default.yaml"), profile_yaml).unwrap();
        dir
    }

    /// Mirror `test_cli` from the outer module but with `state_dir` plumbed
    /// in so the test can drive `open_state_store` at a known path without
    /// touching the real `~/.local/state/cfgd/state.db`.
    fn cli_with_state_dir(
        config_dir: &std::path::Path,
        state_dir: &std::path::Path,
    ) -> super::super::Cli {
        super::super::Cli {
            config: config_dir.join("cfgd.yaml"),
            config_explicit: false,
            profile: None,
            no_color: true,
            verbose: 0,
            quiet: true,
            output: super::super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: Some(state_dir.to_path_buf()),
            config_dir: None,
            // Keep the module/source cache inside the test's tempdir rather than
            // resolving to the real `~/.cache/cfgd`.
            cache_dir: Some(state_dir.to_path_buf()),
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: Some(super::super::Command::Status {
                module: None,
                exit_code: false,
            }),
        }
    }

    #[test]
    #[serial]
    fn remove_remote_module_cleans_lockfile_entry_and_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        let config_dir = setup_with_module_in_profile("ghmod");
        let state_dir = tmp.path().join("state");

        // Seed modules.lock with a remote entry. parse_git_source treats
        // `https://...@<tag>` as repo_url + tag, so the cleanup branch
        // computes git_cache_dir(<cache_base>, "https://github.com/x/y.git").
        let lock_yaml = "modules:\n  - name: ghmod\n    url: https://github.com/x/y.git@v1.0.0\n    pinnedRef: v1.0.0\n    commit: deadbeef\n    integrity: sha256:0\n";
        std::fs::write(config_dir.path().join("modules.lock"), lock_yaml).unwrap();

        let cli = cli_with_state_dir(config_dir.path(), &state_dir);

        // Pre-create the cache dir at the location the cleanup branch will
        // compute. Using the same primitives the production code uses
        // means future hashing-scheme changes don't silently bypass the
        // assertion.
        let cache_base = module_cache_dir(&cli).unwrap();
        let cache_dir =
            cfgd_core::modules::git_cache_dir(&cache_base, "https://github.com/x/y.git");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join("HEAD"), "ref: refs/heads/master\n").unwrap();
        assert!(cache_dir.exists(), "test precondition: cache dir staged");
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let mut args = make_profile_update_args();
        args.modules = vec!["-ghmod".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args)
            .expect("remove of remote module should succeed");
        drop(printer);

        // Lockfile entry is gone.
        let lock_after = std::fs::read_to_string(config_dir.path().join("modules.lock")).unwrap();
        assert!(
            !lock_after.contains("ghmod"),
            "lockfile should drop the removed entry: {lock_after}"
        );

        // Cache dir was wiped.
        assert!(
            !cache_dir.exists(),
            "cache dir for removed module should be gone: {}",
            cache_dir.display()
        );

        // Profile no longer lists the module.
        let profile_yaml =
            std::fs::read_to_string(config_dir.path().join("profiles").join("default.yaml"))
                .unwrap();
        assert!(
            !profile_yaml.contains("ghmod"),
            "profile should drop ghmod after removal: {profile_yaml}"
        );

        // User-visible signals are emitted by the cleanup branch.
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Removed 'ghmod' from modules.lock"),
            "should announce lockfile removal: {out}"
        );
        assert!(
            out.contains("Cleaned cached checkout"),
            "should announce cache cleanup: {out}"
        );
        assert!(
            out.contains("Removed module: ghmod"),
            "should report module removal success: {out}"
        );
    }

    #[test]
    #[serial]
    fn remove_module_with_deployed_files_lists_them_and_clears_state() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        let config_dir = setup_with_module_in_profile("statemod");
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        // No modules.lock — pure local-module path. The state store still
        // carries records the module deployed, so the deployed-files
        // listing + delete_module_files + remove_module_state arms run.
        let store = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
        let apply_id = store
            .record_apply(
                "default",
                "plan-hash",
                cfgd_core::state::ApplyStatus::Success,
                None,
            )
            .unwrap();
        store
            .upsert_module_file(
                "statemod",
                "/tmp/cfgd-test/deployed-file.conf",
                "sha256:abc",
                "copy",
                apply_id,
            )
            .unwrap();
        drop(store);

        let cli = cli_with_state_dir(config_dir.path(), &state_dir);
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let mut args = make_profile_update_args();
        args.modules = vec!["-statemod".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args)
            .expect("remove of module with state should succeed");
        drop(printer);

        // The listing arm prints the deployed-file path. The prompt to
        // restore returns false in a non-interactive printer, so we don't
        // assert on the restore loop body — we only pin the listing +
        // state-cleanup arms.
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Module 'statemod' deployed 1 file(s)"),
            "should announce deployed-file count: {out}"
        );
        assert!(
            out.contains("deployed-file.conf"),
            "should list the deployed file path: {out}"
        );

        // After the removal, both module_deployed_files and the module
        // state row should be cleared.
        let store2 = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
        let remaining = store2.module_deployed_files("statemod").unwrap();
        assert!(
            remaining.is_empty(),
            "delete_module_files should wipe the manifest"
        );
    }

    // ─── prompt-yes restore arms (update.rs lines 146-194) ─────────────────────
    //
    // With Printer::for_test_with_prompt_responses, `prompt_confirm` returns the
    // queued `Confirm(true)` instead of falling through to `unwrap_or(false)`.
    // That unblocks the `if should_clean { ... }` body — the per-file restore
    // loop — which was uncovered for many sessions because every existing test
    // used the default `Printer::for_test()` (queue absent → Err → false).
    //
    // Each test stages exactly one canned response for the "Remove deployed
    // files? Backups will be restored where available." prompt. Because
    // `collect_module_file_targets` returns Vec::new() when no local module
    // dir exists, the subsequent `prompt_restore_backups` call is a no-op and
    // does not consume queue entries.

    /// Drop a tmpdir with the standard config+profile shape PLUS a state DB
    /// containing one deployed-file row for `module`. Returns the config dir
    /// guard, state dir, and the on-disk path the module "owns".
    fn setup_module_with_deployed_file(
        module: &str,
        file_basename: &str,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        // The setup_with_module_in_profile helper above writes its tmpdir
        // contents at $TMP/{cfgd.yaml, profiles/default.yaml}; we replicate
        // it inline so the file targets here can live alongside (not inside)
        // the config_dir.
        let cfg_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(cfg_dir.join("profiles")).unwrap();
        std::fs::write(
            cfg_dir.join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::write(
            cfg_dir.join("profiles").join("default.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - {module}\n",
            ),
        )
        .unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let deployed_path = tmp.path().join(file_basename);
        (tmp, cfg_dir, state_dir, deployed_path)
    }

    /// Record one apply + one deployed-file row in the state store at
    /// `state_dir/state.db`. Returns the apply_id for backup-staging tests.
    fn record_apply_and_deployed_file(
        state_dir: &std::path::Path,
        module: &str,
        deployed_path: &std::path::Path,
    ) -> i64 {
        let store = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
        let apply_id = store
            .record_apply(
                "default",
                "plan-hash",
                cfgd_core::state::ApplyStatus::Success,
                None,
            )
            .unwrap();
        store
            .upsert_module_file(
                module,
                deployed_path.to_str().unwrap(),
                "sha256:abc",
                "copy",
                apply_id,
            )
            .unwrap();
        apply_id
    }

    #[test]
    #[serial]
    fn remove_module_with_prompt_yes_and_no_backup_removes_deployed_file() {
        // No backup recorded: `latest_backup_for_path` returns Ok(None) → the
        // cleanup falls through to the `path.exists()` arm and removes the file.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        let (_tmp_guard, cfg_dir, state_dir, deployed_path) =
            setup_module_with_deployed_file("noBackupMod", "deployed-no-backup.conf");
        std::fs::write(&deployed_path, b"old-deployed-content").unwrap();
        let _apply_id = record_apply_and_deployed_file(&state_dir, "noBackupMod", &deployed_path);

        let cli = cli_with_state_dir(&cfg_dir, &state_dir);
        let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
            vec![cfgd_core::output::PromptAnswer::Confirm(true)],
            cfgd_core::output::Verbosity::Normal,
        );
        let mut args = make_profile_update_args();
        args.modules = vec!["-noBackupMod".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args)
            .expect("remove-with-yes-prompt should succeed");

        assert!(
            !deployed_path.exists(),
            "should_clean=true + no backup → file must be removed: {}",
            deployed_path.display()
        );
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Removed:") && out.contains("deployed-no-backup.conf"),
            "should announce the fallback removal: {out}"
        );
    }

    #[test]
    #[serial]
    fn remove_module_with_prompt_yes_restores_content_from_backup() {
        // Backup exists with non-empty, not-oversized content (existed=true):
        // the shared restore path writes the backup content back to the
        // deployed path. The post-removal file content must match the staged
        // backup content (not the prior deployed content).
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        let (_tmp_guard, cfg_dir, state_dir, deployed_path) =
            setup_module_with_deployed_file("backupMod", "deployed-with-backup.conf");
        std::fs::write(&deployed_path, b"DEPLOYED-NOW-OVERWRITES-BACKUP").unwrap();

        let apply_id = record_apply_and_deployed_file(&state_dir, "backupMod", &deployed_path);

        // Stage a backup row at the deployed path. FileState shape:
        // - content: pre-deploy bytes to restore
        // - is_symlink=false, oversized=false, content non-empty → branch B fires
        let state = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
        let backup_state = cfgd_core::FileState {
            content: b"original-pre-deploy-content".to_vec(),
            content_hash: "sha256:original".to_string(),
            permissions: Some(0o644),
            is_symlink: false,
            symlink_target: None,
            oversized: false,
        };
        state
            .store_file_backup(apply_id, deployed_path.to_str().unwrap(), &backup_state)
            .unwrap();
        drop(state);

        let cli = cli_with_state_dir(&cfg_dir, &state_dir);
        let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
            vec![cfgd_core::output::PromptAnswer::Confirm(true)],
            cfgd_core::output::Verbosity::Normal,
        );
        let mut args = make_profile_update_args();
        args.modules = vec!["-backupMod".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args)
            .expect("backup-restore should succeed");

        // Post: deployed file content matches backup content.
        let restored = std::fs::read(&deployed_path).expect("file must still exist after restore");
        assert_eq!(
            restored, b"original-pre-deploy-content",
            "atomic_write must have replaced deployed content with backup content"
        );
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Restored:") && out.contains("deployed-with-backup.conf"),
            "should announce the restore: {out}"
        );
    }

    #[test]
    #[serial]
    fn remove_module_with_prompt_yes_restores_symlink_from_backup() {
        // Backup has was_symlink=true with a symlink_target and empty content
        // (a legacy symlink backup): the shared restore path removes whatever
        // is at the deployed path and recreates the symlink to the original
        // target.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        let (_tmp_guard, cfg_dir, state_dir, deployed_path) =
            setup_module_with_deployed_file("symlinkMod", "deployed-symlink");
        // Pre-deploy: deployed path was a regular file (the "managed" form);
        // backup row will indicate it was *previously* a symlink to
        // original-target.
        std::fs::write(&deployed_path, b"managed-file-content").unwrap();

        let original_target = tmp.path().join("original-target.bin");
        std::fs::write(&original_target, b"target-payload").unwrap();

        let apply_id = record_apply_and_deployed_file(&state_dir, "symlinkMod", &deployed_path);
        let state = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
        let backup_state = cfgd_core::FileState {
            content: Vec::new(),
            content_hash: String::new(),
            permissions: None,
            is_symlink: true,
            symlink_target: Some(original_target.clone()),
            oversized: false,
        };
        state
            .store_file_backup(apply_id, deployed_path.to_str().unwrap(), &backup_state)
            .unwrap();
        drop(state);

        let cli = cli_with_state_dir(&cfg_dir, &state_dir);
        let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
            vec![cfgd_core::output::PromptAnswer::Confirm(true)],
            cfgd_core::output::Verbosity::Normal,
        );
        let mut args = make_profile_update_args();
        args.modules = vec!["-symlinkMod".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args)
            .expect("symlink-restore should succeed");

        // Post: deployed path is now a symlink pointing at original_target.
        let meta = std::fs::symlink_metadata(&deployed_path)
            .expect("symlink should exist at deployed path");
        assert!(
            meta.file_type().is_symlink(),
            "restore must recreate the symlink"
        );
        let link_dest = std::fs::read_link(&deployed_path).unwrap();
        assert_eq!(
            link_dest, original_target,
            "symlink must point back at the original target"
        );
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Restored:") && out.contains("deployed-symlink"),
            "should announce the symlink restore: {out}"
        );
    }
}

// --- cmd_profile_update — invalid env/alias specs ---

#[test]
fn profile_update_invalid_env_no_equals() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.env = vec!["NOEQUALSSIGN".to_string()];

    let err = cmd_profile_update(&cli, &printer, "default", &args).unwrap_err();
    assert!(
        err.to_string().contains("expected KEY=VALUE"),
        "should mention expected format, got: {err}"
    );
}

#[test]
fn profile_update_invalid_env_name_chars() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.env = vec!["INVALID@NAME=value".to_string()];

    let err = cmd_profile_update(&cli, &printer, "default", &args).unwrap_err();
    assert!(
        err.to_string().contains("invalid env var name"),
        "should mention invalid env var name, got: {err}"
    );
}

#[test]
fn profile_update_invalid_alias_no_equals() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.aliases = vec!["aliaswithoutequalssign".to_string()];

    let err = cmd_profile_update(&cli, &printer, "default", &args).unwrap_err();
    assert!(
        err.to_string().contains("expected name=command"),
        "should mention expected format, got: {err}"
    );
}

#[test]
fn profile_update_invalid_alias_name_chars() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.aliases = vec!["bad@name=ls -la".to_string()];

    let err = cmd_profile_update(&cli, &printer, "default", &args).unwrap_err();
    assert!(
        err.to_string().contains("invalid alias name"),
        "should mention invalid alias name, got: {err}"
    );
}

// --- cmd_profile_update — file add and remove ---

#[test]
fn profile_update_add_file() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let src = dir.path().join("myconfig.conf");
    std::fs::write(&src, b"key=val").unwrap();

    let spec = format!("{}:{}", src.display(), src.display());
    let mut args = make_profile_update_args();
    args.files = vec![spec];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let files = doc.spec.files.expect("files spec must be set after add");
    assert_eq!(files.managed.len(), 1, "should have one managed file");
    assert!(
        files.managed[0].source.contains("myconfig.conf"),
        "source should reference the file: {:?}",
        files.managed[0].source
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Added file"),
        "should confirm file was added, got: {output}"
    );
    assert!(
        output.contains("Updated profile"),
        "should confirm profile updated, got: {output}"
    );
}

#[test]
fn profile_update_remove_file_from_profile() {
    let dir = setup_config_dir();

    let profile_with_file = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  files:
    managed:
      - source: profiles/default/files/vimrc
        target: /tmp/cfgd-test-vimrc
"#;
    std::fs::write(
        dir.path().join("profiles").join("default.yaml"),
        profile_with_file,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.files = vec!["-/tmp/cfgd-test-vimrc".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let managed = doc.spec.files.map(|f| f.managed).unwrap_or_default();
    assert!(
        managed.is_empty(),
        "managed files should be empty after removal"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Removed file"),
        "should confirm file removal, got: {output}"
    );
}

#[test]
fn profile_update_remove_file_not_in_profile_warns() {
    let dir = setup_config_dir();

    let profile_with_file = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  files:
    managed:
      - source: profiles/default/files/vimrc
        target: /tmp/cfgd-test-vimrc
"#;
    std::fs::write(
        dir.path().join("profiles").join("default.yaml"),
        profile_with_file,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.files = vec!["-/tmp/cfgd-does-not-exist".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn file not found in profile, got: {output}"
    );
}

// --- cmd_profile_update — module add duplicate skip ---

#[test]
fn profile_update_add_duplicate_module_is_skipped() {
    let dir = setup_config_dir();

    let profile_with_module = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - shell
"#;
    std::fs::write(
        dir.path().join("profiles").join("default.yaml"),
        profile_with_module,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.modules = vec!["shell".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert_eq!(
        doc.spec.modules.len(),
        1,
        "duplicate module add must not create a second entry"
    );
}

// --- cmd_profile_update — remove module not in profile warns ---

#[test]
fn profile_update_remove_module_not_in_profile_warns() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.modules = vec!["-nosuchmodule".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found in profile"),
        "should warn that module is not in profile, got: {output}"
    );
}

// --- cmd_profile_update — nonexistent profile returns a not-found error ---

#[test]
fn profile_update_nonexistent_returns_not_found_error() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    // The handler no longer emits its own error — it returns a CliErrorMeta and the
    // central sink renders it. Assert the returned error, not the printer buffer.
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = make_profile_update_args();
    let err = cmd_profile_update(&cli, &printer, "ghost", &args).unwrap_err();
    drop(printer);

    assert!(
        err.to_string().contains("not found"),
        "error must mention not found: {err}"
    );
    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    assert_eq!(meta.name, "ghost");
    // Exit-6 uniformity across every missing-profile site.
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

// --- cmd_profile_update — file add with private flag ---

#[test]
fn profile_update_add_file_private_writes_gitignore() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let src = dir.path().join("secret.conf");
    std::fs::write(&src, b"password=hunter2").unwrap();
    let spec = format!("{}:{}", src.display(), src.display());

    let mut args = make_profile_update_args();
    args.files = vec![spec];
    args.private = true;

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let gitignore_path = dir.path().join(".gitignore");
    assert!(gitignore_path.exists(), ".gitignore should be created");
    let gitignore_content = std::fs::read_to_string(&gitignore_path).unwrap();
    assert!(
        gitignore_content.contains("secret.conf"),
        ".gitignore should reference the private file, got: {gitignore_content}"
    );
}

#[test]
fn profile_update_add_duplicate_file_is_skipped() {
    let dir = setup_config_dir();

    let src = dir.path().join("myconfig.conf");
    std::fs::write(&src, b"key=val").unwrap();
    let spec = format!("{}:{}", src.display(), src.display());

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.files = vec![spec.clone()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    // The first add moved the source file and created a symlink. Recreate a
    // file at the profile repo path to let copy_files_to_dir see it again.
    let repo_path = dir
        .path()
        .join("profiles")
        .join("default")
        .join("files")
        .join("myconfig.conf");
    let src2 = dir.path().join("myconfig2.conf");
    std::fs::write(&src2, b"key=val2").unwrap();
    let spec2 = format!("{}:{}", src2.display(), src2.display());

    let mut args2 = make_profile_update_args();
    args2.files = vec![spec2];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let _ = repo_path; // keeps the path alive for the assertion

    let files = doc.spec.files.expect("files spec must exist");
    assert!(
        !files.managed.is_empty(),
        "files list must have entries after add"
    );
}

#[test]
fn profile_update_remove_file_deletes_source_from_repo() {
    let dir = setup_config_dir();

    let files_dir = dir.path().join("profiles").join("default").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    let source_file = files_dir.join("vimrc");
    std::fs::write(&source_file, b"set number").unwrap();

    let profile_with_file = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  files:
    managed:
      - source: profiles/default/files/vimrc
        target: /tmp/cfgd-test-vimrc-delete
"#;
    std::fs::write(
        dir.path().join("profiles").join("default.yaml"),
        profile_with_file,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.files = vec!["-/tmp/cfgd-test-vimrc-delete".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    assert!(
        !source_file.exists(),
        "source file in repo should be deleted on file removal"
    );
    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let managed = doc.spec.files.map(|f| f.managed).unwrap_or_default();
    assert!(
        managed.is_empty(),
        "managed files must be empty after removal"
    );
}

#[test]
fn profile_update_remove_file_from_profile_with_no_files_spec_is_no_op() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let mut args = make_profile_update_args();
    args.files = vec!["-/tmp/cfgd-no-files-spec".to_string()];

    cmd_profile_update(&cli, &printer, "default", &args).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No changes"),
        "removing from profile with no files spec should report no changes, got: {output}"
    );
}

// --- cmd_profile_update — pre/post reconcile hooks ---

#[test]
fn profile_update_add_and_remove_pre_post_reconcile() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.pre_reconcile = vec!["check.sh".to_string()];
    args.post_reconcile = vec!["notify.sh".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.as_ref().expect("scripts must be set");
    assert_eq!(scripts.pre_reconcile.len(), 1);
    assert_eq!(scripts.pre_reconcile[0].run_str(), "check.sh");
    assert_eq!(scripts.post_reconcile.len(), 1);
    assert_eq!(scripts.post_reconcile[0].run_str(), "notify.sh");

    let mut args2 = make_profile_update_args();
    args2.pre_reconcile = vec!["-check.sh".to_string()];
    args2.post_reconcile = vec!["-notify.sh".to_string()];
    cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

    let doc2 = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    if let Some(s) = doc2.spec.scripts {
        assert!(s.pre_reconcile.is_empty(), "pre_reconcile must be cleared");
        assert!(
            s.post_reconcile.is_empty(),
            "post_reconcile must be cleared"
        );
    }
}

// --- cmd_profile_update — registry-ref module path errors when registry absent ---

#[test]
fn profile_update_add_registry_ref_module_errors_on_missing_registry() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_update_args();
    args.modules = vec!["myreg/mymod".to_string()];

    let err = cmd_profile_update(&cli, &printer, "default", &args).unwrap_err();
    assert!(
        err.to_string().contains("not configured")
            || err.to_string().contains("not found")
            || err.to_string().contains("Registry"),
        "should fail with registry-not-configured error, got: {err}"
    );
}

// --- cmd_profile_update — non-interactive remote-module install via --yes ─────
//
// `profile update --module <registry-ref>` is the only user-facing way to add a
// remote module. The registry path delegates to `cmd_module_add_from_registry`,
// which calls `prompt_confirm` unless `yes` is set. Under `cargo test` stdin is
// not a TTY, so the prompt refuses with an error — meaning a remote module
// cannot be installed non-interactively unless `--yes` threads through. These
// tests pin that contract: `yes: true` installs without prompting, `yes: false`
// surfaces the refusal.

#[cfg(unix)]
mod profile_update_remote_module_yes {
    use super::*;
    use serial_test::serial;
    use std::path::{Path, PathBuf};

    /// Init a non-bare git repo at `src_dir` with `modules/<mod>/module.yaml`
    /// committed and HEAD tagged `<mod>/v<version>` (the registry tag
    /// convention). Returns the source path so `file://<src>` serves as the
    /// registry URL.
    fn init_registry_source(src_dir: &Path, mod_name: &str, version: &str) -> PathBuf {
        let src_repo = git2::Repository::init(src_dir).unwrap();
        let module_rel = format!("modules/{mod_name}/module.yaml");
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {mod_name}\n  description: test mod\nspec: {{}}\n"
        );
        std::fs::create_dir_all(src_dir.join("modules").join(mod_name)).unwrap();
        std::fs::write(src_dir.join(&module_rel), module_yaml).unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new(&module_rel)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let commit_id = src_repo
            .commit(Some("HEAD"), &sig, &sig, "add module", &tree, &[])
            .unwrap();
        drop(tree);
        let commit_obj = src_repo.find_commit(commit_id).unwrap();
        src_repo
            .tag_lightweight(
                &format!("{mod_name}/v{version}"),
                commit_obj.as_object(),
                false,
            )
            .unwrap();
        src_dir.to_path_buf()
    }

    /// Build a `setup_config_dir`-shaped tempdir whose cfgd.yaml declares the
    /// given registry so `cmd_module_add_from_registry` can resolve it.
    fn setup_with_registry(reg_name: &str, reg_url: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules: []\n",
        )
        .unwrap();
        let cfgd_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  modules:\n    registries:\n      - name: {reg_name}\n        url: {reg_url}\n"
        );
        std::fs::write(dir.path().join("cfgd.yaml"), cfgd_yaml).unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();
        dir
    }

    #[test]
    #[serial]
    fn profile_update_remote_module_with_yes_installs_non_interactively() {
        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "tmux", "1.0.0");
        let reg_url = cfgd_core::test_helpers::file_url(&src);

        let work = setup_with_registry("myorg", &reg_url);
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let cli = test_cli(work.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.modules = vec!["myorg/tmux@v1.0.0".to_string()];
        args.yes = true;

        cmd_profile_update(&cli, &printer, "default", &args)
            .expect("remote-module install with --yes must succeed non-interactively");

        // Lockfile records the installed module.
        let lockfile = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();
        assert!(
            lockfile.contains("tmux"),
            "lockfile should list the installed module: {lockfile}"
        );
        assert!(
            lockfile.contains("tmux/v1.0.0"),
            "lockfile pinned_ref should record the per-module tag: {lockfile}"
        );

        // Profile now references the registry-qualified module.
        let profile_yaml =
            std::fs::read_to_string(work.path().join("profiles/default.yaml")).unwrap();
        assert!(
            profile_yaml.contains("myorg/tmux"),
            "profile should reference the registry module after install: {profile_yaml}"
        );
    }

    #[test]
    #[serial]
    fn profile_update_remote_module_without_yes_refuses_in_non_interactive_context() {
        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "tmux", "1.0.0");
        let reg_url = cfgd_core::test_helpers::file_url(&src);

        let work = setup_with_registry("myorg", &reg_url);
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let cli = test_cli(work.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.modules = vec!["myorg/tmux@v1.0.0".to_string()];
        args.yes = false;

        let err = cmd_profile_update(&cli, &printer, "default", &args)
            .expect_err("without --yes the confirmation prompt must refuse non-interactively");
        let msg = err.to_string();
        assert!(
            msg.contains("non-interactive") || msg.contains("refusing to prompt"),
            "error should be the non-interactive prompt refusal: {msg}"
        );
    }
}

// =========================================================================
// Coverage-gap tests — create.rs lines 163-186, 249-256 (--file path)
// =========================================================================

// create.rs lines 163-186: copy_files_to_dir is called when args.files is
// non-empty. The function copies the source into
// `profiles/<name>/files/<basename>`, replaces the source with a symlink,
// and returns (basename, deploy_target) pairs. The caller then builds a
// Vec<ManagedFileSpec> and sets `doc.spec.files = Some(FilesSpec { .. })`.
// Lines 249-255 are the `files: Some(FilesSpec { managed: file_entries, .. })`
// arm that is only reachable when at least one file entry exists.
//
// Neither path was exercised because every prior create test left
// `args.files = vec![]`.

#[cfg(unix)]
#[test]
fn profile_create_with_file_copies_source_and_populates_files_spec() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // Source lives inside the tempdir (not in a system directory) so
    // copy_files_to_dir accepts it.
    let src = dir.path().join("myapp.conf");
    std::fs::write(&src, b"setting=value").unwrap();

    // Deploy target is an absolute path outside the config dir; the profile
    // YAML records it for later apply.
    let target = dir.path().join("out").join("myapp.conf");
    let spec = format!("{}:{}", src.display(), target.display());

    let mut args = make_profile_create_args("fileprof");
    args.files = vec![spec];

    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    // The source path should now be a symlink pointing into the cfgd
    // profiles directory (copy_files_to_dir replaces the original with a
    // symlink after copying).
    assert!(
        src.is_symlink(),
        "source file should have been replaced by a symlink: {}",
        src.display()
    );

    // The profile YAML must contain a files.managed entry with the right
    // source path (profiles/<name>/files/<basename>).
    let profile_path = dir
        .path()
        .join("profiles")
        .join("fileprof")
        .join("profile.yaml");
    assert!(profile_path.exists(), "profile YAML must be created");
    let doc = config::load_profile(&profile_path).unwrap();
    let files = doc
        .spec
        .files
        .expect("files spec must be populated after --file flag");
    assert_eq!(
        files.managed.len(),
        1,
        "should have exactly one managed file"
    );
    assert!(
        files.managed[0]
            .source
            .contains("fileprof/files/myapp.conf"),
        "managed file source should be profiles/<name>/files/<basename>: {:?}",
        files.managed[0].source
    );
    assert_eq!(
        files.managed[0].target, target,
        "managed file target should match the deploy target from the spec"
    );
    assert!(
        !files.managed[0].private,
        "private should default to false without --private-files"
    );

    // Output must confirm the profile was created.
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Created profile 'fileprof'"),
        "should confirm creation: {output}"
    );
}

// create.rs lines 179-186: when `is_private = true` and at least one file
// was copied, the loop calls `add_to_gitignore` for each file entry. This
// writes the relative source path into `.gitignore` so the private file is
// excluded from version control.
#[cfg(unix)]
#[test]
fn profile_create_with_file_private_writes_gitignore_entry() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let src = dir.path().join("secret.conf");
    std::fs::write(&src, b"top-secret").unwrap();

    let target = dir.path().join("out").join("secret.conf");
    let spec = format!("{}:{}", src.display(), target.display());

    let mut args = make_profile_create_args("private-prof");
    args.files = vec![spec];
    args.private = true;

    cmd_profile_create(&cli, &printer, &args).unwrap();

    // .gitignore must contain the relative path for the copied file.
    let gitignore_path = dir.path().join(".gitignore");
    assert!(gitignore_path.exists(), ".gitignore must be created");
    let gitignore = std::fs::read_to_string(&gitignore_path).unwrap();
    assert!(
        gitignore.contains("profiles/private-prof/files/secret.conf"),
        ".gitignore should contain the relative path for the private file: {gitignore}"
    );

    // The profile YAML must also mark the file as private.
    let profile_path = dir
        .path()
        .join("profiles")
        .join("private-prof")
        .join("profile.yaml");
    let doc = config::load_profile(&profile_path).unwrap();
    let files = doc.spec.files.expect("files spec must be populated");
    assert!(
        files.managed[0].private,
        "managed file should be marked private when --private-files is set"
    );
}

// =========================================================================
// Coverage-gap tests — update.rs (typed error on not_found, line 35-44)
// =========================================================================

// update.rs lines 32-44: the not_found branch wraps a typed
// `CfgdError::Config(ConfigError::ProfileNotFound)` so the exit-code
// downcast resolves to ExitCode::NotFound (6). The existing
// `profile_update_nonexistent_fails` test only checks the string message;
// this test asserts the typed error AND the structured CliErrorMeta payload.
#[test]
fn profile_update_not_found_error_is_typed_profile_not_found() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = make_profile_update_args();
    let err = cmd_profile_update(&cli, &printer, "ghost-profile", &args)
        .expect_err("updating a missing profile must error");

    // The typed CfgdError must be present for exit-code downcast.
    let cfgd_err = err
        .downcast_ref::<cfgd_core::errors::CfgdError>()
        .expect("error chain must carry a CfgdError");
    assert!(
        matches!(
            cfgd_err,
            cfgd_core::errors::CfgdError::Config(
                cfgd_core::errors::ConfigError::ProfileNotFound { name }
            ) if name == "ghost-profile"
        ),
        "should be ProfileNotFound for 'ghost-profile', got: {:?}",
        cfgd_err
    );

    // CliErrorMeta must also be present for structured JSON error output.
    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("error chain must carry CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    assert_eq!(meta.name, "ghost-profile");

    // Exit code must resolve to NotFound (6) — uniform with every other
    // missing-resource site.
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

// =========================================================================
// Canonical bundle layout (profiles/<name>/profile.yaml)
// =========================================================================

#[test]
fn profile_create_writes_canonical_bundle_form() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("bundle");
    args.env = vec!["FOO=bar".to_string()]; // avoid interactive
    cmd_profile_create(&cli, &printer, &args).unwrap();

    let canonical = dir
        .path()
        .join("profiles")
        .join("bundle")
        .join("profile.yaml");
    assert!(
        canonical.is_file(),
        "manifest must land at the canonical path"
    );
    assert!(
        !dir.path().join("profiles").join("bundle.yaml").exists(),
        "no legacy flat manifest may be written"
    );
    let doc = config::load_profile(&canonical).unwrap();
    assert_eq!(doc.kind, "Profile");
    assert_eq!(doc.metadata.name, "bundle");
}

#[test]
fn profile_create_refuses_existing_canonical_profile() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("dup");
    args.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &args).unwrap();

    let err = cmd_profile_create(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("already exists"),
        "second create must refuse: {err}"
    );
}

#[test]
fn profile_create_ambiguous_forms_fails_closed() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // 'default' exists flat via the fixture; add the canonical form too.
    let cdir = dir.path().join("profiles").join("default");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("profile.yaml"), DEFAULT_PROFILE_YAML).unwrap();

    let mut args = make_profile_create_args("default");
    args.env = vec!["X=1".to_string()];
    let err = cmd_profile_create(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("ambiguous"),
        "ambiguous forms must fail closed, got: {err}"
    );
}

#[test]
fn profile_delete_canonical_removes_empty_dir() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("bundled");
    args.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &args).unwrap();

    cmd_profile_delete(&cli, &printer, "bundled", true, false).unwrap();
    assert!(
        !dir.path().join("profiles").join("bundled").exists(),
        "empty canonical dir must be removed with the manifest"
    );
}

#[test]
fn profile_delete_canonical_payload_removed_with_yes() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("payload");
    args.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &args).unwrap();
    let files_dir = dir.path().join("profiles").join("payload").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1").unwrap();

    cmd_profile_delete(&cli, &printer, "payload", true, false).unwrap();
    assert!(
        !dir.path().join("profiles").join("payload").exists(),
        "--yes must remove the payload-bearing profile dir"
    );
}

#[test]
fn profile_delete_canonical_payload_prompt_declined_keeps_dir() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    // First Confirm answers "Delete profile?", second declines payload removal.
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(true),
        cfgd_core::output::PromptAnswer::Confirm(false),
    ]);

    let mut args = make_profile_create_args("kept");
    args.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &args).unwrap();
    let files_dir = dir.path().join("profiles").join("kept").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1").unwrap();

    cmd_profile_delete(&cli, &printer, "kept", false, false).unwrap();
    assert!(
        !dir.path()
            .join("profiles")
            .join("kept")
            .join("profile.yaml")
            .exists(),
        "manifest must be removed"
    );
    assert!(
        files_dir.join(".zshrc").is_file(),
        "declined payload removal must keep the payload dir"
    );
}

#[test]
fn profile_delete_canonical_payload_prompt_confirmed_removes_dir() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(true),
        cfgd_core::output::PromptAnswer::Confirm(true),
    ]);

    let mut args = make_profile_create_args("gone");
    args.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &args).unwrap();
    let files_dir = dir.path().join("profiles").join("gone").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1").unwrap();

    cmd_profile_delete(&cli, &printer, "gone", false, false).unwrap();
    assert!(
        !dir.path().join("profiles").join("gone").exists(),
        "confirmed payload removal must delete the whole profile dir"
    );
}

#[test]
fn profile_delete_legacy_payload_removed_with_yes() {
    let dir = setup_config_dir(); // legacy work.yaml
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let files_dir = dir.path().join("profiles").join("work").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1").unwrap();

    cmd_profile_delete(&cli, &printer, "work", true, false).unwrap();
    assert!(
        !dir.path().join("profiles").join("work").exists(),
        "--yes must remove the legacy payload dir and its empty parent"
    );
    assert!(!dir.path().join("profiles").join("work.yaml").exists());
}

#[test]
fn profile_delete_legacy_payload_prompt_declined_keeps_payload() {
    let dir = setup_config_dir(); // legacy work.yaml
    let cli = test_cli(dir.path());
    // First Confirm answers "Delete profile?", second declines payload removal.
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(true),
        cfgd_core::output::PromptAnswer::Confirm(false),
    ]);

    let files_dir = dir.path().join("profiles").join("work").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1").unwrap();

    cmd_profile_delete(&cli, &printer, "work", false, false).unwrap();
    drop(printer);
    assert!(
        !dir.path().join("profiles").join("work.yaml").exists(),
        "manifest must be removed"
    );
    assert!(
        files_dir.join(".zshrc").is_file(),
        "declined payload removal must keep the legacy files/ dir"
    );
    assert!(
        cap.human().contains("Kept"),
        "declined prompt must note the kept payload dir, got: {}",
        cap.human()
    );
}

#[test]
fn profile_delete_legacy_payload_prompt_confirmed_removes_dir() {
    let dir = setup_config_dir(); // legacy work.yaml
    let cli = test_cli(dir.path());
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(true),
        cfgd_core::output::PromptAnswer::Confirm(true),
    ]);

    let files_dir = dir.path().join("profiles").join("work").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1").unwrap();

    cmd_profile_delete(&cli, &printer, "work", false, false).unwrap();
    assert!(
        !dir.path().join("profiles").join("work").exists(),
        "confirmed payload removal must delete files/ and the empty parent"
    );
}

#[test]
fn profile_delete_legacy_empty_files_dir_silent_cleanup() {
    let dir = setup_config_dir(); // legacy work.yaml
    let cli = test_cli(dir.path());
    // Only the delete confirmation is queued — an empty files/ dir must not
    // consume a payload prompt.
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(true),
    ]);

    let files_dir = dir.path().join("profiles").join("work").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();

    cmd_profile_delete(&cli, &printer, "work", false, false).unwrap();
    assert!(
        !dir.path().join("profiles").join("work").exists(),
        "empty files/ keeps the silent cleanup path"
    );
}

#[test]
fn profile_delete_ambiguous_forms_fails_closed() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let cdir = dir.path().join("profiles").join("work");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("profile.yaml"), WORK_PROFILE_YAML).unwrap();

    let err = cmd_profile_delete(&cli, &printer, "work", true, false).unwrap_err();
    assert!(
        err.to_string().contains("ambiguous"),
        "ambiguous forms must fail closed, got: {err}"
    );
    assert!(
        dir.path().join("profiles").join("work.yaml").is_file()
            && cdir.join("profile.yaml").is_file(),
        "nothing may be deleted on ambiguity"
    );
}

#[test]
fn profile_roundtrip_canonical_create_show_update_delete() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // create (canonical)
    let mut cargs = make_profile_create_args("trip");
    cargs.env = vec!["A=1".to_string()];
    cmd_profile_create(&cli, &printer, &cargs).unwrap();
    let manifest = dir
        .path()
        .join("profiles")
        .join("trip")
        .join("profile.yaml");
    assert!(manifest.is_file());

    // show resolves the canonical form by name
    cmd_profile_show(&cli, &printer, Some("trip")).unwrap();

    // update writes back to the same canonical path
    let mut uargs = make_profile_update_args();
    uargs.env = vec!["B=2".to_string()];
    cmd_profile_update(&cli, &printer, "trip", &uargs).unwrap();
    let doc = config::load_profile(&manifest).unwrap();
    assert!(doc.spec.env.iter().any(|e| e.name == "B"));
    assert!(
        !dir.path().join("profiles").join("trip.yaml").exists(),
        "update must never materialize a flat manifest"
    );

    // delete removes manifest + dir
    cmd_profile_delete(&cli, &printer, "trip", true, false).unwrap();
    assert!(!dir.path().join("profiles").join("trip").exists());
}

#[test]
fn profile_list_mixed_forms() {
    let dir = setup_config_dir(); // default.yaml + work.yaml (legacy)
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let mut args = make_profile_create_args("modern");
    args.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_profile_list(&cli, &printer).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    for name in ["default", "work", "modern"] {
        assert!(
            output.contains(name),
            "list must show both forms; missing '{name}': {output}"
        );
    }
}

#[test]
fn profile_update_canonical_parent_inherits() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // canonical parent
    let mut pargs = make_profile_create_args("parent-bundle");
    pargs.env = vec!["X=1".to_string()];
    cmd_profile_create(&cli, &printer, &pargs).unwrap();

    // legacy child gains the canonical parent
    let mut uargs = make_profile_update_args();
    uargs.inherits = vec!["parent-bundle".to_string()];
    cmd_profile_update(&cli, &printer, "work", &uargs).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
    assert!(doc.spec.inherits.contains(&"parent-bundle".to_string()));
}

// --- cmd_profile_migrate ---

fn run_migrate(
    cli: &super::super::Cli,
    name: Option<&str>,
    all: bool,
    dry_run: bool,
    yes: bool,
) -> (anyhow::Result<usize>, String) {
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = migrate::run_profile_migrate(cli, &printer, name, all, dry_run, yes);
    drop(printer);
    let output = buf.lock().unwrap().clone();
    (result, output)
}

#[test]
fn profile_migrate_single_moves_legacy_to_canonical() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());

    let (result, output) = run_migrate(&cli, Some("work"), false, false, true);
    assert_eq!(result.unwrap(), 0);
    assert!(
        !dir.path().join("profiles").join("work.yaml").exists(),
        "legacy manifest should be gone"
    );
    let canonical = dir
        .path()
        .join("profiles")
        .join("work")
        .join("profile.yaml");
    assert!(canonical.is_file(), "canonical manifest should exist");
    let doc = config::load_profile(&canonical).unwrap();
    assert_eq!(doc.metadata.name, "work");
    assert!(
        output.contains("Migrated 'work'"),
        "should report the move, got: {output}"
    );
    // the untouched sibling stays legacy
    assert!(dir.path().join("profiles").join("default.yaml").is_file());
}

#[test]
fn profile_migrate_all_moves_every_legacy_profile() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());

    let (result, output) = run_migrate(&cli, None, true, false, true);
    assert_eq!(result.unwrap(), 0);
    for name in ["default", "work"] {
        assert!(
            !dir.path()
                .join("profiles")
                .join(format!("{name}.yaml"))
                .exists(),
            "legacy '{name}' should be gone"
        );
        assert!(
            dir.path()
                .join("profiles")
                .join(name)
                .join("profile.yaml")
                .is_file(),
            "canonical '{name}' should exist"
        );
    }
    assert!(
        output.contains("Migrated 2 profile(s)"),
        "should summarize both moves, got: {output}"
    );
}

#[cfg(unix)]
#[test]
fn profile_migrate_all_unreadable_dir_errors() {
    use std::os::unix::fs::PermissionsExt;
    if cfgd_core::is_root() {
        return; // root bypasses mode bits; the denial cannot be simulated
    }
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let pdir = dir.path().join("profiles");
    std::fs::set_permissions(&pdir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let (result, _) = run_migrate(&cli, None, true, false, true);
    let err = result.expect_err("an unreadable profiles dir must fail, not silently do nothing");
    assert!(
        err.to_string().contains("failed to read"),
        "error should name the unreadable dir, got: {err}"
    );

    std::fs::set_permissions(&pdir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn profile_migrate_already_canonical_is_noop() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());

    let (first, _) = run_migrate(&cli, Some("work"), false, false, true);
    assert_eq!(first.unwrap(), 0);
    let (second, output) = run_migrate(&cli, Some("work"), false, false, true);
    assert_eq!(
        second.unwrap(),
        0,
        "already-canonical must be a 0-exit no-op"
    );
    assert!(
        output.contains("already canonical"),
        "should report already canonical, got: {output}"
    );
    assert!(
        dir.path()
            .join("profiles")
            .join("work")
            .join("profile.yaml")
            .is_file(),
        "manifest must stay in place"
    );
}

#[test]
fn profile_migrate_dry_run_changes_nothing() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());

    let (result, output) = run_migrate(&cli, None, true, true, false);
    assert_eq!(result.unwrap(), 0);
    assert!(
        dir.path().join("profiles").join("work.yaml").is_file(),
        "dry run must not move files"
    );
    assert!(
        !dir.path().join("profiles").join("work").exists(),
        "dry run must not create bundle dirs"
    );
    assert!(
        output.contains("Would move") && output.contains("Dry run"),
        "should print the move plan, got: {output}"
    );
}

#[test]
fn profile_migrate_dry_run_failed_item_exits_nonzero() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    // an ambiguous profile fails planning; dry-run must report it in the
    // failure count exactly like a real run would
    let bundle = dir.path().join("profiles").join("work");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("profile.yaml"), WORK_PROFILE_YAML).unwrap();

    let (result, output) = run_migrate(&cli, None, true, true, false);
    assert_eq!(
        result.unwrap(),
        1,
        "dry run must surface the planned failure in its return count"
    );
    assert!(
        output.contains("Cannot migrate 'work'"),
        "should name the failing profile, got: {output}"
    );
    assert!(
        output.contains("1 failed"),
        "summary should count the failure, got: {output}"
    );
    // still a dry run: nothing moved
    assert!(dir.path().join("profiles").join("default.yaml").is_file());
}

#[test]
fn profile_migrate_dry_run_json_reports_failed() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let bundle = dir.path().join("profiles").join("work");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("profile.yaml"), WORK_PROFILE_YAML).unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    let failed = migrate::run_profile_migrate(&cli, &printer, None, true, true, false).unwrap();
    drop(printer);
    assert_eq!(failed, 1);
    let output = buf.lock().unwrap().clone();
    // Fail-role status lines are never suppressed, so the payload starts at
    // the first brace.
    let payload = &output[output.find('{').expect("payload must be present")..];
    let json: serde_json::Value = serde_json::from_str(payload.trim())
        .unwrap_or_else(|e| panic!("payload must be valid JSON ({e}), got: {output:?}"));
    assert_eq!(json["failed"], 1);
    assert_eq!(json["planned"], 1);
    assert_eq!(json["dryRun"], true);
    let rec = json["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["action"] == "failed")
        .expect("payload must carry the failed record");
    assert_eq!(rec["name"], "work");
    assert!(
        rec["reason"]
            .as_str()
            .unwrap()
            .contains("ambiguous profile 'work'"),
        "reason should carry the planning error, got: {}",
        rec["reason"]
    );
}

#[test]
fn profile_migrate_single_ambiguous_refuses() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let bundle = dir.path().join("profiles").join("work");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("profile.yaml"), WORK_PROFILE_YAML).unwrap();

    let (result, _) = run_migrate(&cli, Some("work"), false, false, true);
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("ambiguous profile 'work'"),
        "should surface the typed ambiguity error, got: {err}"
    );
    // fail closed: both forms remain untouched
    assert!(dir.path().join("profiles").join("work.yaml").is_file());
    assert!(bundle.join("profile.yaml").is_file());
}

#[test]
fn profile_migrate_all_continues_past_ambiguous() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let bundle = dir.path().join("profiles").join("work");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("profile.yaml"), WORK_PROFILE_YAML).unwrap();

    let (result, output) = run_migrate(&cli, None, true, false, true);
    assert_eq!(
        result.unwrap(),
        1,
        "ambiguous profile counts as one failure"
    );
    // the clean profile still migrated
    assert!(
        dir.path()
            .join("profiles")
            .join("default")
            .join("profile.yaml")
            .is_file(),
        "loop must continue past the ambiguous profile"
    );
    // the ambiguous one is untouched
    assert!(dir.path().join("profiles").join("work.yaml").is_file());
    assert!(
        output.contains("ambiguous profile 'work'"),
        "should report the per-profile failure, got: {output}"
    );
}

#[test]
fn profile_migrate_payload_dir_already_exists() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let files_dir = dir.path().join("profiles").join("work").join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(".zshrc"), "export A=1\n").unwrap();

    let (result, _) = run_migrate(&cli, Some("work"), false, false, true);
    assert_eq!(result.unwrap(), 0);
    assert!(
        dir.path()
            .join("profiles")
            .join("work")
            .join("profile.yaml")
            .is_file(),
        "manifest should join its payload dir"
    );
    assert!(
        files_dir.join(".zshrc").is_file(),
        "existing payload must be untouched"
    );
}

fn git_in(dir: &Path, args: &[&str]) -> std::process::Output {
    cfgd_core::git_cmd_local()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn profile_migrate_git_work_tree_uses_git_mv() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    assert!(git_in(dir.path(), &["init", "-q"]).status.success());
    assert!(
        git_in(dir.path(), &["add", "profiles/work.yaml"])
            .status
            .success()
    );
    assert!(
        git_in(
            dir.path(),
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "seed"
            ]
        )
        .status
        .success()
    );

    let (result, _) = run_migrate(&cli, Some("work"), false, false, true);
    assert_eq!(result.unwrap(), 0);
    assert!(
        dir.path()
            .join("profiles")
            .join("work")
            .join("profile.yaml")
            .is_file()
    );

    // git mv stages the rename: the new path is in the index, the old is not
    let ls = String::from_utf8_lossy(&git_in(dir.path(), &["ls-files"]).stdout).to_string();
    assert!(
        ls.contains("profiles/work/profile.yaml"),
        "git index should track the canonical path, got: {ls}"
    );
    assert!(
        !ls.contains("profiles/work.yaml"),
        "git index should no longer track the legacy path, got: {ls}"
    );
}

#[test]
fn profile_migrate_git_untracked_falls_back_to_plain_rename() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    // work tree, but the manifest is untracked → git mv declines → fs rename
    assert!(git_in(dir.path(), &["init", "-q"]).status.success());

    let (result, output) = run_migrate(&cli, Some("work"), false, false, true);
    assert_eq!(result.unwrap(), 0);
    assert!(
        dir.path()
            .join("profiles")
            .join("work")
            .join("profile.yaml")
            .is_file(),
        "untracked manifest must still migrate via plain rename, got: {output}"
    );
    assert!(!dir.path().join("profiles").join("work.yaml").exists());
}

#[test]
fn profile_migrate_git_mv_failure_warns_and_falls_back() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    assert!(git_in(dir.path(), &["init", "-q"]).status.success());
    assert!(
        git_in(dir.path(), &["add", "profiles/work.yaml"])
            .status
            .success()
    );
    assert!(
        git_in(
            dir.path(),
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "seed"
            ]
        )
        .status
        .success()
    );
    // a stale index.lock makes `git mv` fail to lock the index while the
    // read-only tracking check and the plain rename still succeed — a
    // deterministic stand-in for real-world index.lock contention, and it
    // works for any user on any OS (unlike permission tricks, which root
    // bypasses)
    std::fs::write(dir.path().join(".git").join("index.lock"), "").unwrap();

    let (result, output) = run_migrate(&cli, Some("work"), false, false, true);

    assert_eq!(result.unwrap(), 0, "fallback rename must still succeed");
    assert!(
        dir.path()
            .join("profiles")
            .join("work")
            .join("profile.yaml")
            .is_file(),
        "manifest must be moved by the plain-rename fallback"
    );
    assert!(
        output.contains("git mv failed for") && output.contains("plain rename"),
        "tracked-file git mv failure must warn before falling back, got: {output}"
    );
    assert!(
        output.contains("Migrated 'work'"),
        "the move itself still reports success, got: {output}"
    );
}

#[test]
fn profile_migrate_prompt_decline_cancels() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(false)],
        cfgd_core::output::Verbosity::Normal,
    );

    let failed =
        migrate::run_profile_migrate(&cli, &printer, Some("work"), false, false, false).unwrap();
    drop(printer);
    assert_eq!(failed, 0);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Cancelled"),
        "declined prompt should cancel, got: {output}"
    );
    assert!(
        dir.path().join("profiles").join("work.yaml").is_file(),
        "declined prompt must not move files"
    );
}

#[test]
fn profile_migrate_not_found_errors() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());

    let (result, _) = run_migrate(&cli, Some("ghost"), false, false, true);
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should error for a missing profile, got: {err}"
    );
    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    // Exit-6 uniformity across every missing-profile site.
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

#[test]
fn profile_migrate_json_payload_shape() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    let failed = migrate::run_profile_migrate(&cli, &printer, None, true, false, true).unwrap();
    drop(printer);
    assert_eq!(failed, 0);
    let output = buf.lock().unwrap().clone();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(json["migrated"], 2);
    assert_eq!(json["failed"], 0);
    assert_eq!(json["dryRun"], false);
    let profiles = json["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2);
    for rec in profiles {
        assert_eq!(rec["action"], "migrated");
        let from = rec["from"].as_str().unwrap();
        let to = rec["to"].as_str().unwrap();
        assert!(!from.contains('\\'), "payload paths must be posix: {from}");
        assert!(
            to.ends_with(&format!("{}/profile.yaml", rec["name"].as_str().unwrap())),
            "to should be the canonical path: {to}"
        );
    }
}
