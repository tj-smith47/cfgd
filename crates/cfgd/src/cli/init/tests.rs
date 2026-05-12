use super::*;

// ─────────────────────────────────────────────────────
// ensure_config_file — used by profile switch and tests
// ─────────────────────────────────────────────────────

fn ensure_config_file(
    config_dir: &Path,
    config_path: &Path,
    profile_name: &str,
    from_url: Option<&str>,
    branch: &str,
    theme: Option<&str>,
) -> anyhow::Result<()> {
    if config_path.exists() {
        let contents = std::fs::read_to_string(config_path)?;
        let mut cfg = config::parse_config(&contents, config_path)?;
        if cfg.spec.profile.as_deref() != Some(profile_name) {
            cfg.spec.profile = Some(profile_name.to_string());
            let yaml = serde_yaml::to_string(&cfg)?;
            cfgd_core::atomic_write_str(config_path, &yaml)?;
        }
        return Ok(());
    }

    let name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-config");

    let origin_section = if let Some(url) = from_url {
        format!(
            r#"  origin:
    type: Git
    url: {}
    branch: {}
"#,
            url, branch
        )
    } else {
        String::new()
    };

    let theme_value = theme.unwrap_or("default");

    let config_content = format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: {}
spec:
  profile: {}
  theme: {}
  fileStrategy: Symlink
{}"#,
        name, profile_name, theme_value, origin_section
    );

    cfgd_core::atomic_write_str(config_path, &config_content)?;
    Ok(())
}

// ─────────────────────────────────────────────────────
// count_packages — used by profile display
// ─────────────────────────────────────────────────────

fn count_packages(spec: &config::ProfileSpec) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = spec.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
        count += pkgs.pipx.len();
        count += pkgs.dnf.len();
    }
    count
}

#[test]
fn scaffold_creates_structure() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("test-config"), None, &printer).unwrap();

    assert!(dir.path().join("cfgd.yaml").exists());
    assert!(dir.path().join("profiles").is_dir());
    assert!(dir.path().join("modules").is_dir());
    assert!(!dir.path().join("files").exists());
    assert!(dir.path().join(".gitignore").exists());

    let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    assert!(contents.contains("name: test-config"));
}

#[test]
fn scaffold_with_theme() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("themed"), Some("minimal"), &printer).unwrap();

    let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    assert!(contents.contains("name: themed"));
    assert!(contents.contains("theme: minimal"));
}

#[test]
fn scaffold_uses_dir_name_as_default() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), None, None, &printer).unwrap();

    let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    // Should use the tempdir name
    assert!(contents.contains("name:"));
}

#[test]
fn ensure_config_file_creates_new() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    ensure_config_file(
        dir.path(),
        &config_path,
        "work",
        Some("https://github.com/test/init-cfg.git"),
        "master",
        None,
    )
    .unwrap();

    assert!(config_path.exists());
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("profile: work"));
    assert!(contents.contains("https://github.com/test/init-cfg.git"));
}

#[test]
fn ensure_config_file_updates_existing() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
    )
    .unwrap();

    ensure_config_file(dir.path(), &config_path, "work", None, "master", None).unwrap();

    let cfg = config::load_config(&config_path).unwrap();
    assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
}

#[test]
fn ensure_config_file_no_update_if_same_profile() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    let original = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n";
    std::fs::write(&config_path, original).unwrap();

    ensure_config_file(dir.path(), &config_path, "default", None, "master", None).unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(contents, original);
}

#[test]
fn ensure_config_file_with_branch_and_theme() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    ensure_config_file(
        dir.path(),
        &config_path,
        "work",
        Some("https://github.com/test/cfg.git"),
        "develop",
        Some("minimal"),
    )
    .unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("branch: develop"));
    assert!(contents.contains("theme: minimal"));
}

#[test]
fn count_packages_empty() {
    let spec = config::ProfileSpec::default();
    assert_eq!(count_packages(&spec), 0);
}

#[test]
fn count_packages_with_various() {
    let spec = config::ProfileSpec {
        packages: Some(config::PackagesSpec {
            brew: Some(config::BrewSpec {
                formulae: vec!["rg".into(), "fd".into()],
                casks: vec!["firefox".into()],
                ..Default::default()
            }),
            cargo: Some(config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(count_packages(&spec), 4);
}

#[test]
fn profile_switch_via_config_update() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("work.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  env: []\n",
    )
    .unwrap();

    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
    )
    .unwrap();

    ensure_config_file(dir.path(), &config_path, "work", None, "master", None).unwrap();

    let cfg = config::load_config(&config_path).unwrap();
    assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
}

#[test]
fn regenerate_workflow_empty_repo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    regenerate_workflow(dir.path(), &printer).unwrap();

    // No profiles or modules → no workflow generated
    assert!(
        !dir.path()
            .join(".github/workflows/cfgd-release.yml")
            .exists()
    );
}

#[test]
fn regenerate_workflow_with_profile() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();
    std::fs::write(
        profiles_dir.join("base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    regenerate_workflow(dir.path(), &printer).unwrap();

    let workflow = dir.path().join(".github/workflows/cfgd-release.yml");
    assert!(workflow.exists());
    let contents = std::fs::read_to_string(&workflow).unwrap();
    assert!(contents.contains("base"));
}

#[test]
fn scaffold_includes_default_theme() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("test"), None, &printer).unwrap();
    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.theme.unwrap().name, "default");
}

#[test]
fn scaffold_with_custom_theme() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("test"), Some("minimal"), &printer).unwrap();
    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.theme.unwrap().name, "minimal");
}

#[test]
fn pick_profile_single_profile() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = pick_profile(&profiles_dir, &printer).unwrap();
    assert_eq!(result, "base");
}

#[test]
fn pick_profile_no_profiles_errors() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let err = pick_profile(&profiles_dir, &printer).unwrap_err();
    assert!(
        err.to_string().contains("No profiles found"),
        "should report no profiles, got: {err}"
    );
}

#[test]
fn pick_profile_multi_lists_options_and_propagates_prompt_error() {
    // Multiple profiles → pick_profile must (a) emit the "Available Profiles"
    // subheader, (b) emit one info line per profile in 1-based numbered form,
    // (c) call prompt_text. With a JSON-format printer, prompt_text Errs in
    // non-interactive tests, so the function returns that Err — confirming
    // that the multi-profile branch wires through to prompt_text.
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    for stem in ["alpha", "bravo"] {
        std::fs::write(
            profiles_dir.join(format!("{stem}.yaml")),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: {stem}\nspec: {{}}\n"
            ),
        )
        .unwrap();
    }

    let (printer, buf) = Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
    let err = pick_profile(&profiles_dir, &printer)
        .expect_err("non-interactive prompt_text Errs in JSON-format printer");
    let msg = err.to_string();
    assert!(
        msg.contains("Select profile") || msg.to_ascii_lowercase().contains("interactive"),
        "Err must mention the prompt that could not be satisfied: {msg}"
    );

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("Available Profiles"),
        "subheader must fire on multi-profile path: {captured}"
    );
    assert!(
        captured.contains("1. alpha") && captured.contains("2. bravo"),
        "each profile must be enumerated 1-based on its own info line: {captured}"
    );
}

#[test]
fn pick_profile_no_dir_errors() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    // Don't create the dir

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let err = pick_profile(&profiles_dir, &printer).unwrap_err();
    assert!(
        err.to_string().contains("No profiles directory"),
        "should report missing dir, got: {err}"
    );
}

#[test]
fn is_git_source_detects_urls() {
    assert!(is_git_source("https://github.com/user/repo"));
    assert!(is_git_source("http://github.com/user/repo"));
    assert!(is_git_source("git@github.com:user/repo.git"));
    assert!(is_git_source("ssh://git@github.com/user/repo"));
    assert!(is_git_source("git://github.com/user/repo"));
    assert!(is_git_source("/some/local/path.git"));
}

#[test]
fn is_git_source_detects_local_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    assert!(is_git_source(&dir.path().display().to_string()));
}

#[test]
fn is_git_source_rejects_plain_paths() {
    assert!(!is_git_source("/home/user/config"));
    assert!(!is_git_source("~/my-config"));
    assert!(!is_git_source("./relative/path"));
    assert!(!is_git_source("config"));
}

#[test]
fn resolve_from_local_path_with_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = resolve_from(&dir.path().display().to_string(), None, "master", &printer);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), dir.path());
}

#[test]
fn resolve_from_local_path_missing_config_errors() {
    let dir = tempfile::tempdir().unwrap();
    // No cfgd.yaml

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = resolve_from(&dir.path().display().to_string(), None, "master", &printer);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No cfgd.yaml"));
}

#[test]
fn resolve_from_nonexistent_path_errors() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("does-not-exist");
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = resolve_from(&nonexistent.display().to_string(), None, "master", &printer);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn check_prerequisites_returns_true_when_git_available() {
    // git should be available in CI and dev environments
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = check_prerequisites(&printer);
    if cfgd_core::command_available("git") {
        assert!(
            result,
            "check_prerequisites should return true when git is available"
        );
    } else {
        assert!(
            !result,
            "check_prerequisites should return false when git is missing"
        );
    }
}

