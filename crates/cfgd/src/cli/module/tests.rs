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
    let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
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
        jsonpath: None,
        state_dir: None,
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
        jsonpath: None,
        state_dir: None,
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_list(&cli, &printer).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_list(&cli, &printer).unwrap();

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

    let output = buf.lock().unwrap();
    // JSON may have preamble text from load_config_and_profile — find first '['
    let start = output.find('[').expect("should have JSON array in output");
    let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_show(&cli, &printer, "devtools", false).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    let err = cmd_module_show(&cli, &printer, "missing", false).unwrap_err();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("existing"),
        "should hint available modules, got: {output}"
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    // Without --show-values, env values should be masked
    cmd_module_show(&cli, &printer, "secrets-mod", false).unwrap();
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_show(&cli, &printer, "env-mod", true).unwrap();
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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    let args = make_module_update_args("ghost");
    let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
}

// ─── cmd_module_create — non-interactive flags ─────────────

#[test]
fn cmd_module_create_with_env_and_aliases() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    let args = super::ModuleCreateArgs {
        description: Some("Test module".to_string()),
        env: vec!["EDITOR=nvim".to_string()],
        aliases: vec!["ll=ls -la".to_string()],
        ..make_module_create_args("env-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    let err = cmd_module_delete(&cli, &printer, "ghost", true, false).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "should report not found, got: {err}"
    );
}

#[test]
fn cmd_module_delete_invalid_name_fails() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    let err = cmd_module_delete(&cli, &printer, "-bad", true, false).unwrap_err();
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/team/modules.git",
        Some("team"),
    )
    .unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/team/modules.git",
        Some("team"),
    )
    .unwrap();

    // Second add should be a no-op
    let (printer2, buf2) = cfgd_core::output::Printer::for_test();
    cmd_module_registry_add(
        &cli,
        &printer2,
        "https://github.com/team/other.git",
        Some("team"),
    )
    .unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    // Add first
    cmd_module_registry_add(
        &cli,
        &printer,
        "https://example.com/reg.git",
        Some("myrepo"),
    )
    .unwrap();

    // Remove
    let (printer2, buf2) = cfgd_core::output::Printer::for_test();
    cmd_module_registry_remove(&cli, &printer2, "myrepo").unwrap();

    let output = buf2.lock().unwrap();
    assert!(
        output.contains("Removed module registry 'myrepo'"),
        "should confirm removal, got: {output}"
    );
}

#[test]
fn cmd_module_registry_remove_not_found() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_remove(&cli, &printer, "nonexistent").unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No module registries") || output.contains("not found"),
        "should report not found or no registries, got: {output}"
    );
}

// ─── cmd_module_registry_rename ─────────────────────────────

#[test]
fn cmd_module_registry_rename_success() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_add(
        &cli,
        &printer,
        "https://example.com/reg.git",
        Some("old-name"),
    )
    .unwrap();

    let (printer2, buf2) = cfgd_core::output::Printer::for_test();
    cmd_module_registry_rename(&cli, &printer2, "old-name", "new-name").unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_add(&cli, &printer, "https://example.com/a.git", Some("alpha")).unwrap();
    cmd_module_registry_add(&cli, &printer, "https://example.com/b.git", Some("beta")).unwrap();

    let (printer2, _buf2) = cfgd_core::output::Printer::for_test();
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_list(&cli, &printer).unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_add(&cli, &printer, "https://example.com/a.git", Some("alpha")).unwrap();
    cmd_module_registry_add(&cli, &printer, "https://example.com/b.git", Some("beta")).unwrap();

    let (printer2, buf2) = cfgd_core::output::Printer::for_test();
    cmd_module_registry_list(&cli, &printer2).unwrap();

    let output = buf2.lock().unwrap();
    assert!(output.contains("alpha"), "should list alpha, got: {output}");
    assert!(output.contains("beta"), "should list beta, got: {output}");
}

#[test]
fn cmd_module_registry_list_json() {
    let dir = setup_config_dir();
    let cli = test_cli(dir.path());
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_add(&cli, &printer, "https://example.com/r.git", Some("team")).unwrap();

    let cli_json = test_cli_json(dir.path());
    let (printer2, buf2) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_module_registry_list(&cli_json, &printer2).unwrap();

    let output = buf2.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_registry_list(&cli, &printer).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No config found"),
        "should report no config, got: {output}"
    );
}

// ─── cmd_module_keys_list ───────────────────────────────────

