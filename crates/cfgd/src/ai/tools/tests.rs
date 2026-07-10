use super::*;

#[test]
fn test_dispatch_unknown_tool() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let result = dispatch_tool_call(
        "nonexistent",
        &Value::Null,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("Unknown tool"));
}

#[test]
fn test_dispatch_get_schema() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"kind": "Module"});
    let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("apiVersion"));
}

#[test]
fn test_dispatch_get_schema_profile() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"kind": "Profile"});
    let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("kind: Profile"));
}

#[test]
fn test_dispatch_get_schema_config() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"kind": "Config"});
    let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("kind: Config"));
}

#[test]
fn test_dispatch_get_schema_invalid_kind() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"kind": "InvalidKind"});
    let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("unknown schema kind"));
}

#[test]
fn test_dispatch_get_schema_missing_kind() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'kind' parameter is required"));
}

#[test]
fn test_dispatch_validate_yaml_valid() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test\nspec: {}\n";
    let input = serde_json::json!({"content": yaml, "kind": "Module"});
    let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("true"));
}

#[test]
fn test_dispatch_validate_yaml_invalid() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"content": "not valid yaml {{", "kind": "Module"});
    let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
    assert!(!result.is_error); // validate_yaml itself returns a result struct, not an error
    assert!(result.content.contains("false"));
}

#[test]
fn test_dispatch_validate_yaml_missing_content() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"kind": "Module"});
    let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'content' parameter is required"));
}

#[test]
fn test_dispatch_scan_dotfiles() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".zshrc"), "# config").unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({});
    let result = dispatch_tool_call("scan_dotfiles", &input, &mut session, tmp.path(), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains(".zshrc"));
}

#[test]
fn test_dispatch_scan_dotfiles_with_home_override() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".bashrc"), "# bash").unwrap();
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"home": tmp.path().to_str().unwrap()});
    let result = dispatch_tool_call(
        "scan_dotfiles",
        &input,
        &mut session,
        Path::new("/nonexistent"),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains(".bashrc"));
}

#[test]
fn test_dispatch_scan_shell_config() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(".zshrc"),
        "alias ll='ls -la'\nexport EDITOR=nvim\n",
    )
    .unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"shell": "zsh"});
    let result = dispatch_tool_call("scan_shell_config", &input, &mut session, tmp.path(), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("ll"));
    assert!(result.content.contains("EDITOR"));
}

#[test]
fn test_dispatch_scan_shell_config_missing_shell() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call(
        "scan_shell_config",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("'shell' parameter is required"));
}

#[test]
fn test_dispatch_scan_system_settings() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call(
        "scan_system_settings",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(!result.is_error);
}

#[test]
fn test_dispatch_detect_platform() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call("detect_platform", &input, &mut session, Path::new("/"), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("os"));
    assert!(result.content.contains("arch"));
}

#[test]
fn test_dispatch_inspect_tool_missing_name() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call("inspect_tool", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'name' parameter is required"));
}

#[test]
fn test_dispatch_inspect_tool() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".zshrc"), "# zsh config").unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"name": "zsh"});
    let result = dispatch_tool_call("inspect_tool", &input, &mut session, tmp.path(), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("zsh"));
}

#[test]
fn test_dispatch_query_package_manager_missing_manager() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"package": "neovim"});
    let result = dispatch_tool_call(
        "query_package_manager",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("'manager' parameter is required"));
}

#[test]
fn test_dispatch_query_package_manager_missing_package() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"manager": "brew"});
    let result = dispatch_tool_call(
        "query_package_manager",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("'package' parameter is required"));
}

#[test]
fn test_dispatch_query_package_manager_not_found() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"manager": "nonexistent", "package": "vim"});
    let result = dispatch_tool_call(
        "query_package_manager",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("not found"));
}

#[test]
fn test_dispatch_read_file_missing_path() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call("read_file", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'path' parameter is required"));
}

#[test]
fn test_dispatch_list_directory_missing_path() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call("list_directory", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'path' parameter is required"));
}

#[test]
fn test_dispatch_read_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "hello world").unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": file.to_str().unwrap()});
    let result = dispatch_tool_call("read_file", &input, &mut session, tmp.path(), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("hello world"));
}

#[test]
fn test_dispatch_list_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "").unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": tmp.path().to_str().unwrap()});
    let result = dispatch_tool_call("list_directory", &input, &mut session, tmp.path(), &[]);
    assert!(!result.is_error);
    assert!(result.content.contains("a.txt"));
    assert!(result.content.contains("b.txt"));
}