#[test]
fn cmd_init_scaffolds_local_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("my-config");
    std::fs::create_dir_all(&target).unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let args = InitArgs {
        path: Some(target.to_str().unwrap()),
        from: None,
        branch: "master",
        name: Some("test-init"),
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    let result = cmd_init(&printer, &args);
    assert!(result.is_ok(), "cmd_init failed: {:?}", result.err());

    // Verify scaffolded files exist and have expected content
    let cfg = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        cfg.contains("name: test-init"),
        "cfgd.yaml should contain the name"
    );
    assert!(
        cfg.contains("cfgd.io/v1alpha1"),
        "cfgd.yaml should have apiVersion"
    );
    assert!(target.join("profiles").is_dir());
    assert!(target.join("modules").is_dir());
    let gitignore = std::fs::read_to_string(target.join(".gitignore")).unwrap();
    assert!(
        gitignore.contains("!cfgd.yaml"),
        ".gitignore should allow cfgd.yaml"
    );
    assert!(target.join("README.md").exists());

    // Verify git repo was initialized
    assert!(target.join(".git").exists());
}

#[test]
fn cmd_init_skips_if_already_initialized() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("existing");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: existing\nspec: {}\n",
    )
    .unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let args = InitArgs {
        path: Some(target.to_str().unwrap()),
        from: None,
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    let result = cmd_init(&printer, &args);
    assert!(result.is_ok());
    // cfgd.yaml should be unchanged (not overwritten)
    let contents = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(contents.contains("name: existing"));
}

#[test]
fn cmd_init_creates_directory_if_missing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("new-config");
    assert!(!target.exists());

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let args = InitArgs {
        path: Some(target.to_str().unwrap()),
        from: None,
        branch: "master",
        name: Some("fresh"),
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: Some("minimal"),
        apply_profile: None,
        apply_modules: &[],
    };

    let result = cmd_init(&printer, &args);
    assert!(result.is_ok(), "cmd_init failed: {:?}", result.err());
    assert!(target.exists());
    assert!(target.join("cfgd.yaml").exists());

    let contents = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(contents.contains("theme: minimal"));
}

#[test]
fn clone_into_local_repo() {
    let dir = tempfile::tempdir().unwrap();

    // Create a source repo with a commit
    let origin = dir.path().join("origin");
    let repo = git2::Repository::init(&origin).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    std::fs::write(
        origin.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let target = dir.path().join("clone");
    std::fs::create_dir_all(&target).unwrap();

    let (printer, buf) = Printer::for_test();
    clone_into(&target, &origin.display().to_string(), "master", &printer).unwrap();

    assert!(target.join(".git").exists(), "should create .git directory");
    let cfg = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        cfg.contains("name: test"),
        "cloned cfgd.yaml should contain the name"
    );

    let output = buf.lock().unwrap();
    assert!(output.contains("Cloned"), "should report successful clone");
}

#[test]
fn clone_into_skips_if_already_cloned() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clone");
    std::fs::create_dir_all(target.join(".git")).unwrap();

    let (printer, buf) = Printer::for_test();
    // Should return Ok without actually cloning
    clone_into(
        &target,
        "https://example.com/nonexistent",
        "master",
        &printer,
    )
    .unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("already exists"),
        "should report repo already exists, got: {output}"
    );
}

#[test]
fn resolve_from_git_source_local_repo() {
    let dir = tempfile::tempdir().unwrap();

    // Create a source repo
    let origin = dir.path().join("origin");
    let repo = git2::Repository::init(&origin).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    std::fs::write(
        origin.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let target = dir.path().join("target");
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = resolve_from(
        &origin.display().to_string(),
        Some(&target),
        "master",
        &printer,
    )
    .unwrap();

    assert_eq!(result, target, "should return the target path");
    let cfg = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        cfg.contains("name: test"),
        "cloned cfgd.yaml should have the source content"
    );
}

#[test]
fn resolve_from_already_initialized_git_source() {
    let dir = tempfile::tempdir().unwrap();

    // Create target with existing cfgd.yaml
    let target = dir.path().join("already-init");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    // Using a git URL (https://) triggers the git source path
    let result = resolve_from(
        "https://example.com/repo.git",
        Some(&target),
        "master",
        &printer,
    );
    assert!(result.is_ok());
    // Should return the target path without re-cloning
    assert_eq!(result.unwrap(), target);
}

#[test]
fn scaffold_creates_readme_with_name() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("my-dotfiles"), None, &printer).unwrap();

    let readme = std::fs::read_to_string(dir.path().join("README.md")).unwrap();
    assert!(
        readme.contains("my-dotfiles"),
        "README should contain the config name"
    );
}

#[test]
fn scaffold_creates_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("test"), None, &printer).unwrap();

    let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains("!cfgd.yaml"));
    assert!(gitignore.contains("!profiles/"));
    assert!(gitignore.contains("!modules/"));
}

#[test]
fn scaffold_creates_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("test"), None, &printer).unwrap();

    let workflow_path = dir.path().join(".github/workflows/cfgd-release.yml");
    let workflow = std::fs::read_to_string(&workflow_path).unwrap();
    assert!(workflow.contains("cfgd"), "workflow should reference cfgd");
}

#[test]
fn cmd_init_with_from_local_path() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: source\nspec: {}\n",
    )
    .unwrap();

    let (printer, buf) = Printer::for_test();
    let source_str = source.display().to_string();
    let args = InitArgs {
        path: None,
        from: Some(&source_str),
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Initialize") || output.contains("Initialized"),
        "should show init header, got: {output}"
    );
}

#[test]
fn ensure_config_file_creates_without_origin() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    ensure_config_file(dir.path(), &config_path, "default", None, "master", None).unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("profile: default"));
    // No origin section when from_url is None
    assert!(!contents.contains("origin:"));
}

#[test]
fn count_packages_npm_and_pipx() {
    let spec = config::ProfileSpec {
        packages: Some(config::PackagesSpec {
            npm: Some(config::NpmSpec {
                global: vec!["prettier".into(), "eslint".into()],
                ..Default::default()
            }),
            pipx: vec!["black".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(count_packages(&spec), 3);
}

#[test]
fn count_packages_dnf() {
    let spec = config::ProfileSpec {
        packages: Some(config::PackagesSpec {
            dnf: vec!["vim".into(), "tmux".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(count_packages(&spec), 2);
}

// ─── cmd_init — scaffold to new directory ──────────────────

#[test]
fn cmd_init_scaffold_to_new_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("new-config");

    let (printer, buf) = Printer::for_test();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("my-machine"),
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    // Verify scaffolded structure
    assert!(target.join("cfgd.yaml").exists(), "should create cfgd.yaml");
    assert!(target.join("profiles").exists(), "should create profiles/");
    assert!(target.join("modules").exists(), "should create modules/");
    assert!(
        target.join(".gitignore").exists(),
        "should create .gitignore"
    );

    let config = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        config.contains("my-machine"),
        "config should contain the name, got: {config}"
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Initialized"),
        "should show init success, got: {output}"
    );
}

#[test]
fn cmd_init_already_initialized() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("existing");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: existing\nspec: {}\n",
    )
    .unwrap();

    let (printer, buf) = Printer::for_test();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Already initialized"),
        "should report already initialized, got: {output}"
    );
}

#[test]
fn cmd_init_with_theme() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("themed");

    let (printer, _buf) = Printer::for_test();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: Some("nord"),
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    let config = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        config.contains("nord"),
        "config should contain the theme, got: {config}"
    );
}

// ─── resolve_from — local path variants ────────────────────

#[test]
fn resolve_from_local_path_valid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = resolve_from(&dir.path().display().to_string(), None, "master", &printer).unwrap();
    assert_eq!(result, dir.path());
}

#[test]
fn resolve_from_local_path_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    let err =
        resolve_from(&dir.path().display().to_string(), None, "master", &printer).unwrap_err();
    assert!(
        err.to_string().contains("No cfgd.yaml"),
        "should report missing cfgd.yaml, got: {err}"
    );
}

#[test]
fn resolve_from_nonexistent_path_fails() {
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let err = resolve_from("/nonexistent/path/xyz", None, "master", &printer).unwrap_err();
    assert!(
        err.to_string().contains("does not exist"),
        "should report path does not exist, got: {err}"
    );
}

// ─── is_git_source — comprehensive coverage ────────────────

#[test]
fn is_git_source_https() {
    assert!(is_git_source("https://github.com/user/repo"));
    assert!(is_git_source("https://github.com/user/repo.git"));
}

#[test]
fn is_git_source_ssh() {
    assert!(is_git_source("ssh://git@github.com/user/repo"));
    assert!(is_git_source("git@github.com:user/repo.git"));
}

#[test]
fn is_git_source_git_protocol() {
    assert!(is_git_source("git://github.com/user/repo"));
}

#[test]
fn is_git_source_dot_git_suffix() {
    assert!(is_git_source("anything.git"));
}

#[test]
fn is_git_source_local_plain_dir_not_git() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        !is_git_source(&dir.path().display().to_string()),
        "plain directory without .git should not be a git source"
    );
}

#[test]
fn is_git_source_local_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    assert!(
        is_git_source(&dir.path().display().to_string()),
        "directory with .git should be a git source"
    );
}

// ─── scaffold — content verification ───────────────────────

#[test]
fn scaffold_gitignore_content() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("test"), None, &printer).unwrap();

    let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.contains("!profiles/**"),
        "should unignore profiles"
    );
    assert!(gitignore.contains("!modules/**"), "should unignore modules");
    assert!(gitignore.contains("!.github/"), "should unignore .github");
}

#[test]
fn scaffold_config_content() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(dir.path(), Some("my-dots"), Some("catppuccin"), &printer).unwrap();

    let config = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    assert!(config.contains("my-dots"), "should use custom name");
    assert!(config.contains("catppuccin"), "should use custom theme");
    assert!(
        config.contains("Symlink"),
        "should set default file strategy"
    );
}

#[test]
fn scaffold_uses_dir_name_when_no_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cool-dotfiles");
    std::fs::create_dir_all(&target).unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

    scaffold(&target, None, None, &printer).unwrap();

    let config = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        config.contains("cool-dotfiles"),
        "should use directory name, got: {config}"
    );
}