#[test]
fn cmd_module_keys_list_no_keys() {
    let (printer, buf) = cfgd_core::output::Printer::for_test();
    cmd_module_keys_list(&printer).unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_search(&cli, &printer, "test").unwrap();

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
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_search(&cli, &printer, "test").unwrap();

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

// ─── cmd_module_keys_generate — no cosign ───────────────────

#[test]
fn cmd_module_keys_generate_no_cosign_fails() {
    // In test environment, cosign is very unlikely to be available
    if cfgd_core::command_available("cosign") {
        return; // skip if cosign is actually installed
    }
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_show(&cli, &printer, "locked-mod", false).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_show(&cli, &printer, "alias-mod", false).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_show(&cli, &printer, "script-mod", false).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_show(&cli, &printer, "git-file-mod", false).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    let args = super::ModuleCreateArgs {
        packages: vec!["ripgrep".to_string(), "fd-find".to_string()],
        sets: vec![
            "package.ripgrep.minVersion=13.0".to_string(),
            "package.fd-find.platforms=linux,macos".to_string(),
        ],
        ..make_module_create_args("pkg-set-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    let args = super::ModuleCreateArgs {
        description: Some("test module".to_string()),
        ..make_module_create_args("dup-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

    // Second create with same name should fail
    let (printer2, _buf2) = cfgd_core::output::Printer::for_test();
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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_delete(&cli, &printer, "to-delete", true, false).unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    let err = cmd_module_delete(&cli, &printer, "referenced", true, false).unwrap_err();
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_delete(&cli, &printer, "purge-mod", true, true).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_delete(&cli, &printer, "restore-mod", true, false).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_delete(&cli, &printer, "lock-mod", true, false).unwrap();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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

    let output = buf.lock().unwrap();
    let start = output.find('[').expect("should have JSON array");
    let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2);

    let active = arr.iter().find(|e| e["name"] == "active-mod").unwrap();
    assert_eq!(active["active"], true);
    assert_eq!(active["status"], "pending");

    let inactive = arr.iter().find(|e| e["name"] == "inactive-mod").unwrap();
    assert_eq!(inactive["active"], false);
    assert_eq!(inactive["status"], "available");
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    cmd_module_list(&cli, &printer).unwrap();

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

    let output = buf.lock().unwrap();
    let start = output.find('[').expect("should have JSON array");
    let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
    let arr = json.as_array().unwrap();
    let entry = &arr[0];
    assert_eq!(entry["source"], "remote");
}

// ─── cmd_module_registry_remove — no config fails ──────────────

#[test]
fn cmd_module_registry_remove_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    let err = cmd_module_registry_remove(&cli, &printer, "test").unwrap_err();
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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

    // Add a registry
    cmd_module_registry_add(&cli, &printer, "https://example.com/reg.git", Some("team")).unwrap();

    // Add a profile that references team/somemod
    let profile_path = dir.path().join("profiles/default.yaml");
    std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - team/somemod\n",
        ).unwrap();

    let (printer2, buf2) = cfgd_core::output::Printer::for_test();
    cmd_module_registry_remove(&cli, &printer2, "team").unwrap();

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
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_registry_list(&cli, &printer).unwrap();

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

// ─── cmd_module_registry_list — JSON no config ──────────────────

#[test]
fn cmd_module_registry_list_json_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli_json(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_module_registry_list(&cli, &printer).unwrap();

    let output = buf.lock().unwrap();
    let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    // Remove with manager prefix: -brew:ripgrep should strip prefix and remove "ripgrep"
    let args = super::ModuleUpdateArgs {
        packages: vec!["-brew:ripgrep".to_string()],
        ..make_module_update_args("mod1")
    };
    cmd_module_update_local(&cli, &printer, &args).unwrap();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

    let args = super::ModuleCreateArgs {
        description: Some("My awesome module".to_string()),
        depends: vec!["base".to_string(), "core".to_string()],
        packages: vec!["curl".to_string()],
        ..make_module_create_args("desc-mod")
    };
    cmd_module_create(&cli, &printer, &args).unwrap();

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

// ─── cmd_module_registry_rename — no config fails ───────────────

#[test]
fn cmd_module_registry_rename_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
    let (printer, buf) = cfgd_core::output::Printer::for_test();

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
fn cmd_module_keys_rotate_no_cosign_fails() {
    if cfgd_core::command_available("cosign") {
        return;
    }
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
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
    let (printer, _buf) = cfgd_core::output::Printer::for_test();

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