#[test]
fn test_dispatch_adopt_files() {
    let src_dir = tempfile::TempDir::new().unwrap();
    let repo_dir = tempfile::TempDir::new().unwrap();
    let src_file = src_dir.path().join("config.toml");
    std::fs::write(&src_file, "key = 'val'").unwrap();

    let mut session = GenerateSession::new(repo_dir.path().to_path_buf());
    let input = serde_json::json!({
        "files": [
            {"source": src_file.to_str().unwrap(), "dest": "tool/config.toml"}
        ]
    });
    let result = dispatch_tool_call("adopt_files", &input, &mut session, src_dir.path(), &[]);
    assert!(!result.is_error);
    assert!(repo_dir.path().join("tool/config.toml").exists());
}

#[test]
fn test_dispatch_adopt_files_missing_files() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call("adopt_files", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'files' parameter is required"));
}

#[test]
fn test_dispatch_adopt_files_missing_source() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"files": [{"dest": "out.txt"}]});
    let result = dispatch_tool_call("adopt_files", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'source'"));
}

#[test]
fn test_dispatch_adopt_files_missing_dest() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"files": [{"source": "/tmp/x"}]});
    let result = dispatch_tool_call("adopt_files", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(result.content.contains("'dest'"));
}

#[test]
fn test_dispatch_write_module_yaml() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test\nspec:\n  packages:\n    - name: test-pkg\n";
    let input = serde_json::json!({"name": "test", "content": yaml});
    let result = dispatch_tool_call("write_module_yaml", &input, &mut session, tmp.path(), &[]);
    assert!(!result.is_error, "Error: {}", result.content);
    assert!(result.content.contains("path"));
    assert!(tmp.path().join("modules/test/module.yaml").exists());
}

#[test]
fn test_dispatch_write_module_yaml_invalid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"name": "bad", "content": "invalid yaml {{"});
    let result = dispatch_tool_call("write_module_yaml", &input, &mut session, tmp.path(), &[]);
    assert!(result.is_error);
}

#[test]
fn test_dispatch_write_module_yaml_missing_name() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"content": "test"});
    let result = dispatch_tool_call(
        "write_module_yaml",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("'name' parameter is required"));
}

#[test]
fn test_dispatch_write_profile_yaml() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - test\n";
    let input = serde_json::json!({"name": "base", "content": yaml});
    let result = dispatch_tool_call("write_profile_yaml", &input, &mut session, tmp.path(), &[]);
    assert!(!result.is_error, "Error: {}", result.content);
    assert!(result.content.contains("path"));
    assert!(tmp.path().join("profiles/base/profile.yaml").exists());
}

#[test]
fn test_dispatch_write_profile_yaml_missing_content() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"name": "test"});
    let result = dispatch_tool_call(
        "write_profile_yaml",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("'content' parameter is required"));
}

#[test]
fn test_dispatch_list_generated_empty() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let result = dispatch_tool_call(
        "list_generated",
        &Value::Null,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(!result.is_error);
    assert_eq!(result.content, "[]");
}

#[test]
fn test_dispatch_list_generated_after_write() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec:\n  packages:\n    - name: neovim\n";
    session.write_module_yaml("nvim", yaml).unwrap();
    let result = dispatch_tool_call(
        "list_generated",
        &Value::Null,
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("nvim"));
    assert!(result.content.contains("Module"));
}

#[test]
fn test_dispatch_get_existing_modules() {
    let tmp = tempfile::TempDir::new().unwrap();
    let nvim_dir = tmp.path().join("modules").join("nvim");
    std::fs::create_dir_all(&nvim_dir).unwrap();
    std::fs::write(nvim_dir.join("module.yaml"), "test").unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let result = dispatch_tool_call(
        "get_existing_modules",
        &Value::Null,
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("nvim"));
}

#[test]
fn test_dispatch_get_existing_modules_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let result = dispatch_tool_call(
        "get_existing_modules",
        &Value::Null,
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert_eq!(result.content, "[]");
}

#[test]
fn test_dispatch_get_existing_profiles() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("base.yaml"), "test").unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let result = dispatch_tool_call(
        "get_existing_profiles",
        &Value::Null,
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("base"));
}

#[test]
fn test_dispatch_get_existing_profiles_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let result = dispatch_tool_call(
        "get_existing_profiles",
        &Value::Null,
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert_eq!(result.content, "[]");
}

#[test]
fn test_tool_definitions_not_empty() {
    let defs = tool_definitions();
    assert!(!defs.is_empty());
    for def in &defs {
        assert!(!def.name.is_empty());
        assert!(!def.description.is_empty());
    }
}

#[test]
fn test_tool_definitions_all_have_object_schema() {
    let defs = tool_definitions();
    for def in &defs {
        assert_eq!(
            def.input_schema["type"], "object",
            "tool '{}' input_schema must have type: object",
            def.name
        );
    }
}

#[test]
fn test_tool_definitions_present_yaml_included() {
    let defs = tool_definitions();
    assert!(
        defs.iter().any(|d| d.name == "present_yaml"),
        "present_yaml should be in tool definitions even though it's handled specially"
    );
}