// ─── pick_profile — edge cases ─────────────────────────────

#[test]
fn pick_profile_single_profile_auto_selects() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(
        dir.path().join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let (printer, buf) = Printer::for_test();
    let result = pick_profile(dir.path(), &printer).unwrap();
    assert_eq!(result, "default");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Using only available profile: default"),
        "should auto-select single profile, got: {output}"
    );
}

#[test]
fn pick_profile_no_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nope");
    let (printer, _buf) = Printer::for_test();

    let err = pick_profile(&nonexistent, &printer).unwrap_err();
    assert!(
        err.to_string().contains("No profiles directory"),
        "should report no profiles dir, got: {err}"
    );
}

#[test]
fn pick_profile_empty_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, _buf) = Printer::for_test();

    let err = pick_profile(dir.path(), &printer).unwrap_err();
    assert!(
        err.to_string().contains("No profiles found"),
        "should report no profiles, got: {err}"
    );
}

// ─── regenerate_workflow ────────────────────────────────────

#[test]
fn regenerate_workflow_skips_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();
    let (printer, _buf) = Printer::for_test();

    regenerate_workflow(dir.path(), &printer).unwrap();

    // No workflow should be generated for empty project
    assert!(
        !dir.path()
            .join(".github/workflows/cfgd-release.yml")
            .exists(),
        "should not create workflow for empty project"
    );
}

#[test]
fn regenerate_workflow_creates_for_content() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();

    let (printer, _buf) = Printer::for_test();
    regenerate_workflow(dir.path(), &printer).unwrap();

    let workflow_path = dir.path().join(".github/workflows/cfgd-release.yml");
    assert!(workflow_path.exists(), "should create workflow");
    let content = std::fs::read_to_string(&workflow_path).unwrap();
    assert!(content.contains("cfgd"), "workflow should reference cfgd");
}

// ─── ensure_config_file — branch and origin ────────────────

#[test]
fn ensure_config_file_with_origin() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    ensure_config_file(
        dir.path(),
        &config_path,
        "work",
        Some("https://github.com/team/config"),
        "main",
        None,
    )
    .unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("profile: work"), "should set profile");
    assert!(
        contents.contains("origin:"),
        "should have origin section for git URL"
    );
    assert!(
        contents.contains("https://github.com/team/config"),
        "should contain the URL"
    );
    assert!(
        contents.contains("branch: main"),
        "should contain the branch"
    );
}

#[test]
fn ensure_config_file_with_theme() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    ensure_config_file(
        dir.path(),
        &config_path,
        "default",
        None,
        "master",
        Some("dracula"),
    )
    .unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        contents.contains("dracula"),
        "should use custom theme, got: {contents}"
    );
}

// ─── count_packages — additional cases ─────────────────────

#[test]
fn count_packages_no_packages_spec() {
    let spec = config::ProfileSpec {
        packages: None,
        ..Default::default()
    };
    assert_eq!(count_packages(&spec), 0);
}

#[test]
fn count_packages_brew_formulae_and_casks() {
    let spec = config::ProfileSpec {
        packages: Some(config::PackagesSpec {
            brew: Some(config::BrewSpec {
                file: None,
                taps: vec![],
                formulae: vec!["curl".into(), "vim".into()],
                casks: vec!["firefox".into()],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(count_packages(&spec), 3);
}

#[test]
fn count_packages_apt_and_cargo() {
    let spec = config::ProfileSpec {
        packages: Some(config::PackagesSpec {
            apt: Some(config::AptSpec {
                packages: vec!["build-essential".into()],
                ..Default::default()
            }),
            cargo: Some(config::CargoSpec {
                file: None,
                packages: vec!["ripgrep".into(), "fd-find".into()],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(count_packages(&spec), 3);
}

// ─── is_git_source — additional edge cases ────────────────────

#[test]
fn is_git_source_http_url() {
    assert!(is_git_source("http://internal.host/repo"));
}

#[test]
fn is_git_source_empty_string() {
    assert!(!is_git_source(""));
}

#[test]
fn is_git_source_relative_path_not_git() {
    assert!(!is_git_source("relative/path"));
}

#[test]
fn is_git_source_dot_git_middle_not_matched_as_suffix() {
    // Only .git at the END matters
    assert!(!is_git_source("something.github"));
}

#[test]
fn is_git_source_bare_name_not_git() {
    assert!(!is_git_source("myconfig"));
}

#[test]
fn is_git_source_file_ending_in_dot_git() {
    assert!(is_git_source("file:///path/to/repo.git"));
}

// ─── resolve_from — additional cases ──────────────────────────

#[test]
fn resolve_from_local_path_returns_canonicalized_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = resolve_from(&dir.path().display().to_string(), None, "main", &printer).unwrap();
    // The result should be a valid path containing cfgd.yaml
    assert!(result.join("cfgd.yaml").exists());
}

#[test]
fn resolve_from_git_source_with_target_creates_dir() {
    let dir = tempfile::tempdir().unwrap();

    // Create a source repo
    let origin = dir.path().join("origin");
    let repo = git2::Repository::init(&origin).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    std::fs::write(
        origin.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let target = dir.path().join("new-target");
    assert!(!target.exists());

    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = resolve_from(
        &origin.display().to_string(),
        Some(&target),
        "master",
        &printer,
    )
    .unwrap();

    assert_eq!(result, target);
    assert!(target.join("cfgd.yaml").exists());
}

// ─── scaffold — idempotency and content ───────────────────────

#[test]
fn scaffold_config_has_api_version() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    scaffold(dir.path(), Some("my-cfg"), None, &printer).unwrap();

    let config = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    assert!(
        config.contains("apiVersion: cfgd.io/v1alpha1"),
        "config should have apiVersion"
    );
    assert!(
        config.contains("kind: Config"),
        "config should have kind: Config"
    );
    assert!(
        config.contains("fileStrategy: Symlink"),
        "config should set default file strategy"
    );
}

#[test]
fn scaffold_readme_contains_structure_docs() {
    let dir = tempfile::tempdir().unwrap();
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    scaffold(dir.path(), Some("documented"), None, &printer).unwrap();

    let readme = std::fs::read_to_string(dir.path().join("README.md")).unwrap();
    assert!(
        readme.contains("profiles/"),
        "README should mention profiles"
    );
    assert!(readme.contains("modules/"), "README should mention modules");
    assert!(
        readme.contains("cfgd.yaml"),
        "README should mention cfgd.yaml"
    );
    assert!(
        readme.contains("documented"),
        "README should contain the config name"
    );
}

// ─── cmd_init — flag combinations ─────────────────────────────

#[test]
fn cmd_init_with_name_overrides_dir_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("generic-dir");

    let (printer, _buf) = Printer::for_test();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("my-awesome-config"),
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    let config = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        config.contains("my-awesome-config"),
        "config should use the explicit name, not the dir name, got: {config}"
    );
}

#[test]
fn cmd_init_creates_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("git-init-test");

    let (printer, _buf) = Printer::for_test();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("test"),
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    assert!(
        target.join(".git").exists(),
        "cmd_init should create a .git directory"
    );
    // Verify it's a valid git repo
    let repo = git2::Repository::open(&target);
    assert!(repo.is_ok(), "should be a valid git repository");
}

#[test]
fn cmd_init_with_theme_and_name_together() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("combo-test");

    let (printer, _buf) = Printer::for_test();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: None,
        branch: "master",
        name: Some("my-workstation"),
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: Some("catppuccin"),
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    let config = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        config.contains("my-workstation"),
        "config should have the name"
    );
    assert!(
        config.contains("catppuccin"),
        "config should have the theme"
    );
}

// ─── apply_plan — dry run and empty plan ─────────────────────

#[test]
fn apply_plan_empty_plan_reports_nothing_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, buf) = Printer::for_test();

    let registry = super::build_registry_with_config(None);
    // Isolate the state store under the tempdir so parallel tests don't
    // contend on the shared default state database (SQLite lock contention
    // manifests as 'database is locked' on slower filesystems, e.g. macOS).
    let store = super::open_state_store(Some(dir.path())).unwrap();
    let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
    let resolved = config::ResolvedProfile {
        layers: Vec::new(),
        merged: config::MergedProfile::default(),
    };

    let plan = cfgd_core::reconciler::Plan {
        phases: Vec::new(),
        warnings: Vec::new(),
    };
    let result = apply_plan(
        &plan,
        &reconciler,
        &resolved,
        dir.path(),
        false,
        false,
        &printer,
    );
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Nothing to do"),
        "should report nothing to do for empty plan, got: {output}"
    );
}

// ─── check_prerequisites — git availability ──────────────────

#[test]
fn check_prerequisites_with_test_printer() {
    let (printer, buf) = Printer::for_test();
    let result = check_prerequisites(&printer);
    let output = buf.lock().unwrap();

    if cfgd_core::command_available("git") {
        assert!(result, "should return true when git available");
        // No error output when git is available
        assert!(
            !output.contains("not installed"),
            "should not show error when git is available"
        );
    } else {
        assert!(!result, "should return false when git unavailable");
        assert!(
            output.contains("not installed"),
            "should show error when git is missing, got: {output}"
        );
    }
}

// ─── ensure_config_file — edge cases ─────────────────────────

#[test]
fn ensure_config_file_preserves_other_fields_on_profile_update() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");

    // Write a config with extra fields
    let original = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-config
spec:
  profile: old-profile
  fileStrategy: Symlink
"#;
    std::fs::write(&config_path, original).unwrap();

    ensure_config_file(
        dir.path(),
        &config_path,
        "new-profile",
        None,
        "master",
        None,
    )
    .unwrap();

    let cfg = config::load_config(&config_path).unwrap();
    assert_eq!(
        cfg.spec.profile.as_deref(),
        Some("new-profile"),
        "profile should be updated"
    );
    assert_eq!(cfg.metadata.name, "my-config", "name should be preserved");
}

