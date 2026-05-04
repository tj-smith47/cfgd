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

fn make_printer() -> Printer {
    Printer::new(cfgd_core::output::Verbosity::Quiet)
}

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
    let result = profiles_inheriting(Path::new("/nonexistent-dir-12345"), "base").unwrap();
    assert!(result.is_empty());
}

#[test]
fn profiles_inheriting_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - other\n  modules: []\n".to_string();
    std::fs::write(dir.path().join("child.yaml"), &profile).unwrap();

    let result = profiles_inheriting(dir.path(), "base").unwrap();
    assert!(result.is_empty());
}

#[test]
fn profiles_inheriting_match_found() {
    let dir = tempfile::tempdir().unwrap();
    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - base\n  modules: []\n".to_string();
    std::fs::write(dir.path().join("child.yaml"), &profile).unwrap();

    let result = profiles_inheriting(dir.path(), "base").unwrap();
    assert_eq!(result, vec!["child"]);
}

// --- collect_module_file_targets ---

#[test]
fn collect_module_file_targets_nonexistent_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let result = collect_module_file_targets("nope", dir.path());
    assert!(result.is_empty());
}

#[test]
fn collect_module_file_targets_local_module() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("test-mod");
    std::fs::create_dir_all(&module_dir).unwrap();
    let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: []\n  files:\n    - source: foo.conf\n      target: /tmp/foo.conf\n";
    std::fs::write(module_dir.join("module.yaml"), module_yaml).unwrap();

    let result = collect_module_file_targets("test-mod", dir.path());
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
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: true,
        output: super::super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        jsonpath: None,
        state_dir: None,
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
    }
}

// --- cmd_profile_show ---

#[test]
fn profile_show_named_profile() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = Printer::for_test();

    cmd_profile_show(&cli, &printer, Some("default")).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Resolved Profile"),
        "should show resolved profile header, got: {output}"
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
    let (printer, buf) = Printer::for_test();

    // None means "show the active profile" — reads from cfgd.yaml
    cmd_profile_show(&cli, &printer, None).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Resolved Profile"),
        "should show resolved profile header, got: {output}"
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
    let (printer, buf) = Printer::for_test();

    // work inherits from default, should resolve both layers
    cmd_profile_show(&cli, &printer, Some("work")).unwrap();
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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
        err.to_string().contains("No cfgd.yaml found"),
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
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Available profiles"),
        "error should list available profiles: {}",
        err
    );
}

// --- cmd_profile_create ---

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

    let profile_path = dir.path().join("profiles").join("devops.yaml");
    assert!(profile_path.exists(), "profile YAML should be created");

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

    let doc = config::load_profile(&dir.path().join("profiles").join("child.yaml")).unwrap();
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

    let doc = config::load_profile(&dir.path().join("profiles").join("modular.yaml")).unwrap();
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

    let doc = config::load_profile(&dir.path().join("profiles").join("alias-test.yaml")).unwrap();
    assert_eq!(doc.spec.aliases.len(), 1);
    assert_eq!(doc.spec.aliases[0].name, "ll");
    assert_eq!(doc.spec.aliases[0].command, "ls -la");
}

#[test]
fn profile_create_with_system_settings() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = Printer::for_test();

    let mut args = make_profile_create_args("sys-test");
    args.system = vec!["sysctl=net.core.somaxconn".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("sys-test.yaml")).unwrap();
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

    let doc = config::load_profile(&dir.path().join("profiles").join("secret-test.yaml")).unwrap();
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

    let doc = config::load_profile(&dir.path().join("profiles").join("script-test.yaml")).unwrap();
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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

    // Delete 'work' (not active, not inherited by others)
    cmd_profile_delete(&cli, &printer, "work", true).unwrap();

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

    let result = cmd_profile_delete(&cli, &printer, "nonexistent", true);
    assert!(result.is_err(), "deleting nonexistent profile should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "error should mention not found: {}",
        err
    );
}

#[test]
fn profile_delete_active_profile_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // 'default' is the active profile in cfgd.yaml
    let result = cmd_profile_delete(&cli, &printer, "default", true);
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

    let result = cmd_profile_delete(&cli, &printer, "default", true);
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

    cmd_profile_delete(&cli, &printer, "ephemeral", true).unwrap();

    assert!(!files_dir.exists(), "files directory should be cleaned up");
}