#[test]
fn test_tool_definitions_count() {
    let defs = tool_definitions();
    // 17 dispatch tools + present_yaml = 18
    assert_eq!(defs.len(), 18, "expected 18 tool definitions");
}

#[test]
fn test_dispatch_scan_installed_packages_empty() {
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({});
    let result = dispatch_tool_call(
        "scan_installed_packages",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(!result.is_error);
    assert_eq!(result.content, "[]");
}

// ---------------------------------------------------------------------------
// Pipeline integration tests — sequential tool call flows
// ---------------------------------------------------------------------------

#[test]
fn test_generate_tool_pipeline_writes_module() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());

    // AI calls get_schema to learn Module format
    let result = dispatch_tool_call(
        "get_schema",
        &serde_json::json!({"kind": "Module"}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("apiVersion"));

    // AI calls validate_yaml to check its generated YAML
    let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git\nspec:\n  packages:\n    - name: git\n";
    let result = dispatch_tool_call(
        "validate_yaml",
        &serde_json::json!({"content": module_yaml, "kind": "Module"}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("true"));

    // AI calls write_module_yaml
    let result = dispatch_tool_call(
        "write_module_yaml",
        &serde_json::json!({"name": "git", "content": module_yaml}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);

    // Verify file was written
    let module_path = tmp.path().join("modules/git/module.yaml");
    assert!(module_path.exists());
    assert_eq!(std::fs::read_to_string(&module_path).unwrap(), module_yaml);

    // AI calls list_generated to see what it wrote
    let result = dispatch_tool_call(
        "list_generated",
        &serde_json::json!({}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("git"));
}

#[test]
fn test_generate_tool_pipeline_writes_profile() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());

    let profile_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - git\n";
    let result = dispatch_tool_call(
        "write_profile_yaml",
        &serde_json::json!({"name": "base", "content": profile_yaml}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);

    let profile_path = tmp.path().join("profiles/base/profile.yaml");
    assert!(profile_path.exists());
}

#[test]
fn test_generate_scan_dotfiles_via_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".gitconfig"), "[user]\nname = Test").unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());

    let result = dispatch_tool_call(
        "scan_dotfiles",
        &serde_json::json!({}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains(".gitconfig"));
}