// ─── regenerate_workflow — with modules ──────────────────────

#[test]
fn regenerate_workflow_with_modules_and_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    let modules_dir = dir.path().join("modules");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::create_dir_all(&modules_dir).unwrap();

    // Create a profile
    std::fs::write(
        profiles_dir.join("work.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec: {}\n",
    )
    .unwrap();

    // Create a module directory
    let mod_dir = modules_dir.join("git");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git\nspec: {}\n",
    )
    .unwrap();

    let (printer, _buf) = Printer::for_test();
    regenerate_workflow(dir.path(), &printer).unwrap();

    let workflow_path = dir.path().join(".github/workflows/cfgd-release.yml");
    assert!(workflow_path.exists(), "should create workflow");
    let content = std::fs::read_to_string(&workflow_path).unwrap();
    assert!(
        content.contains("work") || content.contains("git"),
        "workflow should reference profiles or modules, got: {content}"
    );
}

// ─── count_packages — all managers at once ───────────────────

#[test]
fn count_packages_all_managers_combined() {
    let spec = config::ProfileSpec {
        packages: Some(config::PackagesSpec {
            brew: Some(config::BrewSpec {
                formulae: vec!["git".into()],
                casks: vec!["iterm2".into()],
                ..Default::default()
            }),
            apt: Some(config::AptSpec {
                packages: vec!["vim".into()],
                ..Default::default()
            }),
            cargo: Some(config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            npm: Some(config::NpmSpec {
                global: vec!["prettier".into()],
                ..Default::default()
            }),
            pipx: vec!["black".into()],
            dnf: vec!["tmux".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    // brew: 1 formula + 1 cask = 2, apt: 1, cargo: 1, npm: 1, pipx: 1, dnf: 1 = 7
    assert_eq!(count_packages(&spec), 7);
}

// ─── cmd_init — from local path ──────────────────────────────

#[test]
fn cmd_init_from_local_path_uses_source_dir() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("local-source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: local\nspec: {}\n",
    )
    .unwrap();
    // Create profiles/modules so workflow step doesn't fail
    std::fs::create_dir_all(source.join("profiles")).unwrap();
    std::fs::create_dir_all(source.join("modules")).unwrap();

    let (printer, buf) = Printer::for_test();
    let source_str = source.display().to_string();
    let args = InitArgs {
        path: None,
        from: Some(&source_str),
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    cmd_init(&printer, &args).unwrap();

    let output = buf.lock().unwrap();
    // The init should succeed and reference the source path
    assert!(
        output.contains("Initialize")
            || output.contains("Initialized")
            || output.contains("Already"),
        "should show init output, got: {output}"
    );
}

// --- clone_into tests ---

#[test]
fn clone_into_skips_existing_git_dir() {
    let dir = tempfile::tempdir().unwrap();

    // Create a repo to serve as the target that already has .git
    let target = dir.path().join("already-cloned");
    git2::Repository::init(&target).unwrap();

    let (printer, buf) = Printer::for_test();
    clone_into(&target, "https://example.com/repo.git", "main", &printer).unwrap();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("already exists"),
        "should report repo already exists, got: {output}"
    );
}

// --- sign_with_ssh tests ---

#[test]
fn sign_with_ssh_requires_ssh_keygen() {
    if !cfgd_core::command_available("ssh-keygen") {
        // If ssh-keygen not available, the function should error
        let result = sign_with_ssh("test-nonce", "/nonexistent/key");
        assert!(result.is_err());
        return;
    }
    // If ssh-keygen is available but key path is bad, it should fail gracefully
    let result = sign_with_ssh("test-nonce", "/nonexistent/key");
    assert!(result.is_err(), "should fail with nonexistent key file");
}

// --- sign_with_gpg tests ---

#[test]
fn sign_with_gpg_requires_gpg() {
    if !cfgd_core::command_available("gpg") {
        let result = sign_with_gpg("test-nonce", "DEADBEEF");
        assert!(result.is_err());
        return;
    }
    // If gpg is available but key ID is invalid, it should fail
    let result = sign_with_gpg("test-nonce", "NONEXISTENT_KEY_ID_XXXXXXXX");
    assert!(result.is_err(), "should fail with nonexistent GPG key");
}

// --- detect_ssh_key tests ---

#[test]
fn detect_ssh_key_returns_option() {
    let (printer, _buf) = Printer::for_test();
    // This will either find a key or return None — both are valid
    let result = detect_ssh_key(&printer);
    // Just verify it doesn't panic and returns a valid type
    if let Some(ref path) = result {
        assert!(!path.is_empty(), "returned path should be non-empty");
    }
}

// --- default_device_id ---

#[test]
fn default_device_id_is_nonempty() {
    let id = default_device_id();
    assert!(!id.is_empty(), "device ID should not be empty");
}

#[test]
fn default_device_id_is_deterministic() {
    let id1 = default_device_id();
    let id2 = default_device_id();
    assert_eq!(id1, id2, "device ID should be deterministic");
}

// --- apply_plan prompt-declined branch (yes=false, prompt fails or returns false) ---

#[test]
fn apply_plan_prompt_declined_branch_prints_skipped_and_returns_ok() {
    // yes=false + non-empty plan + dry_run=false → cmd_init's apply_plan enters
    // the prompt_confirm arm. In test mode, prompt_confirm returns Err →
    // unwrap_or(false) → false → "Skipped" message + early return. Pins the
    // contract that a declined apply does NOT touch the reconciler or hit
    // the state-store apply lock.
    let dir = tempfile::tempdir().unwrap();
    let (printer, buf) = Printer::for_test();

    let registry = super::build_registry_with_config(None);
    let store = super::open_state_store(Some(dir.path())).unwrap();
    let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
    let resolved = config::ResolvedProfile {
        layers: Vec::new(),
        merged: config::MergedProfile::default(),
    };

    let plan = cfgd_core::reconciler::Plan {
        phases: vec![cfgd_core::reconciler::Phase {
            name: cfgd_core::reconciler::PhaseName::Packages,
            actions: vec![cfgd_core::reconciler::Action::Package(
                cfgd_core::providers::PackageAction::Install {
                    manager: "brew".to_string(),
                    packages: vec!["test-pkg".to_string()],
                    origin: "test".to_string(),
                },
            )],
        }],
        warnings: Vec::new(),
    };

    let result = apply_plan(
        &plan,
        &reconciler,
        &resolved,
        dir.path(),
        false, // dry_run
        false, // yes — exercises the prompt arm
        &printer,
    );
    assert!(result.is_ok(), "declined prompt should still return Ok");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Skipped"),
        "expected 'Skipped' message on declined prompt, got: {output}"
    );
    assert!(
        output.contains("cfgd apply"),
        "expected hint to run 'cfgd apply' later, got: {output}"
    );
}

// --- apply_plan with dry_run ---

#[test]
fn apply_plan_dry_run_skips_apply() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, buf) = Printer::for_test();

    let registry = super::build_registry_with_config(None);
    // Isolate the state store under the tempdir so parallel tests don't
    // contend on the shared default state database.
    let store = super::open_state_store(Some(dir.path())).unwrap();
    let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
    let resolved = config::ResolvedProfile {
        layers: Vec::new(),
        merged: config::MergedProfile::default(),
    };

    // Create a plan with at least one action so dry_run actually has something to skip
    let plan = cfgd_core::reconciler::Plan {
        phases: vec![cfgd_core::reconciler::Phase {
            name: cfgd_core::reconciler::PhaseName::Packages,
            actions: vec![cfgd_core::reconciler::Action::Package(
                cfgd_core::providers::PackageAction::Install {
                    manager: "brew".to_string(),
                    packages: vec!["test-pkg".to_string()],
                    origin: "test".to_string(),
                },
            )],
        }],
        warnings: Vec::new(),
    };

    let result = apply_plan(
        &plan,
        &reconciler,
        &resolved,
        dir.path(),
        true, // dry_run
        false,
        &printer,
    );
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    // Dry run shows the plan but does not apply
    assert!(
        output.contains("action") || output.contains("planned"),
        "should show planned actions, got: {output}"
    );
}

// --- cmd_init with --from pointing to git repo + custom target ---

#[test]
fn cmd_init_from_git_source_with_explicit_target() {
    let dir = tempfile::tempdir().unwrap();

    // Create a source git repo
    let origin = dir.path().join("origin");
    let repo = git2::Repository::init(&origin).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    std::fs::write(
        origin.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: origin-cfg\nspec: {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(origin.join("profiles")).unwrap();
    std::fs::create_dir_all(origin.join("modules")).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let target = dir.path().join("my-target");
    let (printer, _buf) = Printer::for_test();
    let origin_str = origin.display().to_string();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: Some(&origin_str),
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: None,
        apply_profile: None,
        apply_modules: &[],
    };

    let result = cmd_init(&printer, &args);
    assert!(
        result.is_ok(),
        "cmd_init with --from git should succeed: {:?}",
        result.err()
    );
    assert!(
        target.join("cfgd.yaml").exists(),
        "should clone to target dir"
    );
}

// --- cmd_init with --from git and --theme ---

#[test]
fn cmd_init_from_git_with_theme_override() {
    let dir = tempfile::tempdir().unwrap();

    // Create origin repo
    let origin = dir.path().join("origin");
    let repo = git2::Repository::init(&origin).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    std::fs::write(
        origin.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: themed-cfg\nspec:\n  theme:\n    name: default\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let target = dir.path().join("themed-target");
    let (printer, _buf) = Printer::for_test();
    let origin_str = origin.display().to_string();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: Some(&origin_str),
        branch: "master",
        name: None,
        apply: false,
        dry_run: false,
        yes: false,
        install_daemon: false,
        theme: Some("dracula"),
        apply_profile: None,
        apply_modules: &[],
    };

    let result = cmd_init(&printer, &args);
    assert!(
        result.is_ok(),
        "cmd_init with theme override should succeed: {:?}",
        result.err()
    );

    let config = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        config.contains("dracula"),
        "theme should be overridden to dracula, got: {config}"
    );
}

// --- generate_release_workflow_yaml ---

#[test]
fn generate_release_workflow_yaml_empty_inputs() {
    let yaml = generate_release_workflow_yaml(&[], &[], "master");
    assert!(!yaml.is_empty(), "should produce non-empty YAML");
    assert!(yaml.contains("cfgd"), "should reference cfgd");
}

#[test]
fn generate_release_workflow_yaml_with_modules() {
    let yaml =
        generate_release_workflow_yaml(&["git".to_string(), "tmux".to_string()], &[], "master");
    assert!(
        yaml.contains("git") || yaml.contains("tmux"),
        "should reference module names"
    );
}

#[test]
fn generate_release_workflow_yaml_with_profiles() {
    let yaml =
        generate_release_workflow_yaml(&[], &["work".to_string(), "home".to_string()], "master");
    assert!(
        yaml.contains("work") || yaml.contains("home"),
        "should reference profile names"
    );
}

#[test]
fn generate_release_workflow_yaml_with_both() {
    let yaml =
        generate_release_workflow_yaml(&["neovim".to_string()], &["base".to_string()], "master");
    assert!(!yaml.is_empty());
}

// --- scan_profile_names / scan_module_names ---

#[test]
fn scan_profile_names_filters_yaml_only() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("work.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec: {}\n",
    )
    .unwrap();
    std::fs::write(profiles.join("notes.txt"), "not a profile\n").unwrap();
    std::fs::write(
        profiles.join("home.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: home\nspec: {}\n",
    )
    .unwrap();

    let names = scan_profile_names(&profiles).unwrap();
    assert!(names.contains(&"work".to_string()));
    assert!(names.contains(&"home".to_string()));
    assert!(!names.contains(&"notes".to_string()));
}

#[test]
fn scan_module_names_finds_dirs_with_module_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let modules = dir.path().join("modules");
    std::fs::create_dir_all(modules.join("git")).unwrap();
    std::fs::write(modules.join("git").join("module.yaml"), "spec: {}\n").unwrap();
    std::fs::create_dir_all(modules.join("empty")).unwrap();
    // empty module dir without module.yaml

    let names = scan_module_names(&modules).unwrap();
    assert!(names.contains(&"git".to_string()));
    // "empty" should not appear since it has no module.yaml
}