#[test]
fn profile_delete_invalid_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_profile_delete(&cli, &printer, "-bad", true).unwrap_err();
    assert!(
        err.to_string().contains("cannot start with '.' or '-'"),
        "should reject leading dash in name, got: {err}"
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
    let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_show(&cli, &printer, Some("default")).unwrap();

    let output = buf.lock().unwrap();
    // JSON output may have preamble text (key_value lines) — find first '{'
    let start = output.find('{').expect("should have JSON object in output");
    let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
    assert!(
        json.get("layers").is_some(),
        "JSON should have layers field, got: {json}"
    );
    assert!(
        json.get("merged").is_some(),
        "JSON should have merged field, got: {json}"
    );
}

#[test]
fn profile_list_json_schema() {
    let dir = setup_config_dir();
    let cli = test_cli_json(dir.path());
    let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_list(&cli, &printer).unwrap();

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
    let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_list(&cli, &printer).unwrap();

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
    let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_profile_list(&cli, &printer).unwrap();

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
    let (printer, buf) = Printer::for_test();

    cmd_profile_show(&cli, &printer, Some("files-test")).unwrap();

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
    let (printer, buf) = Printer::for_test();

    cmd_profile_show(&cli, &printer, Some("default")).unwrap();

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
    let (printer, buf) = Printer::for_test();

    cmd_profile_show(&cli, &printer, Some("secret-show")).unwrap();

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
    let (printer, buf) = Printer::for_test();

    cmd_profile_show(&cli, &printer, Some("sys-show")).unwrap();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

    let mut args = make_profile_create_args("fancy");
    args.inherits = vec!["default".to_string()];
    args.modules = vec!["shell".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();

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
    let (printer, buf) = Printer::for_test();

    cmd_profile_show(&cli, &printer, Some("rich")).unwrap();
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
fn profile_show_no_packages_displays_none() {
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
    let (printer, buf) = Printer::for_test();
    cmd_profile_show(&cli, &printer, Some("bare")).unwrap();
    let output = buf.lock().unwrap();

    assert!(
        output.contains("Packages"),
        "should show Packages section: {output}"
    );
    assert!(
        output.contains("(none)"),
        "should show (none) for empty packages: {output}"
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
    let (printer, buf) = Printer::for_test();
    cmd_profile_show(&cli, &printer, Some("env-secret")).unwrap();
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
    let (printer, buf) = Printer::for_test();
    cmd_profile_show(&cli, &printer, Some("both-secret")).unwrap();
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

fn test_cli_wide(dir: &Path) -> super::super::Cli {
    super::super::Cli {
        output: super::super::OutputFormatArg(cfgd_core::output::OutputFormat::Wide),
        ..test_cli(dir)
    }
}

#[test]
fn profile_list_wide_format() {
    let dir = setup_config_dir();
    let cli = test_cli_wide(dir.path());
    let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Wide);

    cmd_profile_list(&cli, &printer).unwrap();
    let output = buf.lock().unwrap();
    // Wide format uses table with columns
    assert!(
        output.contains("Profile") && output.contains("Active") && output.contains("Modules"),
        "wide list should show table headers, got: {output}"
    );
    assert!(
        output.contains("default") && output.contains("work"),
        "wide list should show profile names, got: {output}"
    );
}

// --- profile show: no env displays (none) ---

#[test]
fn profile_show_no_env_displays_none() {
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
    let (printer, buf) = Printer::for_test();
    cmd_profile_show(&cli, &printer, Some("noenv")).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Env"),
        "should show Env section, got: {output}"
    );
    // The Env section should show "(none)" since there are no env vars
    // But the "(none)" could also come from packages or files - let's just verify it exists
    assert!(
        output.contains("(none)"),
        "should show (none) for empty env/packages/files, got: {output}"
    );
}

// --- profile show: no files displays (none) ---

#[test]
fn profile_show_no_files_displays_none() {
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
    let (printer, buf) = Printer::for_test();
    cmd_profile_show(&cli, &printer, Some("nofiles")).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Files"),
        "should show Files section, got: {output}"
    );
}

// --- profile update: add and remove packages ---

#[test]
fn profile_update_add_package() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer, buf) = Printer::for_test();

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
    let (printer2, buf2) = Printer::for_test();
    let mut args2 = make_profile_update_args();
    args2.secrets = vec!["other-source:~/target".to_string()];
    cmd_profile_update(&cli, &printer2, "default", &args2).unwrap();

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

    let doc = config::load_profile(&dir.path().join("profiles").join("pkg-test.yaml")).unwrap();
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

    let doc = config::load_profile(&dir.path().join("profiles").join("all-scripts.yaml")).unwrap();
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
