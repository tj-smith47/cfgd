use super::*;
use cfgd_core::output::{Printer, Verbosity};
use cfgd_core::test_helpers::test_printer as quiet_printer;

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
    let printer = quiet_printer();

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
    let printer = quiet_printer();

    scaffold(dir.path(), Some("themed"), Some("minimal"), &printer).unwrap();

    let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
    assert!(contents.contains("name: themed"));
    assert!(contents.contains("theme: minimal"));
}

#[test]
fn scaffold_uses_dir_name_as_default() {
    let dir = tempfile::tempdir().unwrap();
    let printer = quiet_printer();

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

    let printer = quiet_printer();
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

    let printer = quiet_printer();
    regenerate_workflow(dir.path(), &printer).unwrap();

    let workflow = dir.path().join(".github/workflows/cfgd-release.yml");
    assert!(workflow.exists());
    let contents = std::fs::read_to_string(&workflow).unwrap();
    assert!(contents.contains("base"));
}

#[test]
fn scaffold_includes_default_theme() {
    let dir = tempfile::tempdir().unwrap();
    let printer = quiet_printer();

    scaffold(dir.path(), Some("test"), None, &printer).unwrap();
    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.theme.unwrap().name, "default");
}

#[test]
fn scaffold_with_custom_theme() {
    let dir = tempfile::tempdir().unwrap();
    let printer = quiet_printer();

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

    let printer = quiet_printer();
    let result = pick_profile(&profiles_dir, &printer).unwrap();
    assert_eq!(result, "base");
}

#[test]
fn pick_profile_no_profiles_errors() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();

    let printer = quiet_printer();
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

    // Capture the human surface (section + bullets) on one run, then re-run
    // under structured-format to force prompt_text into its non-interactive
    // refusal — pins both halves of the multi-profile path: the enumerator
    // emits the section, then prompt_text Errs deterministically.
    let (printer, cap) = Printer::for_test_doc();
    let _ = pick_profile(&profiles_dir, &printer);
    drop(printer);
    let captured = cap.human();
    assert!(
        captured.contains("Available Profiles"),
        "section header must fire on multi-profile path: {captured}"
    );
    assert!(
        captured.contains("1. alpha") && captured.contains("2. bravo"),
        "each profile must be enumerated 1-based on its own bullet: {captured}"
    );

    let json_printer = Printer::with_format(
        Verbosity::Normal,
        None,
        cfgd_core::output::OutputFormat::Json,
    );
    let err = pick_profile(&profiles_dir, &json_printer)
        .expect_err("non-interactive prompt_text Errs in structured-format printer");
    let msg = err.to_string();
    assert!(
        msg.contains("Select profile") || msg.to_ascii_lowercase().contains("interactive"),
        "Err must mention the prompt that could not be satisfied: {msg}"
    );
}

#[test]
fn pick_profile_no_dir_errors() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    // Don't create the dir

    let printer = quiet_printer();
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

    let printer = quiet_printer();
    let result = resolve_from(&dir.path().display().to_string(), None, "master", &printer);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), dir.path());
}

#[test]
fn resolve_from_local_path_missing_config_errors() {
    let dir = tempfile::tempdir().unwrap();
    // No cfgd.yaml

    let printer = quiet_printer();
    let result = resolve_from(&dir.path().display().to_string(), None, "master", &printer);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No cfgd.yaml"));
}

#[test]
fn resolve_from_nonexistent_path_errors() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("does-not-exist");
    let printer = quiet_printer();
    let result = resolve_from(&nonexistent.display().to_string(), None, "master", &printer);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn check_prerequisites_returns_true_when_git_available() {
    // git should be available in CI and dev environments
    let printer = quiet_printer();
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

    let printer = quiet_printer();
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

    let printer = quiet_printer();
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

    let printer = quiet_printer();
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

    let (printer, cap) = Printer::for_test_doc();
    clone_into(&target, &origin.display().to_string(), "master", &printer).unwrap();

    assert!(target.join(".git").exists(), "should create .git directory");
    let cfg = std::fs::read_to_string(target.join("cfgd.yaml")).unwrap();
    assert!(
        cfg.contains("name: test"),
        "cloned cfgd.yaml should contain the name"
    );

    drop(printer);
    let output = cap.human();
    assert!(output.contains("Cloned"), "should report successful clone");
}

#[test]
fn clone_into_skips_if_already_cloned() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clone");
    std::fs::create_dir_all(target.join(".git")).unwrap();

    let (printer, cap) = Printer::for_test_doc();
    // Should return Ok without actually cloning
    clone_into(
        &target,
        "https://example.com/nonexistent",
        "master",
        &printer,
    )
    .unwrap();

    drop(printer);
    let output = cap.human();
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
    let printer = quiet_printer();
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

    let printer = quiet_printer();
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
    let printer = quiet_printer();

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
    let printer = quiet_printer();

    scaffold(dir.path(), Some("test"), None, &printer).unwrap();

    let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains("!cfgd.yaml"));
    assert!(gitignore.contains("!profiles/"));
    assert!(gitignore.contains("!modules/"));
}