// --- list_yaml_stems ---

#[test]
fn list_yaml_stems_returns_sorted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("zebra.yaml"), "").unwrap();
    std::fs::write(dir.path().join("alpha.yaml"), "").unwrap();
    std::fs::write(dir.path().join("middle.yaml"), "").unwrap();

    let stems = super::list_yaml_stems(dir.path()).unwrap();
    assert_eq!(stems, vec!["alpha", "middle", "zebra"]);
}

#[test]
fn list_yaml_stems_ignores_non_yaml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("good.yaml"), "").unwrap();
    std::fs::write(dir.path().join("bad.txt"), "").unwrap();
    std::fs::write(dir.path().join("also.json"), "").unwrap();

    let stems = super::list_yaml_stems(dir.path()).unwrap();
    assert_eq!(stems, vec!["good"]);
}

#[test]
fn list_yaml_stems_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let stems = super::list_yaml_stems(dir.path()).unwrap();
    assert!(stems.is_empty());
}

// --- should_run_apply ---

#[test]
fn should_run_apply_explicit_apply_flag_triggers() {
    assert!(super::should_run_apply(true, None, &[]));
}

#[test]
fn should_run_apply_apply_profile_triggers() {
    assert!(super::should_run_apply(false, Some("dev"), &[]));
}

#[test]
fn should_run_apply_apply_modules_triggers() {
    assert!(super::should_run_apply(
        false,
        None,
        &["neovim".to_string()]
    ));
}

#[test]
fn should_run_apply_returns_false_when_no_apply_signals() {
    // None of --apply / --apply-profile / --apply-module passed.
    assert!(!super::should_run_apply(false, None, &[]));
}

#[test]
fn should_run_apply_combinations_still_trigger() {
    // Any combination of the three signals must trigger; do not require
    // exclusivity (the cmd_init body uses the same OR rule).
    assert!(super::should_run_apply(
        true,
        Some("dev"),
        &["m".to_string()]
    ));
    assert!(super::should_run_apply(true, None, &["m".to_string()]));
    assert!(super::should_run_apply(
        false,
        Some("dev"),
        &["m".to_string()]
    ));
}

// --- is_module_only_apply ---

#[test]
fn is_module_only_apply_true_when_modules_only() {
    // Modules supplied + no profile → module-only mode.
    assert!(super::is_module_only_apply(
        None,
        &["neovim".to_string(), "tmux".to_string()],
    ));
}

#[test]
fn is_module_only_apply_false_when_profile_present() {
    // Even with modules, presence of --apply-profile drops us into the
    // regular profile-based path so the profile's modules merge with
    // the user's --apply-module additions.
    assert!(!super::is_module_only_apply(
        Some("dev"),
        &["neovim".to_string()]
    ));
}

#[test]
fn is_module_only_apply_false_when_no_modules() {
    // No modules → never module-only, regardless of profile presence.
    assert!(!super::is_module_only_apply(None, &[]));
    assert!(!super::is_module_only_apply(Some("dev"), &[]));
}

// --- first_existing_ssh_key ---

#[test]
fn first_existing_ssh_key_returns_pub_over_private() {
    // When both `id_ed25519` and `id_ed25519.pub` exist, the .pub wins
    // (the public key is what the enrollment payload sends).
    let ssh_dir = tempfile::tempdir().unwrap();
    std::fs::write(ssh_dir.path().join("id_ed25519"), b"private").unwrap();
    std::fs::write(ssh_dir.path().join("id_ed25519.pub"), b"public").unwrap();
    let found = super::first_existing_ssh_key(ssh_dir.path()).unwrap();
    assert_eq!(found.file_name().unwrap(), "id_ed25519.pub");
}

#[test]
fn first_existing_ssh_key_falls_back_to_private_when_no_pub() {
    // Only the private key present — fall back to it (the caller still
    // gets a usable signing path even without a discoverable .pub).
    let ssh_dir = tempfile::tempdir().unwrap();
    std::fs::write(ssh_dir.path().join("id_ed25519"), b"private").unwrap();
    let found = super::first_existing_ssh_key(ssh_dir.path()).unwrap();
    assert_eq!(found.file_name().unwrap(), "id_ed25519");
}

#[test]
fn first_existing_ssh_key_priority_ed25519_over_rsa_over_ecdsa() {
    // All three present → ed25519 wins (modern + smallest key).
    let ssh_dir = tempfile::tempdir().unwrap();
    for key in &["id_ed25519", "id_rsa", "id_ecdsa"] {
        std::fs::write(ssh_dir.path().join(format!("{key}.pub")), b"k").unwrap();
    }
    let found = super::first_existing_ssh_key(ssh_dir.path()).unwrap();
    assert_eq!(found.file_name().unwrap(), "id_ed25519.pub");
}

#[test]
fn first_existing_ssh_key_picks_rsa_when_ed25519_missing() {
    let ssh_dir = tempfile::tempdir().unwrap();
    std::fs::write(ssh_dir.path().join("id_rsa.pub"), b"k").unwrap();
    std::fs::write(ssh_dir.path().join("id_ecdsa.pub"), b"k").unwrap();
    let found = super::first_existing_ssh_key(ssh_dir.path()).unwrap();
    assert_eq!(found.file_name().unwrap(), "id_rsa.pub");
}

#[test]
fn first_existing_ssh_key_picks_ecdsa_when_only_option() {
    let ssh_dir = tempfile::tempdir().unwrap();
    std::fs::write(ssh_dir.path().join("id_ecdsa.pub"), b"k").unwrap();
    let found = super::first_existing_ssh_key(ssh_dir.path()).unwrap();
    assert_eq!(found.file_name().unwrap(), "id_ecdsa.pub");
}

#[test]
fn first_existing_ssh_key_returns_none_for_empty_dir() {
    let ssh_dir = tempfile::tempdir().unwrap();
    assert!(super::first_existing_ssh_key(ssh_dir.path()).is_none());
}

#[test]
fn first_existing_ssh_key_ignores_unrecognized_key_names() {
    // Files like `id_dsa` (legacy) or `github_actions` are not in the
    // priority list and must be skipped — only the canonical names match.
    let ssh_dir = tempfile::tempdir().unwrap();
    std::fs::write(ssh_dir.path().join("id_dsa"), b"k").unwrap();
    std::fs::write(ssh_dir.path().join("github_actions.pub"), b"k").unwrap();
    assert!(super::first_existing_ssh_key(ssh_dir.path()).is_none());
}

#[test]
fn first_existing_ssh_key_returns_none_when_dir_does_not_exist() {
    // No tempdir — pass a path that doesn't exist. Function must not panic.
    let found = super::first_existing_ssh_key(std::path::Path::new("/nonexistent/ssh/dir/12345"));
    assert!(found.is_none());
}

// ============================================================================
// build_device_credential — fields propagated from EnrollResponse → DeviceCredential
// ============================================================================