#[test]
fn test_generate_unknown_tool_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());

    let result = dispatch_tool_call(
        "nonexistent_tool",
        &serde_json::json!({}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(result.is_error);
    assert!(result.content.contains("Unknown tool"));
}

#[test]
fn test_generate_pipeline_module_then_profile() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());

    // Write a module
    let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git\nspec:\n  packages:\n    - name: git\n";
    let result = dispatch_tool_call(
        "write_module_yaml",
        &serde_json::json!({"name": "git", "content": module_yaml}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);

    // Write a profile that references the module
    let profile_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - git\n";
    let result = dispatch_tool_call(
        "write_profile_yaml",
        &serde_json::json!({"name": "base", "content": profile_yaml}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);

    // list_generated shows both
    let result = dispatch_tool_call(
        "list_generated",
        &serde_json::json!({}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("git"));
    assert!(result.content.contains("base"));
    assert!(result.content.contains("Module"));
    assert!(result.content.contains("Profile"));

    // get_existing_modules picks up the written module
    let result = dispatch_tool_call(
        "get_existing_modules",
        &serde_json::json!({}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("git"));

    // get_existing_profiles picks up the written profile
    let result = dispatch_tool_call(
        "get_existing_profiles",
        &serde_json::json!({}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert!(result.content.contains("base"));
}

#[test]
fn test_generate_pipeline_invalid_yaml_does_not_write() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());

    // validate_yaml reports invalid
    let bad_yaml = "not: valid: yaml: {{";
    let result = dispatch_tool_call(
        "validate_yaml",
        &serde_json::json!({"content": bad_yaml, "kind": "Module"}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error); // validate_yaml never errors; returns a result struct
    assert!(result.content.contains("false"));

    // write_module_yaml rejects the same invalid YAML
    let result = dispatch_tool_call(
        "write_module_yaml",
        &serde_json::json!({"name": "bad", "content": bad_yaml}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(result.is_error);

    // Nothing was written
    assert!(!tmp.path().join("modules/bad/module.yaml").exists());

    // list_generated is empty
    let result = dispatch_tool_call(
        "list_generated",
        &serde_json::json!({}),
        &mut session,
        tmp.path(),
        &[],
    );
    assert!(!result.is_error);
    assert_eq!(result.content, "[]");
}

// ---------------------------------------------------------------------------
// Err-arm coverage — each dispatcher with an underlying call that returns Err
// must surface "Error: ..." with is_error=true. These pin the dispatch contract
// so a future refactor doesn't accidentally promote a failure into a success.
// ---------------------------------------------------------------------------

#[test]
fn test_dispatch_read_file_underlying_error_surfaces() {
    // dispatch_read_file Err arm (lines 490-493): path within home but does
    // NOT exist on disk → files::read_file → fs::metadata Err →
    // CfgdError::Generate(FileAccessDenied) → "Error: ..." is_error=true.
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("not-on-disk.txt");
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": missing.to_str().unwrap()});
    let result = dispatch_tool_call("read_file", &input, &mut session, tmp.path(), &[]);
    assert!(result.is_error);
    assert!(
        result.content.starts_with("Error: "),
        "expected leading 'Error: ', got: {}",
        result.content
    );
}

#[test]
fn test_dispatch_list_directory_underlying_error_surfaces() {
    // dispatch_list_directory Err arm (lines 512-515): path within home but
    // does NOT exist on disk → files::list_directory → fs::read_dir Err →
    // "Error: ..." is_error=true.
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("no-such-dir");
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": missing.to_str().unwrap()});
    let result = dispatch_tool_call("list_directory", &input, &mut session, tmp.path(), &[]);
    assert!(result.is_error);
    assert!(
        result.content.starts_with("Error: "),
        "expected leading 'Error: ', got: {}",
        result.content
    );
}

#[test]
fn test_dispatch_adopt_files_underlying_error_surfaces() {
    // dispatch_adopt_files Err arm (lines 561-564): all params present, but
    // source path doesn't exist on disk → files::adopt_files → fs::read Err →
    // "Error: ..." is_error=true. Pins the failure path of the per-pair copy
    // loop; the Ok arm is covered by test_dispatch_adopt_files.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({
        "files": [
            {"source": "/tmp/__cfgd_test_does_not_exist__", "dest": "x.txt"}
        ]
    });
    let result = dispatch_tool_call("adopt_files", &input, &mut session, tmp.path(), &[]);
    assert!(result.is_error);
    assert!(
        result.content.starts_with("Error: "),
        "expected leading 'Error: ', got: {}",
        result.content
    );
}

#[test]
fn test_dispatch_validate_yaml_missing_kind() {
    // dispatch_validate_yaml second-param-check Err arm (lines 607-610):
    // content is present but 'kind' is not → "Error: 'kind' parameter is required".
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"content": "apiVersion: x"});
    let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(
        result.content.contains("'kind' parameter is required"),
        "expected missing-kind error, got: {}",
        result.content
    );
}

#[test]
fn test_dispatch_validate_yaml_invalid_kind_value() {
    // dispatch_validate_yaml kind-parse Err arm (lines 615-619): 'kind' is
    // present but is not "Module" / "Profile" / "Config" → SchemaKind parse
    // Err → "Error: unknown schema kind" surfaced verbatim.
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"content": "anything", "kind": "NotARealKind"});
    let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
    assert!(result.is_error);
    assert!(
        result.content.contains("unknown schema kind"),
        "expected unknown-kind error, got: {}",
        result.content
    );
}

#[test]
fn test_dispatch_write_module_yaml_missing_content() {
    // dispatch_write_module_yaml second-param-check Err arm (lines 642-645):
    // name is present but content is not → "Error: 'content' parameter is required".
    // Complements the existing _missing_name test.
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"name": "test"});
    let result = dispatch_tool_call(
        "write_module_yaml",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(
        result.content.contains("'content' parameter is required"),
        "expected missing-content error, got: {}",
        result.content
    );
}

#[test]
fn test_dispatch_write_profile_yaml_missing_name() {
    // dispatch_write_profile_yaml first-param-check Err arm (lines 664-667):
    // 'name' is missing → "Error: 'name' parameter is required". Complements
    // the existing _missing_content test.
    let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
    let input = serde_json::json!({"content": "yaml goes here"});
    let result = dispatch_tool_call(
        "write_profile_yaml",
        &input,
        &mut session,
        Path::new("/"),
        &[],
    );
    assert!(result.is_error);
    assert!(
        result.content.contains("'name' parameter is required"),
        "expected missing-name error, got: {}",
        result.content
    );
}

#[test]
fn test_dispatch_write_profile_yaml_underlying_error_surfaces() {
    // dispatch_write_profile_yaml Err arm (lines 684-687): both params present
    // but session.write_profile_yaml errors (invalid YAML body) → "Error: ..."
    // is_error=true. Mirrors test_dispatch_write_module_yaml_invalid for the
    // profile side, which had no equivalent test.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"name": "bad", "content": "not yaml {{ unclosed"});
    let result = dispatch_tool_call("write_profile_yaml", &input, &mut session, tmp.path(), &[]);
    assert!(result.is_error);
    assert!(
        result.content.starts_with("Error: "),
        "expected leading 'Error: ', got: {}",
        result.content
    );
}