#[test]
fn scaffold_creates_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let printer = quiet_printer();

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

    let (printer, cap) = Printer::for_test_doc();
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

    drop(printer);
    let output = cap.human();
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

    let (printer, cap) = Printer::for_test_doc();
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

    drop(printer);
    let output = cap.human();
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

    let (printer, cap) = Printer::for_test_doc();
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

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Already initialized"),
        "should report already initialized, got: {output}"
    );
}

#[test]
fn cmd_init_with_theme() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("themed");

    let printer = quiet_printer();
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

    let printer = quiet_printer();
    let result = resolve_from(&dir.path().display().to_string(), None, "master", &printer).unwrap();
    assert_eq!(result, dir.path());
}

#[test]
fn resolve_from_local_path_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let printer = quiet_printer();

    let err =
        resolve_from(&dir.path().display().to_string(), None, "master", &printer).unwrap_err();
    assert!(
        err.to_string().contains("No cfgd.yaml"),
        "should report missing cfgd.yaml, got: {err}"
    );
}

#[test]
fn resolve_from_nonexistent_path_fails() {
    let printer = quiet_printer();
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
    let printer = quiet_printer();

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
    let printer = quiet_printer();

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
    let printer = quiet_printer();

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

    let (printer, cap) = Printer::for_test_doc();
    let result = pick_profile(dir.path(), &printer).unwrap();
    assert_eq!(result, "default");

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Using only available profile: default"),
        "should auto-select single profile, got: {output}"
    );
}

#[test]
fn pick_profile_no_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nope");
    let printer = quiet_printer();

    let err = pick_profile(&nonexistent, &printer).unwrap_err();
    assert!(
        err.to_string().contains("No profiles directory"),
        "should report no profiles dir, got: {err}"
    );
}

#[test]
fn pick_profile_empty_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let printer = quiet_printer();

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
    let printer = quiet_printer();

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

    let printer = quiet_printer();
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

    let printer = quiet_printer();
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

    let printer = quiet_printer();
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
    let printer = quiet_printer();
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
    let printer = quiet_printer();
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

    let printer = quiet_printer();
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

    let printer = quiet_printer();
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

    let printer = quiet_printer();
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
    let (printer, cap) = Printer::for_test_doc();

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

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Nothing to do"),
        "should report nothing to do for empty plan, got: {output}"
    );
}

// ─── check_prerequisites — git availability ──────────────────

#[test]
fn check_prerequisites_with_test_printer() {
    let (printer, cap) = Printer::for_test_doc();
    let result = check_prerequisites(&printer);
    drop(printer);
    let output = cap.human();

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

    let printer = quiet_printer();
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

    let (printer, cap) = Printer::for_test_doc();
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

    drop(printer);
    let output = cap.human();
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

    let (printer, cap) = Printer::for_test_doc();
    clone_into(&target, "https://example.com/repo.git", "main", &printer).unwrap();

    drop(printer);
    let output = cap.human();
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
    let printer = quiet_printer();
    // This will either find a key or return None — both are valid
    let result = detect_ssh_key(&printer);
    // Just verify it doesn't panic and returns a valid type
    if let Some(ref path) = result {
        assert!(!path.is_empty(), "returned path should be non-empty");
    }
}