fn make_enroll_response(
    username: &str,
    team: Option<&str>,
    api_key: &str,
) -> cfgd_core::server_client::EnrollResponse {
    cfgd_core::server_client::EnrollResponse {
        status: "ok".to_string(),
        device_id: "ignored-resp-device-id".to_string(),
        api_key: api_key.to_string(),
        username: username.to_string(),
        team: team.map(|t| t.to_string()),
        desired_config: None,
    }
}

#[test]
fn build_device_credential_propagates_api_key_and_username_from_response() {
    let resp = make_enroll_response("alice", Some("ops"), "secret-key-XYZ");
    let cred = super::build_device_credential("https://srv.example", "dev-1", &resp);
    assert_eq!(cred.server_url, "https://srv.example");
    assert_eq!(cred.device_id, "dev-1");
    assert_eq!(cred.api_key, "secret-key-XYZ");
    assert_eq!(cred.username, "alice");
    assert_eq!(cred.team.as_deref(), Some("ops"));
}

#[test]
fn build_device_credential_uses_caller_device_id_not_response_device_id() {
    // The server is allowed to echo back a device_id in EnrollResponse but
    // the persisted credential MUST use the caller-supplied device_id (the
    // one cfgd computed locally and sent in the request). Anything else
    // would let a server re-bind clients to arbitrary IDs after enroll.
    let resp = make_enroll_response("u", None, "k");
    let cred = super::build_device_credential("https://s", "caller-id", &resp);
    assert_eq!(cred.device_id, "caller-id");
    assert_ne!(cred.device_id, "ignored-resp-device-id");
}

#[test]
fn build_device_credential_no_team_propagates_as_none() {
    let resp = make_enroll_response("u", None, "k");
    let cred = super::build_device_credential("https://s", "d", &resp);
    assert!(cred.team.is_none());
}

#[test]
fn build_device_credential_enrolled_at_is_iso_8601_with_z_suffix() {
    // ISO-8601 UTC timestamps are what every other cfgd surface emits
    // (state store, drift events, gateway logs). The credential must
    // match so operators can correlate by string comparison.
    let resp = make_enroll_response("u", None, "k");
    let cred = super::build_device_credential("https://s", "d", &resp);
    assert!(
        cred.enrolled_at.ends_with('Z'),
        "expected ISO-8601 Z suffix, got: {}",
        cred.enrolled_at
    );
    // Cheap shape check: YYYY-MM-DDTHH:MM:SS minimum is 19 chars before Z.
    assert!(
        cred.enrolled_at.len() >= 20,
        "expected ≥20 chars (YYYY-MM-DDTHH:MM:SSZ), got: {}",
        cred.enrolled_at
    );
}

// ============================================================================
// next_steps_lines — the first-run banner
// ============================================================================

#[test]
fn next_steps_lines_starts_with_checkin_then_apply() {
    let lines = super::next_steps_lines();
    assert_eq!(lines.len(), 4, "exactly four next-step suggestions");
    assert!(
        lines[0].contains("cfgd checkin"),
        "first line: {}",
        lines[0]
    );
    assert!(lines[1].contains("apply --dry-run"), "second: {}", lines[1]);
    assert!(lines[2].contains("cfgd apply"), "third: {}", lines[2]);
    assert!(lines[3].contains("daemon install"), "fourth: {}", lines[3]);
}

#[test]
fn next_steps_lines_are_indented_so_terminal_renders_consistently() {
    // Every banner line is indented by two spaces. The Printer info() call
    // doesn't add indentation — the strings own it. A drift to flush-left
    // would break the visual grouping.
    for line in super::next_steps_lines() {
        assert!(
            line.starts_with("  "),
            "line not 2-space indented: {line:?}"
        );
    }
}

// ===========================================================================
// cmd_enroll mockito chain — drives the token/key-based enrollment paths
// through a real HTTP server so the orchestration in `cli/init/enroll.rs` is
// covered without requiring a live gateway.
// ===========================================================================

mod enroll_mockito {
    use crate::cli::init::enroll::cmd_enroll;
    use cfgd_core::output::Printer;
    use serial_test::serial;

    /// Scoped env override: set/unset `var`, run `f`, restore prior value.
    /// SAFETY: every caller below uses `#[serial]` so no concurrent test
    /// mutates this var.
    fn with_env<F: FnOnce()>(var: &str, value: Option<&str>, f: F) {
        unsafe {
            let prior = std::env::var(var).ok();
            match value {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
            f();
            match prior {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }

    fn enroll_response_json() -> String {
        serde_json::json!({
            "status": "ok",
            "deviceId": "dev-abc-123",
            "apiKey": "key-xyz-789",
            "username": "alice",
            "team": "platform"
        })
        .to_string()
    }

    #[test]
    #[serial]
    fn cmd_enroll_token_path_succeeds_against_mock() {
        let tmp = tempfile::tempdir().unwrap();
        with_env("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(enroll_response_json())
                .create();

            let (printer, buf) = Printer::for_test();
            let url = server.url();
            let result = cmd_enroll(
                &printer,
                &url,
                Some("bootstrap-token-xyz"),
                None,
                None,
                Some("alice"),
            );
            assert!(result.is_ok(), "cmd_enroll should succeed: {result:?}");
            m.assert();

            // Credential file should have been written under the tempdir.
            let cred_path = tmp.path().join("device-credential.json");
            assert!(
                cred_path.exists(),
                "credential file should be at {}",
                cred_path.display()
            );
            let cred: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cred_path).unwrap()).unwrap();
            assert_eq!(cred["apiKey"], "key-xyz-789");
            assert_eq!(cred["username"], "alice");

            // Printer output should announce success.
            let captured = buf.lock().unwrap().clone();
            assert!(
                captured.contains("Enrolled as user 'alice'"),
                "expected success message in: {captured}"
            );
            assert!(
                captured.contains("Next Steps"),
                "expected next-steps header in: {captured}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_token_path_fails_on_server_error() {
        let tmp = tempfile::tempdir().unwrap();
        with_env("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            // 400 is a non-retryable client error — request_error path.
            let _m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(400)
                .with_body(r#"{"error":"invalid token"}"#)
                .create();

            let printer = cfgd_core::test_helpers::test_printer();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, Some("bad-token"), None, None, Some("alice"));
            assert!(result.is_err());
            // No credential written on failure.
            assert!(!tmp.path().join("device-credential.json").exists());
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_key_based_rejects_server_with_token_method() {
        let tmp = tempfile::tempdir().unwrap();
        with_env("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            // Server says it only supports token enrollment.
            let _m = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"method":"token"}"#)
                .create();

            let (printer, _buf) = Printer::for_test();
            let url = server.url();
            // No --token but key-based attempted: should error with a
            // pointer to the token form.
            let result = cmd_enroll(&printer, &url, None, None, None, Some("alice"));
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("bootstrap token enrollment") || err.contains("--token"),
                "expected token-required error, got: {err}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_key_based_bails_when_no_ssh_key_found() {
        // Server says key-based enrollment; no --ssh-key / --gpg-key flags
        // provided; HOME redirected to a tempdir with no `.ssh` directory so
        // detect_ssh_key cannot find anything. Drives the second arm of the
        // key-detection block: cmd_enroll must bail with the help text that
        // names the SSH and GPG flag forms.
        let tmp = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let _home_guard = cfgd_core::with_test_home_guard(home_dir.path());

        with_env("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"method":"key"}"#)
                .create();

            let printer = cfgd_core::test_helpers::test_printer();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, None, None, None, Some("alice"));
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("no SSH key found"),
                "expected no-SSH-key bail, got: {err}"
            );
            assert!(
                err.contains("--ssh-key") && err.contains("--gpg-key"),
                "expected help text naming both flags, got: {err}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_key_based_propagates_enroll_info_endpoint_failure() {
        // The /enroll/info pre-check returns 500. cmd_enroll's first
        // network call should surface as an anyhow error mentioning the
        // failed query — pins the early-fail contract before any key
        // detection / challenge plumbing runs.
        let tmp = tempfile::tempdir().unwrap();
        with_env("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(500)
                .with_body("internal error")
                .create();

            let printer = cfgd_core::test_helpers::test_printer();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, None, None, None, Some("alice"));
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("enrollment info") || err.contains("500"),
                "expected enroll_info failure surfaced, got: {err}"
            );
            // No credential written when the pre-check fails.
            assert!(!tmp.path().join("device-credential.json").exists());
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_token_path_persists_desired_config_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        with_env("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            // EnrollResponse with desiredConfig populated → exercises the
            // save_pending_server_config branch in finish_enrollment.
            let body = serde_json::json!({
                "status": "ok",
                "deviceId": "dev-abc-123",
                "apiKey": "key-xyz-789",
                "username": "alice",
                "team": null,
                "desiredConfig": {
                    "apiVersion": "cfgd.io/v1alpha1",
                    "kind": "Cfgd",
                    "metadata": {"name": "from-server"},
                    "spec": {}
                }
            })
            .to_string();
            let m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create();

            let (printer, buf) = Printer::for_test();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, Some("token"), None, None, Some("alice"));
            assert!(result.is_ok(), "cmd_enroll should succeed: {result:?}");
            m.assert();
            let captured = buf.lock().unwrap().clone();
            assert!(
                captured.contains("Server pushed desired config"),
                "expected desired-config notice in: {captured}"
            );
        });
    }
}

// ─── cmd_init --from <url> against local bare repo ──────────────────────────
//
// These tests drive `cmd_init` with `args.from = Some(<file:// URL>)` to
// exercise the clone-from-URL orchestration path. The `cli/init/source.rs`
// `is_git_source` predicate matches any value ending in `.git`, so a bare
// repo at `<tmp>/upstream.git` works without an env var gate.

#[cfg(unix)]
mod cmd_init_from_local_bare {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Initialise a bare upstream + a working source repo, commit a populated
    /// cfgd config tree (cfgd.yaml + profiles/default.yaml), and push the
    /// branch to the bare. Returns the bare path that ends in `.git` — so
    /// `is_git_source` treats `file://<bare>` as a clonable git source.
    fn make_bare_config_repo(tmp_root: &Path) -> PathBuf {
        let bare = tmp_root.join("upstream.git");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();

        let src = tmp_root.join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(
            src.join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: cloned-cfg\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("profiles")).unwrap();
        std::fs::write(
            src.join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new("cfgd.yaml")).unwrap();
        index.add_path(Path::new("profiles/default.yaml")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let bare_url = format!("file://{}", bare.display());
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        bare
    }

    /// Initialise a bare repo + a working source that has NO cfgd.yaml — the
    /// clone succeeds, but later orchestration (e.g. `--apply`) cannot find a
    /// config. Returns the bare path so `file://<bare>` is clonable.
    fn make_bare_empty_repo(tmp_root: &Path) -> PathBuf {
        let bare = tmp_root.join("empty.git");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();
        let src = tmp_root.join("empty-src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(src.join("README.md"), "no cfgd here").unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let bare_url = format!("file://{}", bare.display());
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        bare
    }

    #[test]
    fn cmd_init_from_url_clones_into_target_and_skips_workflow_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = make_bare_config_repo(tmp.path());
        let target = tmp.path().join("dst");
        let url = format!("file://{}", bare.display());

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let args = InitArgs {
            path: Some(target.to_str().unwrap()),
            from: Some(&url),
            branch: "master",
            name: None,
            apply: false,
            dry_run: false,
            yes: false,
            install_daemon: false,
            theme: None,
            apply_profile: None,
            apply_modules: &[],
        };
        cmd_init(&printer, &args).expect("cmd_init --from should succeed");

        // The cloned cfgd.yaml lands at the target.
        let cfg_yaml = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
        assert!(
            cfg_yaml.contains("cloned-cfg"),
            "cloned cfgd.yaml should preserve the upstream `name` field: {cfg_yaml}"
        );
        // The profile from the source is preserved.
        assert!(
            target.join("profiles").join("default.yaml").is_file(),
            "profile dir should have been cloned"
        );
        // `regenerate_workflow` runs only for scaffolded repos; with --from
        // present, the .github/workflows directory must NOT be auto-written.
        assert!(
            !target.join(".github").join("workflows").exists(),
            "workflow generation must be skipped for cloned repos"
        );
    }

    #[test]
    fn cmd_init_from_url_with_theme_injects_theme_into_cloned_config() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = make_bare_config_repo(tmp.path());
        let target = tmp.path().join("dst");
        let url = format!("file://{}", bare.display());

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let args = InitArgs {
            path: Some(target.to_str().unwrap()),
            from: Some(&url),
            branch: "master",
            name: None,
            apply: false,
            dry_run: false,
            yes: false,
            install_daemon: false,
            theme: Some("solarized-dark"),
            apply_profile: None,
            apply_modules: &[],
        };
        cmd_init(&printer, &args).expect("cmd_init --from --theme should succeed");

        let cfg_yaml = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
        assert!(
            cfg_yaml.contains("solarized-dark"),
            "cloned cfgd.yaml should have been rewritten with the theme: {cfg_yaml}"
        );
    }

    #[test]
    fn cmd_init_from_url_handles_repo_without_cfgd_yaml() {
        // Repo clones successfully but contains no cfgd.yaml — the function
        // should still return Ok (theme injection is gated on cfgd.yaml
        // existing). This exercises the `config_path.exists()` false-arm.
        let tmp = tempfile::tempdir().unwrap();
        let bare = make_bare_empty_repo(tmp.path());
        let target = tmp.path().join("empty-dst");
        let url = format!("file://{}", bare.display());

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let args = InitArgs {
            path: Some(target.to_str().unwrap()),
            from: Some(&url),
            branch: "master",
            name: None,
            apply: false,
            dry_run: false,
            yes: false,
            install_daemon: false,
            theme: Some("dark"),
            apply_profile: None,
            apply_modules: &[],
        };
        cmd_init(&printer, &args).expect("clone of empty repo should still return Ok");

        // README.md from the source is present.
        assert!(target.join("README.md").is_file());
        // No cfgd.yaml in source → none in target. Theme injection skipped.
        assert!(!target.join("cfgd.yaml").exists());
    }
}

// ─── cmd_init --apply orchestration ──────────────────────────────────────────
//
// Cover the apply branch of cmd_init that scaffolds and immediately applies
// the resulting plan. `CFGD_STATE_DIR` redirects the state store away from
// `~/.local/share/cfgd/`; `with_test_home_guard` keeps the module-cache lookup
// off real disk.

#[cfg(unix)]
mod cmd_init_apply_orchestration {
    use super::*;
    use serial_test::serial;

    /// Set CFGD_STATE_DIR for the duration of a closure (process-wide; use
    /// with `#[serial]`).
    fn with_state_dir<F: FnOnce()>(dir: &std::path::Path, f: F) {
        // SAFETY: serialized via #[serial].
        unsafe {
            std::env::set_var("CFGD_STATE_DIR", dir);
        }
        f();
        // SAFETY: serialized via #[serial].
        unsafe {
            std::env::remove_var("CFGD_STATE_DIR");
        }
    }

    #[test]
    #[serial]
    fn cmd_init_with_apply_on_freshly_scaffolded_dir_bails_when_no_profile_exists() {
        // Fresh scaffold creates cfgd.yaml without an active profile and no
        // profile files in profiles/. With --apply set, cmd_init delegates to
        // pick_profile which bails — a real orchestration-state contract:
        // scaffold + apply only works after at least one profile is created.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let target = tmp.path().join("config");
        std::fs::create_dir_all(&target).unwrap();
        let state_dir = tmp.path().join("state");

        let (printer, _buf) = Printer::for_test();
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: None,
                branch: "master",
                name: Some("apply-test"),
                apply: true,
                dry_run: false,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: None,
                apply_modules: &[],
            };
            let err = cmd_init(&printer, &args)
                .expect_err("scaffold+apply without profile should surface pick_profile bail");
            let msg = err.to_string();
            assert!(
                msg.contains("No profiles found"),
                "error should explain why apply cannot proceed: {msg}"
            );
        });