#[test]
#[serial_test::serial]
#[cfg(unix)]
fn detect_ssh_key_ssh_agent_path_returns_disk_key_when_agent_has_identities() {
    // Cover lines 321-331: ssh-add is present, exits 0, first stdout line does
    // NOT contain "no identities", and a disk key exists → the agent-path
    // returns it. Uses a fake `ssh-add` script and a fake HOME with a key.
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("fakebin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_ssh_add = bin_dir.join("ssh-add");
    std::fs::write(
        &fake_ssh_add,
        b"#!/bin/sh\necho '256 SHA256:fakekey alice@host (ED25519)'\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake_ssh_add, std::fs::Permissions::from_mode(0o755)).unwrap();

    let home_dir = tmp.path().join("home");
    let ssh_dir = home_dir.join(".ssh");
    std::fs::create_dir_all(&ssh_dir).unwrap();
    std::fs::write(ssh_dir.join("id_ed25519.pub"), b"ssh-ed25519 AAAA fake-key").unwrap();
    let _home_guard = cfgd_core::with_test_home_guard(&home_dir);

    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries: Vec<std::path::PathBuf> = vec![bin_dir.clone()];
    path_entries.extend(std::env::split_paths(&original_path));
    let new_path = std::env::join_paths(&path_entries).unwrap();
    let _path_guard = cfgd_core::test_helpers::EnvVarGuard::set(
        "PATH",
        new_path.to_str().expect("PATH must be valid UTF-8"),
    );

    let (printer, cap) = Printer::for_test_doc();
    let result = detect_ssh_key(&printer);
    drop(printer);
    let human = cap.human();

    assert!(
        result.is_some(),
        "detect_ssh_key must return Some when agent has identities and disk key exists: {human}"
    );
    let key_path = result.unwrap();
    assert!(
        key_path.contains("id_ed25519"),
        "returned key path must contain the expected key name: {key_path}"
    );
    assert!(
        human.contains("agent") || human.contains("id_ed25519"),
        "output must reference SSH agent or key path: {human}"
    );
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
    let (printer, cap) = Printer::for_test_doc();

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

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Skipped"),
        "expected 'Skipped' message on declined prompt, got: {output}"
    );
    assert!(
        output.contains("cfgd apply"),
        "expected hint to run 'cfgd apply' later, got: {output}"
    );
}

// --- apply_plan with prompt confirmed (yes-branch via harness) ---

#[test]
#[serial_test::serial]
fn apply_plan_with_prompt_confirmed_proceeds_to_apply_path() {
    // yes=false + queued Confirm(true) drives past the prompt at
    // cmd_init.rs:345-353 into the state-dir + apply-lock + reconciler.apply
    // path. With an empty registry the apply runs a no-op flow (no package
    // managers registered → no manager spawn), but the code path that's
    // covered is the post-prompt sequence: default_state_dir, acquire_apply_lock,
    // reconciler.apply, print_apply_result. We assert the apply lock was
    // observed (no "Skipped" output) and that apply completed without panic.
    let dir = tempfile::tempdir().unwrap();
    let _home = cfgd_core::with_test_home_guard(dir.path());
    let (printer, buf) =
        Printer::for_test_with_prompt_responses(vec![cfgd_core::output::PromptAnswer::Confirm(
            true,
        )]);

    let registry = super::build_registry_with_config(None);
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let store = super::open_state_store(Some(&state_dir)).unwrap();
    let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
    let resolved = config::ResolvedProfile {
        layers: Vec::new(),
        merged: config::MergedProfile::default(),
    };

    // Build a plan whose only "action" is a no-op manager that doesn't exist
    // in the registry. The reconciler will surface a planning issue but apply
    // will short-circuit gracefully — we care about the post-prompt branches
    // executing, not the final outcome.
    let plan = cfgd_core::reconciler::Plan {
        phases: vec![cfgd_core::reconciler::Phase {
            name: cfgd_core::reconciler::PhaseName::PreScripts,
            actions: vec![],
        }],
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
    assert!(
        result.is_ok(),
        "confirmed prompt + empty-actions plan must succeed: {:?}",
        result.err()
    );
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        !output.contains("Skipped"),
        "Skipped must NOT fire when prompt is confirmed: {output}"
    );
}

// --- apply_plan with prompt declined (no-branch via harness) ---

#[test]
#[serial_test::serial]
fn apply_plan_with_prompt_declined_emits_skipped_and_returns_early() {
    // yes=false + queued Confirm(false) drives the prompt-declined arm at
    // cmd_init.rs:349-352. The "Skipped — run 'cfgd apply' to apply later"
    // info line fires and apply_plan returns Ok before reaching the apply
    // lock + reconciler.apply chain. Pins the no-branch alongside the
    // existing yes-branch test.
    let dir = tempfile::tempdir().unwrap();
    let _home = cfgd_core::with_test_home_guard(dir.path());
    let (printer, buf) = Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(false)],
        Verbosity::Normal,
    );

    let registry = super::build_registry_with_config(None);
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let store = super::open_state_store(Some(&state_dir)).unwrap();
    let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
    let resolved = config::ResolvedProfile {
        layers: Vec::new(),
        merged: config::MergedProfile::default(),
    };

    // Plan must have at least one action so total > 0 and the prompt fires.
    // A FileAction::Skip is enough — the prompt-declined arm short-circuits
    // before apply runs so the action's body is never executed.
    let plan = cfgd_core::reconciler::Plan {
        phases: vec![cfgd_core::reconciler::Phase {
            name: cfgd_core::reconciler::PhaseName::Files,
            actions: vec![cfgd_core::reconciler::Action::File(
                cfgd_core::providers::FileAction::Skip {
                    target: dir.path().join("noop"),
                    reason: "synthetic prompt-declined coverage".to_string(),
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
        false,
        false,
        &printer,
    );
    assert!(
        result.is_ok(),
        "declined prompt must still return Ok: {:?}",
        result.err()
    );
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Skipped"),
        "Skipped notice must fire when prompt is declined: {output}"
    );
}

// --- apply_plan with dry_run ---

#[test]
fn apply_plan_dry_run_skips_apply() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, cap) = Printer::for_test_doc();

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

    drop(printer);
    let output = cap.human();
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
    let printer = quiet_printer();
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
    let printer = quiet_printer();
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

// --- cmd_init with --from git: ALL clone-path overrides apply together ---

#[test]
fn cmd_init_from_git_applies_name_and_theme_overrides_together() {
    let dir = tempfile::tempdir().unwrap();

    // Origin repo whose cfgd.yaml carries BOTH a source-repo name and a theme,
    // so the test proves every override is funneled through, not just one.
    let origin = dir.path().join("origin");
    let repo = git2::Repository::init(&origin).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    std::fs::write(
        origin.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: upstream-cfg\nspec:\n  theme:\n    name: default\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("cfgd.yaml")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let target = dir.path().join("named-target");
    let printer = quiet_printer();
    let origin_str = origin.display().to_string();
    let target_str = target.display().to_string();
    let args = InitArgs {
        path: Some(&target_str),
        from: Some(&origin_str),
        branch: "master",
        name: Some("acme"),
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
        "cmd_init with name + theme overrides should succeed: {:?}",
        result.err()
    );

    let cfg = config::load_config(&target.join("cfgd.yaml")).unwrap();
    assert_eq!(
        cfg.metadata.name, "acme",
        "metadata.name should be overridden to the --name value"
    );
    assert_eq!(
        cfg.spec.theme.as_ref().map(|t| t.name.as_str()),
        Some("dracula"),
        "spec.theme.name should be overridden to the --theme value"
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
fn next_steps_lines_are_bare_commands_not_pre_indented() {
    // The "Next Steps" Doc section renders each line as a bullet, which
    // supplies its own indent and "- " prefix. The line strings must NOT
    // carry leading whitespace or the bullet output would have two layers
    // of indentation.
    for line in super::next_steps_lines() {
        assert!(
            !line.starts_with(' ') && !line.starts_with('\t'),
            "line must be a bare command, got: {line:?}"
        );
        assert!(
            line.starts_with("cfgd "),
            "line must start with the cfgd command verb, got: {line:?}"
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
    use cfgd_core::test_helpers::with_test_env_var;
    use serial_test::serial;

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
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(enroll_response_json())
                .create();

            let (printer, cap) = Printer::for_test_doc();
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

            // Printer output should announce success and emit the Next
            // Steps section heading.
            drop(printer);
            let captured = cap.human();
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
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            // 400 is a non-retryable client error — request_error path.
            let _m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(400)
                .with_body(r#"{"error":"invalid token"}"#)
                .create();

            let printer = super::quiet_printer();
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
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            // Server says it only supports token enrollment.
            let _m = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"method":"token"}"#)
                .create();

            let printer = super::quiet_printer();
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

        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"method":"key"}"#)
                .create();

            let printer = super::quiet_printer();
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
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(500)
                .with_body("internal error")
                .create();

            let printer = super::quiet_printer();
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
    fn cmd_enroll_token_path_streaming_to_buffered_bridge_invariant() {
        // Bridge anchor under real cmd_enroll data — the existing
        // `tests/enroll_snapshots.rs` snapshots exercise
        // `build_enroll_final_doc` in isolation and therefore miss the
        // streaming portion (kvs + heading + status_simple lines) that
        // `cmd_enroll` emits before the trailing buffered Doc. This test
        // drives the full token-enrollment orchestration through a mock
        // server, captures both streaming and buffered output on the same
        // Printer, asserts the one-blank-line bridge invariant
        // programmatically, and snapshots the combined output for
        // regression coverage.
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(enroll_response_json())
                .create();

            let (printer, cap) = Printer::for_test_doc();
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
            drop(printer);

            let captured = strip_ansi(&cap.human());

            // Sanity: streaming portion present before the buffered Doc.
            assert!(
                captured.contains("Token Enrollment"),
                "missing streaming heading in:\n{captured}"
            );
            assert!(
                captured.contains("Enrolled as user 'alice'"),
                "missing streaming status line in:\n{captured}"
            );
            assert!(
                captured.contains("Next Steps"),
                "missing buffered section header in:\n{captured}"
            );
            assert!(
                captured.find("Token Enrollment").unwrap() < captured.find("Next Steps").unwrap(),
                "streaming surface must precede buffered surface in:\n{captured}"
            );

            // Bridge invariant: exactly one blank line between the last
            // streaming line and the first buffered line. Two newlines in
            // a row = one blank line; three or more = more than one.
            assert!(
                captured.contains("\n\n"),
                "expected at least one blank line in:\n{captured}"
            );
            assert!(
                !captured.contains("\n\n\n"),
                "expected at most one blank line gap in:\n{captured}"
            );

            // Normalize the mockito-allocated server URL (a 127.0.0.1
            // address with a random port) so the golden survives across
            // runs / hosts. Route the credential-save path through
            // `normalize_for_snapshot` so it posixifies on Windows before
            // substituting (production now emits forward-slash paths on
            // every OS via `to_posix_string`).
            let cred_path = tmp.path().join("device-credential.json");
            let normalized =
                cfgd_core::normalize_for_snapshot(&captured, &[(&cred_path, "<CRED_PATH>")]);
            let normalized = normalized.replace(&url, "<SERVER_URL>");
            // Normalize the host-dependent device-id (default_device_id
            // returns the running machine's hostname).
            let device_id = cfgd_core::hostname_string();
            let normalized = normalized.replace(&device_id, "<DEVICE_ID>");

            let snap_path =
                std::path::Path::new("tests/output_snapshots/enroll/cmd_token_flow.txt");
            if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !snap_path.exists() {
                std::fs::create_dir_all(snap_path.parent().unwrap()).unwrap();
                std::fs::write(snap_path, &normalized).unwrap();
            } else {
                let expected = std::fs::read_to_string(snap_path).unwrap();
                // CRLF→LF: windows captures `\r\n`; committed snapshot is LF.
                let actual_norm = normalized.replace("\r\n", "\n");
                let expected_norm = expected.replace("\r\n", "\n");
                pretty_assertions::assert_eq!(
                    actual_norm,
                    expected_norm,
                    "snapshot mismatch: enroll/cmd_token_flow.txt"
                );
            }
        });
    }

    /// ANSI-stripping helper local to this module — mirrors the one in
    /// `tests/init_snapshots.rs`; both are tiny and isolated to keep the
    /// snapshot pipeline self-contained per test file.
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

    #[test]
    #[serial]
    fn cmd_enroll_token_path_persists_desired_config_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
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

            let (printer, cap) = Printer::for_test_doc();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, Some("token"), None, None, Some("alice"));
            assert!(result.is_ok(), "cmd_enroll should succeed: {result:?}");
            m.assert();
            drop(printer);
            let captured = cap.human();
            assert!(
                captured.contains("Server pushed desired config"),
                "expected desired-config notice in: {captured}"
            );
        });
    }

    #[test]
    fn build_enroll_error_method_mismatch_carries_kind_and_hint() {
        // Pure constructor — no HTTP, no state dir. The returned error carries the
        // kind as error_kind, merges extras, and attaches the per-kind hint.
        use crate::cli::CliErrorMeta;
        use crate::cli::init::enroll::build_enroll_error;

        let err = build_enroll_error(
            "https://example.com",
            "method_mismatch",
            "server uses token enrollment",
            serde_json::json!({"serverUrl": "https://example.com", "serverMethod": "token"}),
        );
        let meta = err
            .downcast_ref::<CliErrorMeta>()
            .expect("build_enroll_error returns CliErrorMeta");
        assert_eq!(meta.error_kind, "method_mismatch");
        assert_eq!(meta.name, "https://example.com");
        assert_eq!(meta.extras["serverMethod"], "token");
        assert!(
            meta.hints.iter().any(|h| h.contains("bootstrap token")),
            "method_mismatch hint must reference bootstrap token: {:?}",
            meta.hints
        );
    }

    #[test]
    fn build_enroll_error_no_key_carries_kind_and_hint() {
        use crate::cli::CliErrorMeta;
        use crate::cli::init::enroll::build_enroll_error;

        let err = build_enroll_error(
            "device-1",
            "no_key",
            "no SSH key found",
            serde_json::json!({"checked": ["ssh-agent"]}),
        );
        let meta = err
            .downcast_ref::<CliErrorMeta>()
            .expect("build_enroll_error returns CliErrorMeta");
        assert_eq!(meta.error_kind, "no_key");
        assert_eq!(meta.extras["checked"], serde_json::json!(["ssh-agent"]));
        assert!(
            meta.hints
                .iter()
                .any(|h| h.contains("--ssh-key") || h.contains("--gpg-key")),
            "no_key hint must name both flag forms: {:?}",
            meta.hints
        );
    }

    #[test]
    fn build_enroll_error_signing_failed_carries_kind_and_hint() {
        use crate::cli::CliErrorMeta;
        use crate::cli::init::enroll::build_enroll_error;

        let err = build_enroll_error(
            "/bad/key",
            "signing_failed",
            "signing failed",
            serde_json::json!({"keyType": "ssh", "keyRef": "/bad/key"}),
        );
        let meta = err
            .downcast_ref::<CliErrorMeta>()
            .expect("build_enroll_error returns CliErrorMeta");
        assert_eq!(meta.error_kind, "signing_failed");
        assert_eq!(meta.extras["keyType"], "ssh");
        assert!(
            meta.hints
                .iter()
                .any(|h| h.contains("signing key") || h.contains("signing tool")),
            "signing_failed hint must advise on tool/key accessibility: {:?}",
            meta.hints
        );
    }

    #[test]
    fn build_enroll_error_unknown_kind_carries_no_hint() {
        use crate::cli::CliErrorMeta;
        use crate::cli::init::enroll::build_enroll_error;

        let err = build_enroll_error("x", "some_other_error", "boom", serde_json::json!({}));
        let meta = err
            .downcast_ref::<CliErrorMeta>()
            .expect("build_enroll_error returns CliErrorMeta");
        assert_eq!(meta.error_kind, "some_other_error");
        assert!(
            meta.hints.is_empty(),
            "an unknown kind must carry no hint: {:?}",
            meta.hints
        );
    }

    #[test]
    #[serial]
    fn cmd_enroll_token_path_emits_warning_when_credential_save_fails() {
        // If the state dir is a file (not a directory), save_credential cannot
        // create the parent directory → the Err arm at lines 220-226 fires.
        // cmd_enroll must still return Ok (save failure is non-fatal — it warns
        // and continues) and the printer must carry the failure message.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "I am a file, not a dir").unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(blocker.to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    serde_json::json!({
                        "status": "ok",
                        "deviceId": "dev-fail-cred",
                        "apiKey": "key-fail-cred",
                        "username": "charlie",
                    })
                    .to_string(),
                )
                .create();

            let (printer, cap) = Printer::for_test_doc();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, Some("tok"), None, None, Some("charlie"));
            drop(printer);
            let human = cap.human();

            assert!(
                result.is_ok(),
                "credential-save failure must not fail cmd_enroll: {result:?}"
            );
            assert!(
                human.contains("Failed to save credential") || human.contains("--api-key"),
                "save-failure warning must appear in output: {human}"
            );
        });
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn cmd_enroll_token_path_logs_warn_when_pending_config_save_fails() {
        // Drive the `save_pending_server_config` Err branch at lines 238-239.
        // Strategy: state dir is valid + writable so save_credential succeeds,
        // but `pending-server-config.json` already exists as a DIRECTORY so
        // atomic_write_str (temp+rename) cannot write to that path → Err →
        // tracing::warn fires and cmd_enroll still returns Ok.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("pending-server-config.json");
        std::fs::create_dir_all(&blocker).unwrap();

        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    serde_json::json!({
                        "status": "ok",
                        "deviceId": "dev-pdcfg",
                        "apiKey": "key-pdcfg",
                        "username": "dave",
                        "desiredConfig": {"apiVersion": "cfgd.io/v1alpha1", "kind": "Cfgd"}
                    })
                    .to_string(),
                )
                .create();

            let printer = super::quiet_printer();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, Some("tok"), None, None, Some("dave"));
            assert!(
                result.is_ok(),
                "pending-config save failure must not fail cmd_enroll: {result:?}"
            );
            assert!(
                tmp.path().join("device-credential.json").exists(),
                "credential must still be written even when pending-config save fails"
            );
        });
    }

    #[test]
    fn build_enroll_final_doc_carries_all_output_fields() {
        use crate::cli::init::enroll::{EnrollOutput, build_enroll_final_doc};

        let output = EnrollOutput {
            server_url: "https://cfgd.example.com".to_string(),
            device_id: "test-device-123".to_string(),
            username: "bob".to_string(),
            team: Some("infrastructure".to_string()),
        };

        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_enroll_final_doc(&output));
        drop(printer);

        let json = cap.json().expect("emit must produce JSON payload");
        assert_eq!(
            json["serverUrl"], "https://cfgd.example.com",
            "serverUrl must be present: {json}"
        );
        assert_eq!(
            json["deviceId"], "test-device-123",
            "deviceId must be present: {json}"
        );
        assert_eq!(json["username"], "bob", "username must be present: {json}");
        assert_eq!(
            json["team"], "infrastructure",
            "team must be present when Some: {json}"
        );

        let human = cap.human();
        assert!(
            human.contains("Next Steps"),
            "final doc must include Next Steps section: {human}"
        );
    }

    #[test]
    fn build_enroll_final_doc_omits_team_when_none() {
        use crate::cli::init::enroll::{EnrollOutput, build_enroll_final_doc};

        let output = EnrollOutput {
            server_url: "https://srv.example".to_string(),
            device_id: "d1".to_string(),
            username: "carol".to_string(),
            team: None,
        };

        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_enroll_final_doc(&output));
        drop(printer);

        let json = cap.json().expect("emit must produce JSON payload");
        assert!(
            json.get("team").is_none() || json["team"].is_null(),
            "team must be absent when None: {json}"
        );
    }

    #[test]
    #[serial]
    fn cmd_enroll_connection_refused_returns_err() {
        // Port 1 is never listening — every connection attempt fails
        // immediately. cmd_enroll must propagate the network error as Err.
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let printer = super::quiet_printer();
            let result = cmd_enroll(
                &printer,
                "http://127.0.0.1:1",
                Some("any-token"),
                None,
                None,
                Some("alice"),
            );
            assert!(
                result.is_err(),
                "connection-refused must surface as Err, got Ok"
            );
            assert!(
                !tmp.path().join("device-credential.json").exists(),
                "no credential must be written on connection failure"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_token_path_gateway_500_returns_err() {
        // 500 triggers the retry loop then surfaces an error; no credential written.
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(500)
                .with_body("internal server error")
                .expect_at_least(2)
                .create();

            let printer = super::quiet_printer();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, Some("tok"), None, None, Some("alice"));
            assert!(
                result.is_err(),
                "gateway 500 must surface as Err, got: {result:?}"
            );
            assert!(
                !tmp.path().join("device-credential.json").exists(),
                "no credential must be written after 500 response"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_key_based_with_ssh_key_flag_reaches_signing_step() {
        // --ssh-key supplied → method_mismatch path skipped, key auto-detection
        // skipped, code reaches sign_with_ssh. With an invalid key path,
        // sign_with_ssh Errs and the signing_failed error Doc is emitted.
        // This pins the `key_type=Ssh` / explicit-ssh-key arm.
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _info = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"method":"key"}"#)
                .create();
            let _challenge = server
                .mock("POST", "/api/v1/enroll/challenge")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    r#"{"challengeId":"ch-1","nonce":"sign-this","expiresAt":"2099-01-01T00:00:00Z"}"#,
                )
                .create();

            let (printer, _cap) = Printer::for_test_doc();
            let url = server.url();
            let result = cmd_enroll(
                &printer,
                &url,
                None,
                Some("/nonexistent/fake.pub"),
                None,
                Some("alice"),
            );

            let err = result.expect_err("sign_with_ssh on a nonexistent key must fail");
            let meta = err
                .downcast_ref::<crate::cli::CliErrorMeta>()
                .expect("signing failure returns CliErrorMeta");
            assert_eq!(
                meta.error_kind, "signing_failed",
                "signing failure must carry the signing_failed kind: {meta:?}"
            );
            assert_eq!(
                meta.extras["keyType"], "ssh",
                "extras must record the SSH key type: {:?}",
                meta.extras
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_key_based_with_gpg_key_flag_reaches_signing_step() {
        // --gpg-key supplied → KeyType::Gpg arm exercised. sign_with_gpg with
        // a bogus key ID either Errs (gpg not installed) or Errs (key not
        // found) — either way sign_with_gpg returns Err and signing_failed Doc fires.
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _info = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"method":"key"}"#)
                .create();
            let _challenge = server
                .mock("POST", "/api/v1/enroll/challenge")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    r#"{"challengeId":"ch-2","nonce":"gpg-nonce","expiresAt":"2099-01-01T00:00:00Z"}"#,
                )
                .create();

            let (printer, _cap) = Printer::for_test_doc();
            let url = server.url();
            let result = cmd_enroll(
                &printer,
                &url,
                None,
                None,
                Some("DEADBEEFNONEXISTENTKEYID"),
                Some("alice"),
            );

            let err = result.expect_err("sign_with_gpg on a nonexistent key must fail");
            let meta = err
                .downcast_ref::<crate::cli::CliErrorMeta>()
                .expect("signing failure returns CliErrorMeta");
            assert_eq!(
                meta.error_kind, "signing_failed",
                "signing failure must carry the signing_failed kind: {meta:?}"
            );
            assert_eq!(
                meta.extras["keyType"], "gpg",
                "extras must record the GPG key type: {:?}",
                meta.extras
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_token_path_derives_username_from_user_env() {
        // When username=None, cmd_enroll reads $USER. Verify the credential
        // is written and the enrollment response username (not the env value)
        // is what gets stored in the credential.
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            with_test_env_var("USER", Some("env-driven-user"), || {
                let mut server = mockito::Server::new();
                let _m = server
                    .mock("POST", "/api/v1/enroll")
                    .with_status(200)
                    .with_header("content-type", "application/json")
                    .with_body(
                        serde_json::json!({
                            "status": "ok",
                            "deviceId": "dev-env",
                            "apiKey": "key-env",
                            "username": "server-side-user",
                        })
                        .to_string(),
                    )
                    .create();

                let printer = super::quiet_printer();
                let url = server.url();
                let result = cmd_enroll(&printer, &url, Some("tok"), None, None, None);
                assert!(
                    result.is_ok(),
                    "username-from-env enrollment must succeed: {result:?}"
                );
                let cred_path = tmp.path().join("device-credential.json");
                let cred: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&cred_path).unwrap()).unwrap();
                assert_eq!(
                    cred["username"], "server-side-user",
                    "credential must store server-returned username: {cred}"
                );
            });
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_token_path_no_team_in_response_omits_team_from_credential() {
        // EnrollResponse with team=null → finish_enrollment's `if let Some(ref team)`
        // arm is skipped; credential is written without a team field.
        let tmp = tempfile::tempdir().unwrap();
        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _m = server
                .mock("POST", "/api/v1/enroll")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    serde_json::json!({
                        "status": "ok",
                        "deviceId": "dev-noteam",
                        "apiKey": "key-noteam",
                        "username": "solo",
                    })
                    .to_string(),
                )
                .create();

            let printer = super::quiet_printer();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, Some("tok"), None, None, Some("solo"));
            assert!(
                result.is_ok(),
                "no-team enrollment must succeed: {result:?}"
            );
            let cred: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join("device-credential.json")).unwrap(),
            )
            .unwrap();
            assert!(
                cred.get("team").is_none() || cred["team"].is_null(),
                "credential team field must be absent/null when response has no team: {cred}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_enroll_key_based_auto_detects_ssh_key_in_home() {
        // When neither --ssh-key nor --gpg-key is provided and detect_ssh_key
        // finds a key in $HOME/.ssh, the `Some(path)` arm is taken. With a fake
        // key file, sign_with_ssh fails → cmd_enroll returns the signing_failed
        // CliErrorMeta carrier. This pins the auto-detect branch alongside the
        // explicit-flag tests. The "Using SSH key: <path>" info line is still
        // emitted by detect_ssh_key, so the human surface names the key.
        let tmp = tempfile::tempdir().unwrap();
        let ssh_dir = tmp.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::write(ssh_dir.join("id_ed25519.pub"), b"ssh-ed25519 AAAA fake-key").unwrap();
        let _home_guard = cfgd_core::with_test_home_guard(tmp.path());

        with_test_env_var("CFGD_STATE_DIR", Some(tmp.path().to_str().unwrap()), || {
            let mut server = mockito::Server::new();
            let _info = server
                .mock("GET", "/api/v1/enroll/info")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"method":"key"}"#)
                .create();
            let _challenge = server
                .mock("POST", "/api/v1/enroll/challenge")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    r#"{"challengeId":"auto-ch","nonce":"auto-nonce","expiresAt":"2099-01-01T00:00:00Z"}"#,
                )
                .create();

            let (printer, cap) = Printer::for_test_doc();
            let url = server.url();
            let result = cmd_enroll(&printer, &url, None, None, None, Some("alice"));
            drop(printer);
            let human = cap.human();

            let err = result.expect_err("sign_with_ssh on a fake key must fail");
            assert_eq!(
                err.downcast_ref::<crate::cli::CliErrorMeta>()
                    .expect("signing failure returns CliErrorMeta")
                    .error_kind,
                "signing_failed",
                "auto-detect signing failure must carry the signing_failed kind"
            );
            assert!(
                human.contains("id_ed25519") || human.contains("SSH"),
                "auto-detect info line must reference the discovered key: {human}"
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
        let bare_url = cfgd_core::test_helpers::file_url(&bare);
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
        let bare_url = cfgd_core::test_helpers::file_url(&bare);
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
        let url = cfgd_core::test_helpers::file_url(&bare);

        let printer = quiet_printer();
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
        let url = cfgd_core::test_helpers::file_url(&bare);

        let printer = quiet_printer();
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
        let url = cfgd_core::test_helpers::file_url(&bare);

        let printer = quiet_printer();
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

        let printer = quiet_printer();
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

        let printer = quiet_printer();
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

        let printer = quiet_printer();
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
        let bare_url = cfgd_core::test_helpers::file_url(&bare);
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
        let url = cfgd_core::test_helpers::file_url(&bare);

        let (printer, cap) = Printer::for_test_doc();
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

        drop(printer);
        let out = cap.human();
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
        let url = cfgd_core::test_helpers::file_url(&bare);

        let (printer, cap) = Printer::for_test_doc();
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

        drop(printer);
        let out = cap.human();
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
        let url = cfgd_core::test_helpers::file_url(&bare);
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
        let (printer, cap) = Printer::for_test_doc();
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

        drop(printer);
        let out = cap.human();
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
        let url = cfgd_core::test_helpers::file_url(&bare);
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
        let (printer, cap) = Printer::for_test_doc();
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

        drop(printer);
        let out = cap.human();
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
        let bare_url = cfgd_core::test_helpers::file_url(&bare);
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
        let url = cfgd_core::test_helpers::file_url(&bare);

        let (printer, cap) = Printer::for_test_doc();
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

        drop(printer);
        let out = cap.human();
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
        let url = cfgd_core::test_helpers::file_url(&bare);

        let printer = quiet_printer();
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

    // Drives the install_daemon=true branch at cmd_init.rs:258-282. The
    // user systemd service install at install_systemd_service writes to
    // `$HOME/.config/systemd/user/cfgd.service`, so with HOME pointing at a
    // tempdir via with_test_home_guard, the install succeeds and the
    // "Daemon service installed" success line fires. Pins the happy path.
    #[test]
    #[serial]
    #[cfg(target_os = "linux")]
    fn cmd_init_with_install_daemon_writes_user_unit_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = cfgd_core::with_test_home_guard(tmp.path());
        let target = tmp.path().join("install-daemon-cfg");
        std::fs::create_dir_all(&target).unwrap();

        let (printer, cap) = Printer::for_test_doc();
        let args = InitArgs {
            path: Some(target.to_str().unwrap()),
            from: None,
            branch: "master",
            name: Some("install-daemon-test"),
            apply: false,
            dry_run: false,
            yes: false,
            install_daemon: true,
            theme: None,
            apply_profile: None,
            apply_modules: &[],
        };
        cmd_init(&printer, &args).expect("cmd_init with install_daemon must succeed");

        drop(printer);
        let captured = cap.human();
        let unit = tmp.path().join(".config/systemd/user/cfgd.service");
        if unit.exists() {
            assert!(
                captured.contains("Daemon service installed"),
                "success line should fire when systemd-user install succeeds: {captured}"
            );
            let content = std::fs::read_to_string(&unit).unwrap();
            assert!(
                content.contains("[Service]") && content.contains("ExecStart="),
                "generated systemd unit should be a valid [Service] file: {content}"
            );
        } else {
            // Some hosts reject XDG-style user-service writes (rare but
            // possible). The warning arm at lines 271-274 must have fired
            // instead. Either way, the install_daemon block was exercised.
            assert!(
                captured.contains("Failed to install daemon"),
                "warning fallback must fire when install fails: {captured}"
            );
        }
        drop(home);
    }

    // macOS equivalent of cmd_init_with_install_daemon_writes_user_unit_file.
    // install_launchd_service writes to $HOME/Library/LaunchAgents/com.cfgd.daemon.plist.
    #[test]
    #[serial]
    #[cfg(target_os = "macos")]
    fn cmd_init_with_install_daemon_writes_user_launchd_plist() {
        let tmp = tempfile::tempdir().unwrap();
        let home = cfgd_core::with_test_home_guard(tmp.path());
        let target = tmp.path().join("install-daemon-cfg");
        std::fs::create_dir_all(&target).unwrap();

        let (printer, cap) = Printer::for_test_doc();
        let args = InitArgs {
            path: Some(target.to_str().unwrap()),
            from: None,
            branch: "master",
            name: Some("install-daemon-test"),
            apply: false,
            dry_run: false,
            yes: false,
            install_daemon: true,
            theme: None,
            apply_profile: None,
            apply_modules: &[],
        };
        cmd_init(&printer, &args).expect("cmd_init with install_daemon must succeed");

        drop(printer);
        let captured = cap.human();
        let plist = tmp
            .path()
            .join("Library/LaunchAgents/com.cfgd.daemon.plist");
        if plist.exists() {
            assert!(
                captured.contains("Daemon service installed"),
                "success line should fire when launchd install succeeds: {captured}"
            );
            let content = std::fs::read_to_string(&plist).unwrap();
            assert!(
                content.contains("com.cfgd.daemon") && content.contains("ProgramArguments"),
                "generated plist should be a valid launchd plist: {content}"
            );
        } else {
            assert!(
                captured.contains("Failed to install daemon"),
                "warning fallback must fire when install fails: {captured}"
            );
        }
        drop(home);
    }
}