        // Scaffold still landed before the apply bail.
        assert!(target.join("cfgd.yaml").exists());
        assert!(target.join("profiles").is_dir());
    }

    #[test]
    #[serial]
    fn cmd_init_with_apply_module_unknown_name_bails() {
        // --apply-module on a name that doesn't resolve to a local or remote
        // module triggers the validation bail before reconciler.plan().
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let target = tmp.path().join("config");
        std::fs::create_dir_all(&target).unwrap();
        let state_dir = tmp.path().join("state");

        let (printer, _buf) = Printer::for_test();
        with_state_dir(&state_dir, || {
            let modules = vec!["ghost-module".to_string()];
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: None,
                branch: "master",
                name: Some("apply-modules-test"),
                apply: false,
                dry_run: false,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: None,
                apply_modules: &modules,
            };
            let err = cmd_init(&printer, &args)
                .expect_err("--apply-module on unknown module should bail");
            let msg = err.to_string();
            assert!(
                msg.contains("ghost-module") && msg.contains("not found"),
                "error should explain that ghost-module isn't found: {msg}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_init_with_apply_profile_unknown_name_bails() {
        // --apply-profile pointing at a profile that doesn't exist on disk
        // surfaces a clear error from the profile-validation arm.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let target = tmp.path().join("config");
        std::fs::create_dir_all(&target).unwrap();
        let state_dir = tmp.path().join("state");

        let (printer, _buf) = Printer::for_test();
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: None,
                branch: "master",
                name: Some("apply-profile-test"),
                apply: false,
                dry_run: false,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: Some("missing-profile"),
                apply_modules: &[],
            };
            let err = cmd_init(&printer, &args)
                .expect_err("--apply-profile on missing profile should bail");
            let msg = err.to_string();
            assert!(
                msg.contains("missing-profile") && msg.contains("not found"),
                "error should reference missing-profile: {msg}"
            );
        });
    }

    /// Stage a working source repo + bare remote at `<tmp>/upstream.git` whose
    /// committed tree has `cfgd.yaml` + `profiles/default.yaml` (empty profile,
    /// no inherits / packages / modules). Mirror of the helper inside the
    /// sibling `cmd_init_from_local_bare` module — duplicated locally because
    /// Rust visibility forbids sharing a sibling-private helper across two
    /// `#[cfg(unix)] mod` blocks without exposing it crate-wide.
    fn make_bare_config_repo_with_default(tmp_root: &std::path::Path) -> std::path::PathBuf {
        let bare = tmp_root.join("upstream.git");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();

        let src = tmp_root.join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(
            src.join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: cloned-cfg\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("profiles")).unwrap();
        std::fs::write(
            src.join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
        index
            .add_path(std::path::Path::new("profiles/default.yaml"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let bare_url = format!("file://{}", bare.display());
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        bare
    }

    #[test]
    #[serial]
    fn cmd_init_from_url_with_apply_drives_profile_branch_via_dry_run() {
        // `--from <bare> --apply --dry-run` exercises the profile-based apply
        // arm end-to-end:
        //   clone → resolve_profile → build_registry → reconciler.plan() →
        //   apply_plan(dry_run=true) → "Nothing to do" early return.
        // The cloned profile is empty, so plan.total_actions() == 0 and we
        // hit the apply_plan zero-action branch.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let bare = make_bare_config_repo_with_default(tmp.path());
        let target = tmp.path().join("dst");
        let state_dir = tmp.path().join("state");
        let url = format!("file://{}", bare.display());

        let (printer, buf) = Printer::for_test();
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: Some(&url),
                branch: "master",
                name: None,
                apply: true,
                dry_run: true,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: None,
                apply_modules: &[],
            };
            cmd_init(&printer, &args).expect("--from + --apply --dry-run should succeed");
        });

        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Applying Configuration"),
            "should enter the profile-based apply branch: {out}"
        );
        // Empty profile -> 0 planned actions -> the apply_plan no-op success.
        assert!(
            out.contains("Nothing to do") || out.contains("already configured"),
            "0-action plan should reach the 'Nothing to do' early return: {out}"
        );
    }

    #[test]
    #[serial]
    fn cmd_init_from_url_with_apply_profile_flag_persists_profile_to_cfgd_yaml() {
        // `--apply-profile default` on a cloned repo should:
        //   1. validate profiles/default.yaml exists
        //   2. write spec.profile=default into cfgd.yaml (even though the
        //      clone already has it — exercises the mutate-on-set arm)
        //   3. run the profile-based apply (dry_run → zero-action exit)
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let bare = make_bare_config_repo_with_default(tmp.path());
        let target = tmp.path().join("dst");
        let state_dir = tmp.path().join("state");
        let url = format!("file://{}", bare.display());

        let (printer, buf) = Printer::for_test();
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: Some(&url),
                branch: "master",
                name: None,
                apply: false,
                dry_run: true,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: Some("default"),
                apply_modules: &[],
            };
            cmd_init(&printer, &args).expect("--apply-profile default should drive apply branch");
        });

        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Set active profile: default"),
            "validated --apply-profile branch should announce profile selection: {out}"
        );
        let cfg_yaml = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
        assert!(
            cfg_yaml.contains("profile: default"),
            "spec.profile should be persisted to cfgd.yaml: {cfg_yaml}"
        );
    }

    #[test]
    #[serial]
    fn cmd_init_with_apply_module_drives_module_only_branch_via_dry_run() {
        // Build a bare repo where the cloned tree carries a local module —
        // `modules/sample/module.yaml`. Then call cmd_init --from <url>
        // --apply-module sample --dry-run. This exercises the module-only
        // arm of cmd_init's apply orchestration (lines 107-159): loads
        // all modules, validates the requested name, builds an empty
        // ResolvedProfile, resolves modules, calls reconciler.plan with
        // ReconcileContext::Apply, and apply_plan bails at "Nothing to do"
        // because the sample module declares no packages or files.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        // Build the bare repo with cfgd.yaml + modules/sample/module.yaml.
        let bare = tmp.path().join("upstream.git");
        let _ = git2::Repository::init_bare(&bare).unwrap();
        let src = tmp.path().join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(
            src.join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: with-module\nspec:\n  theme: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("modules").join("sample")).unwrap();
        std::fs::write(
            src.join("modules").join("sample").join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: sample\nspec: {}\n",
        )
        .unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
        index
            .add_path(std::path::Path::new("modules/sample/module.yaml"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let url = format!("file://{}", bare.display());
        let mut remote = src_repo.remote("origin", &url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();

        let target = tmp.path().join("dst");
        let state_dir = tmp.path().join("state");
        let modules = vec!["sample".to_string()];
        let (printer, buf) = Printer::for_test();
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: Some(&url),
                branch: "master",
                name: None,
                apply: false,
                dry_run: true,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: None,
                apply_modules: &modules,
            };
            cmd_init(&printer, &args).expect("--apply-module drives module-only branch");
        });

        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Applying Modules"),
            "should hit the Applying Modules header in the module-only arm: {out}"
        );
        // Sample module declares no packages/files → 0 actions → "Nothing to do".
        assert!(
            out.contains("Nothing to do") || out.contains("already configured"),
            "0-action module plan should hit the apply_plan no-op early return: {out}"
        );
    }

    #[test]
    #[serial]
    fn cmd_init_from_url_pick_profile_uses_only_available_when_spec_profile_blank() {
        // Clone a config without `spec.profile`, then drive apply with
        // dry_run=true. pick_profile sees a single profile (`default.yaml`)
        // and selects it non-interactively — exercises the `names.len() == 1`
        // happy-path of pick_profile.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        // Build a bare repo whose cfgd.yaml has NO spec.profile.
        let bare = tmp.path().join("upstream.git");
        let _b = git2::Repository::init_bare(&bare).unwrap();
        let src = tmp.path().join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(
            src.join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: profileless\nspec:\n  theme: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("profiles")).unwrap();
        std::fs::write(
            src.join("profiles").join("only.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: only\nspec: {}\n",
        )
        .unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
        index
            .add_path(std::path::Path::new("profiles/only.yaml"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let url = format!("file://{}", bare.display());
        let mut remote = src_repo.remote("origin", &url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();

        let target = tmp.path().join("dst");
        let state_dir = tmp.path().join("state");
        let (printer, buf) = Printer::for_test();
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: Some(&url),
                branch: "master",
                name: None,
                apply: true,
                dry_run: true,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: None,
                apply_modules: &[],
            };
            cmd_init(&printer, &args).expect("pick_profile should select the sole profile");
        });

        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Using only available profile: only"),
            "pick_profile single-profile fast path should be exercised: {out}"
        );
    }

    /// Same shape as `make_bare_config_repo_with_default`, but the cloned tree
    /// also carries `modules/<mod>/module.yaml` so a `--apply-module <mod>`
    /// invocation can pull it into the profile-based plan.
    fn make_bare_config_repo_with_default_and_module(
        tmp_root: &std::path::Path,
        mod_name: &str,
    ) -> std::path::PathBuf {
        let bare = tmp_root.join("upstream.git");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();

        let src = tmp_root.join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(
            src.join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: cloned-cfg\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("profiles")).unwrap();
        std::fs::write(
            src.join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("modules").join(mod_name)).unwrap();
        let module_rel = format!("modules/{mod_name}/module.yaml");
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {mod_name}\nspec: {{}}\n"
        );
        std::fs::write(src.join(&module_rel), module_yaml).unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
        index
            .add_path(std::path::Path::new("profiles/default.yaml"))
            .unwrap();
        index.add_path(std::path::Path::new(&module_rel)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let bare_url = format!("file://{}", bare.display());
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        bare
    }

    #[test]
    #[serial]
    fn cmd_init_combined_apply_profile_and_apply_module_merges_module_into_profile_plan() {
        // `--apply-profile default --apply-module extra` drives the
        // profile-based branch (lines 162-174 in cmd_init: set spec.profile,
        // load it) AND the apply-modules merge (lines 195-198: extend
        // module_names with names passed in --apply-module that the profile
        // doesn't already list). With dry_run=true, the reconciler plan
        // bails at "Nothing to do" since the module declares nothing — what
        // we're pinning is the *control flow*: profile validated + persisted
        // + module name carried through into the plan.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let bare = make_bare_config_repo_with_default_and_module(tmp.path(), "extra");
        let target = tmp.path().join("dst");
        let state_dir = tmp.path().join("state");
        let url = format!("file://{}", bare.display());

        let (printer, buf) = Printer::for_test();
        let modules = vec!["extra".to_string()];
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: Some(&url),
                branch: "master",
                name: None,
                apply: false,
                dry_run: true,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: Some("default"),
                apply_modules: &modules,
            };
            cmd_init(&printer, &args)
                .expect("--apply-profile + --apply-module should walk the combined arm");
        });

        let out = buf.lock().unwrap().clone();
        // Profile-validation arm fires.
        assert!(
            out.contains("Set active profile: default"),
            "combined arm should still announce profile selection: {out}"
        );
        // Profile-based apply header (not the module-only one).
        assert!(
            out.contains("Applying Configuration"),
            "combined arm should drive the profile-based apply branch, not module-only: {out}"
        );
        // Profile + module both reach the reconciler — empty module + empty
        // profile yield 0 actions → apply_plan's no-op path.
        assert!(
            out.contains("Nothing to do") || out.contains("already configured"),
            "0-action combined plan should hit the apply_plan no-op early return: {out}"
        );
    }

    #[test]
    #[serial]
    fn cmd_init_combined_apply_profile_and_unknown_apply_module_bails_in_profile_branch() {
        // Profile-based apply branch (apply_profile=Some) with an
        // --apply-module that doesn't exist on disk. The combined arm
        // resolves the profile first (writes spec.profile), then validates
        // every name in args.apply_modules against load_all_modules and
        // bails at lines 207-211. This pins the "validate before plan"
        // contract on the profile-based path (mirror of the module-only
        // bail test).
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let bare = make_bare_config_repo_with_default(tmp.path());
        let target = tmp.path().join("dst");
        let state_dir = tmp.path().join("state");
        let url = format!("file://{}", bare.display());

        let (printer, _buf) = Printer::for_test();
        let modules = vec!["ghost-extra".to_string()];
        with_state_dir(&state_dir, || {
            let args = InitArgs {
                path: Some(target.to_str().unwrap()),
                from: Some(&url),
                branch: "master",
                name: None,
                apply: false,
                dry_run: true,
                yes: true,
                install_daemon: false,
                theme: None,
                apply_profile: Some("default"),
                apply_modules: &modules,
            };
            let err = cmd_init(&printer, &args).expect_err(
                "profile-based apply with unknown --apply-module should bail before plan",
            );
            let msg = err.to_string();
            assert!(
                msg.contains("ghost-extra") && msg.contains("not found"),
                "error should name the missing module: {msg}"
            );
        });
    }
}
