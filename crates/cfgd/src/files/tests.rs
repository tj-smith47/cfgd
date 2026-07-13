use super::apply::ensure_target_writable;
#[cfg(unix)]
use super::apply::set_permissions;
use super::plan::detect_language;
use super::template::format_tera_error;
use super::*;

use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use cfgd_core::config::{
    EncryptionMode, EncryptionSpec, EnvVar, FilesSpec, LayerPolicy, ManagedFileSpec, MergedProfile,
    ProfileLayer, ProfileSpec, ResolvedProfile,
};
use cfgd_core::expand_tilde;
use cfgd_core::providers::{
    FileAction, FileDiff, FileDiffKind, FileEntry, FileLayer, FileManager as _, FileTree,
};
use cfgd_core::test_helpers::test_printer;

fn make_resolved_profile(env: Vec<EnvVar>, files: FilesSpec) -> ResolvedProfile {
    ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".to_string(),
            profile_name: "test".to_string(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            env,
            files,
            ..Default::default()
        },
    }
}

#[test]
fn expand_tilde_with_home() {
    // Verify expand_tilde resolves ~/... to $HOME/... without mutating env vars
    // (mutating HOME in parallel tests can corrupt other tests).
    let result = expand_tilde(Path::new("~/.config/test"));
    #[cfg(unix)]
    let home = std::env::var("HOME").expect("HOME must be set");
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("USERPROFILE or HOME must be set");
    assert_eq!(result, PathBuf::from(home).join(".config/test"));
}

#[test]
fn expand_tilde_absolute_path_unchanged() {
    let result = expand_tilde(Path::new("/etc/config"));
    assert_eq!(result, PathBuf::from("/etc/config"));
}

#[test]
fn sha256_hex_deterministic() {
    let h1 = cfgd_core::sha256_hex(b"hello world");
    let h2 = cfgd_core::sha256_hex(b"hello world");
    assert_eq!(h1, h2);
    assert_ne!(h1, cfgd_core::sha256_hex(b"different"));
}

#[test]
fn detect_language_from_extension() {
    assert_eq!(detect_language(Path::new("test.rs")), "rs");
    assert_eq!(detect_language(Path::new("config.yaml")), "yaml");
    assert_eq!(detect_language(Path::new("noext")), "txt");
}

#[test]
fn plan_creates_file_when_target_missing() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create source file
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "hello world").unwrap();

    let target = config_dir.join("target").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();

    assert_eq!(actions.len(), 1);
    assert!(matches!(&actions[0], FileAction::Create { target: t, .. } if *t == target));
}

#[test]
fn plan_skips_when_content_matches() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create source file
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "hello world").unwrap();

    // Create matching target file
    let target_dir = config_dir.join("target");
    fs::create_dir_all(&target_dir).unwrap();
    let target = target_dir.join("test.txt");
    fs::write(&target, "hello world").unwrap();

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();

    assert!(actions.is_empty());
}

#[test]
fn plan_detects_update_when_content_differs() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create source file
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "new content").unwrap();

    // Create different target file
    let target_dir = config_dir.join("target");
    fs::create_dir_all(&target_dir).unwrap();
    let target = target_dir.join("test.txt");
    fs::write(&target, "old content").unwrap();

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();

    assert_eq!(actions.len(), 1);
    assert!(
        matches!(&actions[0], FileAction::Update { target: t, diff, .. } if *t == target && !diff.is_empty())
    );
}

#[test]
fn template_rendering_with_env() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create template file
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(
        files_dir.join("config.txt.tera"),
        "editor={{ editor }}\nshell={{ shell }}",
    )
    .unwrap();

    let target = config_dir.join("target").join("config.txt");
    let expected_target = target.clone();

    let env = vec![
        EnvVar {
            name: "editor".into(),
            value: "vim".into(),
        },
        EnvVar {
            name: "shell".into(),
            value: "/bin/zsh".into(),
        },
    ];

    let resolved = make_resolved_profile(
        env,
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/config.txt.tera".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        FileAction::Create {
            target: t,
            strategy,
            ..
        } => {
            assert_eq!(t, &expected_target);
            assert_eq!(*strategy, FileStrategy::Copy);
        }
        other => panic!("expected Create, got: {other:?}"),
    }
}

#[test]
fn apply_creates_files() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create source file
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "hello world").unwrap();

    let target = config_dir.join("output").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();

    let printer = test_printer();
    fm.apply(&actions, &printer).unwrap();

    assert!(target.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
}

#[test]
fn apply_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create source file
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "hello world").unwrap();

    let target = config_dir.join("output").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let printer = test_printer();

    // First apply
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    fm.apply(&actions, &printer).unwrap();

    // Second apply — should find nothing to do
    let fm2 = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions2 = fm2.plan(&resolved.merged).unwrap();
    assert!(actions2.is_empty());
}

#[test]
fn template_custom_functions() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(
        files_dir.join("sys.txt.tera"),
        "os={{ os() }}\narch={{ arch() }}",
    )
    .unwrap();

    let target = config_dir.join("target").join("sys.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/sys.txt.tera".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let printer = test_printer();

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    fm.apply(&actions, &printer).unwrap();

    let content = fs::read_to_string(&target).unwrap();
    assert!(content.contains(&format!("os={}", std::env::consts::OS)));
    assert!(content.contains(&format!("arch={}", std::env::consts::ARCH)));
}

#[test]
fn template_system_fact_distro() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("distro.txt.tera"), "distro={{ __distro }}").unwrap();

    let target = config_dir.join("target").join("distro.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/distro.txt.tera".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let printer = test_printer();
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    fm.apply(&actions, &printer).unwrap();

    let content = fs::read_to_string(&target).unwrap();
    let expected_distro = cfgd_core::platform::Platform::detect()
        .distro
        .as_str()
        .to_string();
    let known = [
        "ubuntu", "debian", "fedora", "rhel", "centos", "arch", "manjaro", "alpine", "opensuse",
        "macos", "freebsd", "windows", "unknown",
    ];
    assert!(
        known.contains(&expected_distro.as_str()),
        "unexpected __distro value: {expected_distro}"
    );
    assert_eq!(
        content,
        format!("distro={expected_distro}"),
        "__distro system fact not rendered correctly"
    );
}

#[test]
#[cfg(unix)]
fn permissions_set_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("secret.txt"), "secret data").unwrap();

    let target = config_dir.join("output").join("secret.txt");

    let mut permissions = HashMap::new();
    permissions.insert(target.display().to_string(), "600".to_string());

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions,
        },
    );

    let printer = test_printer();

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    fm.apply(&actions, &printer).unwrap();

    let metadata = fs::metadata(&target).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

#[test]
fn source_not_found_error() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let target = config_dir.join("target").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "nonexistent/file.txt".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn template_error_includes_path() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // Invalid template syntax
    fs::write(files_dir.join("bad.txt.tera"), "{{ invalid %}").unwrap();

    let target = config_dir.join("target").join("bad.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/bad.txt.tera".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("template"));
}

#[test]
fn scan_source_builds_file_tree() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("source");
    fs::create_dir_all(source_dir.join("sub")).unwrap();
    fs::write(source_dir.join("file1.txt"), "content1").unwrap();
    fs::write(source_dir.join("sub").join("file2.txt"), "content2").unwrap();

    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    use cfgd_core::providers::FileManager as _;
    let layers = vec![FileLayer {
        source_dir,
        origin_source: "local".to_string(),
        priority: 1000,
    }];

    let tree = fm.scan_source(&layers).unwrap();
    assert_eq!(tree.files.len(), 2);
}

#[test]
fn source_template_cannot_access_local_env() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create a source template that tries to use a local variable
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(
        files_dir.join("team.txt.tera"),
        "team={{ team_name }}\nlocal={{ personal_var }}",
    )
    .unwrap();

    let target = config_dir.join("target").join("team.txt");

    // Local profile has personal_var but NOT team_name
    let local_env = vec![EnvVar {
        name: "personal_var".into(),
        value: "my-secret".into(),
    }];

    let resolved = make_resolved_profile(
        local_env,
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/team.txt.tera".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: Some("acme-corp".to_string()),
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    // Set source env — acme-corp only has team_name, NOT personal_var
    let mut source_vars = HashMap::new();
    source_vars.insert(
        "acme-corp".to_string(),
        vec![EnvVar {
            name: "team_name".into(),
            value: "Platform".into(),
        }],
    );
    fm.set_source_env(&source_vars);

    // Planning should fail because the template references personal_var
    // which is NOT in the source's sandboxed context
    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_err(),
        "Source template should not access local env vars"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("personal_var"),
        "Error should mention the inaccessible variable: {err}"
    );
}

#[test]
fn source_template_sandbox_violation_pins_variant() {
    // Same production path as `source_template_cannot_access_local_env`, but
    // asserts the exact CompositionError variant rather than the rendered
    // error string — so a rename or signature change to the variant trips
    // this test rather than silently reshaping the error surface.
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(
        files_dir.join("team.txt.tera"),
        "team={{ team_name }}\nlocal={{ personal_var }}",
    )
    .unwrap();

    let target = config_dir.join("target").join("team.txt");

    let local_env = vec![EnvVar {
        name: "personal_var".into(),
        value: "my-secret".into(),
    }];

    let resolved = make_resolved_profile(
        local_env,
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/team.txt.tera".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: Some("acme-corp".to_string()),
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let mut source_vars = HashMap::new();
    source_vars.insert(
        "acme-corp".to_string(),
        vec![EnvVar {
            name: "team_name".into(),
            value: "Platform".into(),
        }],
    );
    fm.set_source_env(&source_vars);

    let err = fm.plan(&resolved.merged).unwrap_err();
    let inner = match err {
        cfgd_core::errors::CfgdError::Composition(boxed) => *boxed,
        other => panic!("expected CfgdError::Composition, got: {other:?}"),
    };
    assert!(matches!(
        inner,
        cfgd_core::errors::CompositionError::TemplateSandboxViolation { .. }
    ));
}

#[test]
fn source_template_can_access_own_env() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create a source template using only source-provided env vars
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("team.txt.tera"), "team={{ team_name }}").unwrap();

    let target = config_dir.join("target").join("team.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/team.txt.tera".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: Some("acme-corp".to_string()),
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    // Set source env
    let mut source_vars = HashMap::new();
    source_vars.insert(
        "acme-corp".to_string(),
        vec![EnvVar {
            name: "team_name".into(),
            value: "Platform".into(),
        }],
    );
    fm.set_source_env(&source_vars);

    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        FileAction::Create {
            target: t,
            strategy,
            ..
        } => {
            assert_eq!(t, &target);
            assert_eq!(*strategy, FileStrategy::Copy);
        }
        other => panic!("expected Create, got: {other:?}"),
    }
}

#[test]
fn source_template_can_access_system_facts() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create a source template using system facts
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(
        files_dir.join("info.txt.tera"),
        "os={{ __os }}\narch={{ __arch }}",
    )
    .unwrap();

    let target = config_dir.join("target").join("info.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/info.txt.tera".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: Some("acme-corp".to_string()),
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    // Set source env (empty — but system facts should still be available)
    let mut source_vars: HashMap<String, Vec<EnvVar>> = HashMap::new();
    source_vars.insert("acme-corp".to_string(), vec![]);
    fm.set_source_env(&source_vars);

    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        FileAction::Create {
            target: t,
            strategy,
            ..
        } => {
            assert_eq!(t, &target);
            assert_eq!(*strategy, FileStrategy::Copy);
        }
        other => panic!("expected Create, got: {other:?}"),
    }
}

#[test]
fn symlink_strategy_creates_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "symlink content").unwrap();

    let target = config_dir.join("output").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        FileAction::Create {
            strategy: FileStrategy::Symlink,
            ..
        }
    ));

    let printer = test_printer();
    fm.apply(&actions, &printer).unwrap();

    assert!(target.is_symlink());
    let link_target = fs::read_link(&target).unwrap();
    assert_eq!(link_target, files_dir.join("test.txt"));
    assert_eq!(fs::read_to_string(&target).unwrap(), "symlink content");
}

#[test]
fn symlink_strategy_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "content").unwrap();

    let target = config_dir.join("output").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let printer = test_printer();

    // First apply
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    fm.apply(&actions, &printer).unwrap();

    // Second plan — symlink already points to correct source, no actions needed
    let fm2 = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions2 = fm2.plan(&resolved.merged).unwrap();
    assert!(
        actions2.is_empty(),
        "expected no actions, got {:?}",
        actions2
    );
}

#[test]
fn hardlink_strategy_creates_hardlink() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "hardlink content").unwrap();

    let target = config_dir.join("output").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Hardlink),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);

    let printer = test_printer();
    fm.apply(&actions, &printer).unwrap();

    assert!(target.exists());
    assert!(cfgd_core::is_same_inode(
        &files_dir.join("test.txt"),
        &target
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), "hardlink content");
}

#[test]
fn template_auto_upgrades_to_copy() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("config.txt.tera"), "val={{ val }}").unwrap();

    let target = config_dir.join("output").join("config.txt");

    let env = vec![EnvVar {
        name: "val".into(),
        value: "hello".into(),
    }];

    let resolved = make_resolved_profile(
        env,
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/config.txt.tera".to_string(),
                target: target.clone(),
                // Explicitly set Symlink — but template should force Copy
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    // Strategy should be Copy despite Symlink being requested
    assert!(matches!(
        &actions[0],
        FileAction::Create {
            strategy: FileStrategy::Copy,
            ..
        }
    ));

    let printer = test_printer();
    fm.apply(&actions, &printer).unwrap();

    // Should be a regular file, not a symlink
    assert!(!target.is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "val=hello");
}

#[test]
fn global_strategy_applies_when_no_per_file_override() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "content").unwrap();

    let target = config_dir.join("output").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: None, // No per-file override
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    fm.set_global_strategy(FileStrategy::Copy);

    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        FileAction::Create {
            strategy: FileStrategy::Copy,
            ..
        }
    ));
}

#[test]
fn private_file_skipped_when_source_missing() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let target = config_dir.join("target").join("secret.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/nonexistent.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: true,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        FileAction::Skip { reason, .. } if reason.contains("private")
    ));
}

#[test]
fn non_private_file_errors_when_source_missing() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let target = config_dir.join("target").join("test.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/nonexistent.txt".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("source file not found"),
        "error should mention missing source file, got: {msg}"
    );
    assert!(
        msg.contains("nonexistent.txt"),
        "error should mention the missing filename, got: {msg}"
    );
}

// --- is_tera_template ---

#[test]
fn is_tera_template_true() {
    assert!(is_tera_template(Path::new("config.txt.tera")));
}

#[test]
fn is_tera_template_false() {
    assert!(!is_tera_template(Path::new("config.txt")));
    assert!(!is_tera_template(Path::new("noext")));
}

// --- diff ---

#[test]
fn diff_no_changes_prints_success() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "same content").unwrap();

    let target_dir = config_dir.join("target");
    fs::create_dir_all(&target_dir).unwrap();
    let target = target_dir.join("test.txt");
    fs::write(&target, "same content").unwrap();

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let has_drift = fm.diff(&resolved.merged, &printer).unwrap();
    assert!(!has_drift, "identical content should report no drift");
}

#[test]
fn diff_detects_content_difference() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "new").unwrap();

    let target_dir = config_dir.join("target");
    fs::create_dir_all(&target_dir).unwrap();
    let target = target_dir.join("test.txt");
    fs::write(&target, "old").unwrap();

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    assert!(fm.diff(&resolved.merged, &printer).is_ok());
    let output = buf.lock().unwrap();
    assert!(
        output.contains("test.txt"),
        "diff output should reference the changed file, got: {output}"
    );
    assert!(
        !output.contains("All files match desired state"),
        "diff output should NOT say all files match when content differs, got: {output}"
    );
}

#[test]
fn diff_new_file_shown() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("new.txt"), "brand new").unwrap();

    let target = config_dir.join("nonexistent").join("new.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/new.txt".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    assert!(fm.diff(&resolved.merged, &printer).is_ok());
    let output = buf.lock().unwrap();
    assert!(
        output.contains("new.txt"),
        "diff output should reference the new file, got: {output}"
    );
    assert!(
        output.contains("new file"),
        "diff output should indicate the file is new, got: {output}"
    );
    assert!(
        !output.contains("All files match desired state"),
        "diff output should NOT say all files match for a new file, got: {output}"
    );
}

// --- render_template_for_display ---

#[test]
fn render_template_for_display_basic() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    let tpl = files_dir.join("greeting.txt.tera");
    fs::write(&tpl, "Hello {{ name }}!").unwrap();

    let env = vec![EnvVar {
        name: "name".into(),
        value: "world".into(),
    }];

    let resolved = make_resolved_profile(env, FilesSpec::default());
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let rendered = fm.render_template_for_display(&tpl).unwrap();
    assert_eq!(rendered, "Hello world!");
}

// --- check_permissions ---

#[test]
#[cfg(unix)]
fn check_permissions_drift_detected() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let target = config_dir.join("secret.txt");
    fs::write(&target, "data").unwrap();
    // Set permissive permissions
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

    let mut permissions = HashMap::new();
    permissions.insert(target.display().to_string(), "600".to_string());

    let managed = ManagedFileSpec {
        source: "files/secret.txt".to_string(),
        target: target.clone(),
        strategy: Some(FileStrategy::Copy),
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    };

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![managed.clone()],
            permissions,
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let action = fm
        .check_permissions(&target, &managed, &resolved.merged)
        .unwrap();
    assert!(action.is_some());
    assert!(matches!(
        action.unwrap(),
        FileAction::SetPermissions { mode: 0o600, .. }
    ));
}

#[test]
#[cfg(unix)]
fn check_permissions_no_drift() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let target = config_dir.join("secret.txt");
    fs::write(&target, "data").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

    let mut permissions = HashMap::new();
    permissions.insert(target.display().to_string(), "600".to_string());

    let managed = ManagedFileSpec {
        source: "files/secret.txt".to_string(),
        target: target.clone(),
        strategy: Some(FileStrategy::Copy),
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    };

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![managed.clone()],
            permissions,
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let action = fm
        .check_permissions(&target, &managed, &resolved.merged)
        .unwrap();
    assert!(action.is_none());
}

// --- set_permissions ---

#[test]
#[cfg(unix)]
fn set_permissions_changes_mode() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "data").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    set_permissions(&file, 0o600).unwrap();

    let metadata = fs::metadata(&file).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

#[test]
#[cfg(unix)]
fn set_permissions_nonexistent_path_returns_io_err_not_permission_denied() {
    // Drives the non-PermissionDenied branch of the error-mapping at
    // files/apply.rs:347-352. chmod on a nonexistent path returns ENOENT
    // which std::io maps to ErrorKind::NotFound — distinct from
    // ErrorKind::PermissionDenied, so the FileError::Io arm fires.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.txt");
    let err = set_permissions(&missing, 0o600).expect_err("ENOENT must error");
    let msg = err.to_string();
    assert!(
        !msg.contains("PermissionDenied"),
        "non-PD error should NOT be mapped to FileError::PermissionDenied, got: {msg}"
    );
}

// --- ensure_target_writable ---

#[test]
fn ensure_target_writable_creates_parent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("sub").join("deep").join("file.txt");
    ensure_target_writable(&target).unwrap();
    assert!(target.parent().unwrap().exists());
}

// --- format_tera_error ---

#[test]
fn format_tera_error_basic() {
    // Create a tera error by trying to render invalid template
    let mut tera = Tera::default();
    let err = tera.add_raw_template("bad", "{{ invalid %}").unwrap_err();
    let formatted = format_tera_error(&err);
    assert!(!formatted.is_empty());
}

// --- multiple files in plan ---

#[test]
fn plan_multiple_files() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("a.txt"), "aaa").unwrap();
    fs::write(files_dir.join("b.txt"), "bbb").unwrap();

    let target_a = config_dir.join("out").join("a.txt");
    let target_b = config_dir.join("out").join("b.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![
                ManagedFileSpec {
                    source: "files/a.txt".to_string(),
                    target: target_a,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
                ManagedFileSpec {
                    source: "files/b.txt".to_string(),
                    target: target_b,
                    strategy: Some(FileStrategy::Copy),
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
            ],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 2);
}

// --- apply update overwrites ---

#[test]
fn apply_update_overwrites_target() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("test.txt"), "updated content").unwrap();

    let target_dir = config_dir.join("output");
    fs::create_dir_all(&target_dir).unwrap();
    let target = target_dir.join("test.txt");
    fs::write(&target, "old content").unwrap();

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/test.txt".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let printer = test_printer();
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    fm.apply(&actions, &printer).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "updated content");
}

// --- encryption detection ---

#[test]
fn is_file_encrypted_detects_sops_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secrets.yaml");
    // A minimal SOPS-encrypted YAML file always has a top-level `sops` key
    // with at least `mac` and `lastmodified` sub-keys.
    fs::write(
        &file,
        r#"mysecret: ENC[AES256_GCM,data=abc,type=str]
sops:
    mac: ENC[AES256_GCM,data=xyz,type=str]
    lastmodified: "2024-01-01T00:00:00Z"
    version: 3.8.1
"#,
    )
    .unwrap();
    assert!(is_file_encrypted(&file, "sops").unwrap());
}

#[test]
fn is_file_encrypted_rejects_plaintext_yaml_for_sops() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain.yaml");
    // A file that merely mentions sops in a comment must NOT be detected
    // as encrypted — the check is structural, not textual.
    fs::write(
        &file,
        r#"# managed by sops
key: value
other: data
"#,
    )
    .unwrap();
    assert!(!is_file_encrypted(&file, "sops").unwrap());
}

#[test]
fn is_file_encrypted_detects_age_header() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.age");
    // age-encrypted files begin with the age-encryption.org magic header.
    fs::write(
        &file,
        "age-encryption.org/v1\n-> X25519 abc123\n--- abc\nbinarydata\n",
    )
    .unwrap();
    assert!(is_file_encrypted(&file, "age").unwrap());
}

#[test]
fn is_file_encrypted_rejects_plaintext_for_age() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain.txt");
    fs::write(&file, "this is not age encrypted\n").unwrap();
    assert!(!is_file_encrypted(&file, "age").unwrap());
}

#[test]
fn is_file_encrypted_unknown_backend_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    fs::write(&file, "content").unwrap();
    let result = is_file_encrypted(&file, "gpg");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, FileError::UnknownEncryptionBackend { .. }));
    assert!(err.to_string().contains("gpg"));
}

// --- encryption validation in plan() ---

#[test]
fn plan_rejects_unencrypted_sops_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // Plaintext YAML — not SOPS encrypted
    fs::write(files_dir.join("secret.yaml"), "password: hunter2\n").unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("sops"),
        "error should mention backend: {err}"
    );
}

#[test]
fn plan_accepts_sops_encrypted_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // Minimal SOPS-encrypted YAML
    fs::write(
        files_dir.join("secret.yaml"),
        "mysecret: ENC[AES256_GCM,data=abc,type=str]\nsops:\n    mac: ENC[AES256_GCM,data=xyz,type=str]\n    lastmodified: \"2024-01-01T00:00:00Z\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_ok(),
        "expected plan to succeed: {:?}",
        result.err()
    );
    let actions = result.unwrap();
    assert_eq!(actions.len(), 1, "should produce exactly one file action");
    assert!(
        matches!(
            &actions[0],
            FileAction::Create {
                source,
                target: t,
                strategy: FileStrategy::Copy,
                ..
            } if source.ends_with("secret.yaml") && *t == target
        ),
        "expected Create action targeting secret.yaml with Copy strategy, got: {:?}",
        actions[0]
    );
}

#[test]
fn plan_rejects_always_mode_with_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // SOPS-encrypted file content so encryption check passes first
    fs::write(
        files_dir.join("secret.yaml"),
        "mysecret: ENC[AES256_GCM,data=abc,type=str]\nsops:\n    mac: ENC[AES256_GCM,data=xyz,type=str]\n    lastmodified: \"2024-01-01T00:00:00Z\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::Always,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Always"),
        "error should mention Always mode: {err}"
    );
    assert!(
        err.to_string().contains("Symlink") || err.to_string().contains("symlink"),
        "error should mention symlink: {err}"
    );
}

#[test]
fn plan_rejects_always_mode_with_hardlink() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // SOPS-encrypted file content
    fs::write(
        files_dir.join("secret.yaml"),
        "mysecret: ENC[AES256_GCM,data=abc,type=str]\nsops:\n    mac: ENC[AES256_GCM,data=xyz,type=str]\n    lastmodified: \"2024-01-01T00:00:00Z\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Hardlink),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::Always,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Always"),
        "error should mention Always mode: {err}"
    );
    assert!(
        err.to_string().contains("Hardlink") || err.to_string().contains("hardlink"),
        "error should mention hardlink: {err}"
    );
}

#[test]
fn plan_accepts_always_mode_with_copy() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // SOPS-encrypted file
    fs::write(
        files_dir.join("secret.yaml"),
        "mysecret: ENC[AES256_GCM,data=abc,type=str]\nsops:\n    mac: ENC[AES256_GCM,data=xyz,type=str]\n    lastmodified: \"2024-01-01T00:00:00Z\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target: target.clone(),
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::Always,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_ok(),
        "expected plan to succeed: {:?}",
        result.err()
    );
    let actions = result.unwrap();
    assert_eq!(actions.len(), 1, "should produce exactly one file action");
    assert!(
        matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Copy,
                ..
            }
        ),
        "expected Create action with Copy strategy for Always+Copy encryption, got: {:?}",
        actions[0]
    );
    // Verify the encryption spec (Always mode) didn't prevent Copy strategy
    if let FileAction::Create {
        source, target: t, ..
    } = &actions[0]
    {
        assert!(source.ends_with("secret.yaml"));
        assert_eq!(*t, target);
    }
}

// --- is_tera_template edge cases ---

#[test]
fn is_tera_template_double_extension() {
    // .yaml.tera should be detected as a template (extension is "tera")
    assert!(is_tera_template(Path::new("config.yaml.tera")));
    assert!(is_tera_template(Path::new("/some/path/deep.nested.tera")));
}

#[test]
fn is_tera_template_case_sensitive() {
    // .TERA and .Tera should NOT match (extension check is case-sensitive)
    assert!(!is_tera_template(Path::new("config.TERA")));
    assert!(!is_tera_template(Path::new("config.Tera")));
    assert!(!is_tera_template(Path::new("config.TeRa")));
}

#[test]
fn is_tera_template_j2_not_detected() {
    // Jinja2 templates (.j2) are not recognized as Tera templates
    assert!(!is_tera_template(Path::new("config.j2")));
    assert!(!is_tera_template(Path::new("config.yaml.j2")));
}

#[test]
fn is_tera_template_dotfile_named_tera() {
    // ".tera" is a hidden file with no extension (stem="tera", ext=None)
    assert!(!is_tera_template(Path::new(".tera")));
}

#[test]
fn is_tera_template_no_extension() {
    assert!(!is_tera_template(Path::new("Makefile")));
    assert!(!is_tera_template(Path::new("/usr/local/bin/cfgd")));
}

#[test]
fn is_tera_template_tera_in_directory_not_file() {
    // "tera" in the directory path should not affect the result
    assert!(!is_tera_template(Path::new("/templates/tera/config.txt")));
}

// --- effective_strategy unit tests ---

#[test]
fn effective_strategy_template_forces_copy_over_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let result = fm.effective_strategy(Path::new("config.yaml.tera"), Some(FileStrategy::Symlink));
    assert_eq!(result, FileStrategy::Copy);
}

#[test]
fn effective_strategy_template_forces_copy_over_hardlink() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let result = fm.effective_strategy(Path::new("app.conf.tera"), Some(FileStrategy::Hardlink));
    assert_eq!(result, FileStrategy::Copy);
}

#[test]
fn effective_strategy_template_forces_copy_even_with_none() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let result = fm.effective_strategy(Path::new("file.tera"), None);
    assert_eq!(result, FileStrategy::Copy);
}

#[test]
fn effective_strategy_non_template_uses_per_file() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let mut fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();
    fm.set_global_strategy(FileStrategy::Copy);

    // Per-file Symlink should override global Copy
    let result = fm.effective_strategy(Path::new("config.txt"), Some(FileStrategy::Symlink));
    assert_eq!(result, FileStrategy::Symlink);
}

#[test]
fn effective_strategy_non_template_no_override_uses_global() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let mut fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();
    fm.set_global_strategy(FileStrategy::Hardlink);

    let result = fm.effective_strategy(Path::new("plain.txt"), None);
    assert_eq!(result, FileStrategy::Hardlink);
}

// --- render_template_for_display additional tests ---

#[test]
fn render_template_for_display_with_system_facts() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    let tpl = files_dir.join("sysinfo.txt.tera");
    fs::write(&tpl, "os={{ __os }} arch={{ __arch }}").unwrap();

    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let rendered = fm.render_template_for_display(&tpl).unwrap();
    assert!(
        rendered.contains(std::env::consts::OS),
        "rendered should contain OS: {rendered}"
    );
    assert!(
        rendered.contains(std::env::consts::ARCH),
        "rendered should contain ARCH: {rendered}"
    );
}

#[test]
fn render_template_for_display_undefined_var_error() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    let tpl = files_dir.join("bad_ref.txt.tera");
    fs::write(&tpl, "value={{ nonexistent_variable }}").unwrap();

    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let result = fm.render_template_for_display(&tpl);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("nonexistent_variable"),
        "error should mention the undefined variable name, got: {msg}"
    );
    assert!(
        msg.contains("is not defined"),
        "error should explain the variable was not defined, got: {msg}"
    );
}

#[test]
fn render_template_for_display_with_custom_functions() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    let tpl = files_dir.join("funcs.txt.tera");
    fs::write(
        &tpl,
        "os={{ os() }} arch={{ arch() }} host={{ hostname() }}",
    )
    .unwrap();

    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let rendered = fm.render_template_for_display(&tpl).unwrap();
    assert!(rendered.contains(&format!("os={}", std::env::consts::OS)));
    assert!(rendered.contains(&format!("arch={}", std::env::consts::ARCH)));
    // hostname() should return something (at least "unknown")
    assert!(rendered.contains("host="));
}

#[test]
fn render_template_for_display_env_function_reads_real_env() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    let tpl = files_dir.join("envfn.txt.tera");
    fs::write(&tpl, "val={{ env(name=\"CFGD_TEST_RENDER_VAR\") }}").unwrap();

    // SAFETY: test environment
    unsafe { std::env::set_var("CFGD_TEST_RENDER_VAR", "render_test_value") };

    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let rendered = fm.render_template_for_display(&tpl).unwrap();
    assert_eq!(rendered, "val=render_test_value");

    unsafe { std::env::remove_var("CFGD_TEST_RENDER_VAR") };
}

#[test]
fn render_template_for_display_multiline_and_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    let tpl = files_dir.join("multi.txt.tera");
    fs::write(
        &tpl,
        "line1={{ greeting }}\n  line2={{ greeting }}\n\nline4=end",
    )
    .unwrap();

    let env = vec![EnvVar {
        name: "greeting".into(),
        value: "hi".into(),
    }];
    let resolved = make_resolved_profile(env, FilesSpec::default());
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let rendered = fm.render_template_for_display(&tpl).unwrap();
    assert_eq!(rendered, "line1=hi\n  line2=hi\n\nline4=end");
}

// --- detect_language edge cases ---

#[test]
fn detect_language_tera_double_extension() {
    // For a .yaml.tera file, extension is "tera" not "yaml"
    assert_eq!(detect_language(Path::new("config.yaml.tera")), "tera");
}

#[test]
fn detect_language_hidden_file_no_extension() {
    // Hidden files like .bashrc have no extension; detect_language returns "txt"
    assert_eq!(detect_language(Path::new(".bashrc")), "txt");
}

// --- source_env template env() sandbox ---

#[test]
fn source_template_env_function_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    // Create a source template that tries to use env() function
    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(
        files_dir.join("exfil.txt.tera"),
        "stolen={{ env(name=\"HOME\") }}",
    )
    .unwrap();

    let target = config_dir.join("target").join("exfil.txt");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/exfil.txt.tera".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: Some("untrusted-source".to_string()),
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let mut source_vars: HashMap<String, Vec<EnvVar>> = HashMap::new();
    source_vars.insert("untrusted-source".to_string(), vec![]);
    fm.set_source_env(&source_vars);

    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_err(),
        "source template should not be able to call env()"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not available in source templates"),
        "error should mention sandbox restriction: {err}"
    );
}

// --- encryption: InRepo mode with symlink is allowed ---

#[test]
fn plan_allows_inrepo_mode_with_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // SOPS-encrypted file
    fs::write(
        files_dir.join("secret.yaml"),
        "mysecret: ENC[AES256_GCM,data=abc,type=str]\nsops:\n    mac: ENC[AES256_GCM,data=xyz,type=str]\n    lastmodified: \"2024-01-01T00:00:00Z\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target: target.clone(),
                // InRepo + Symlink should be allowed (only Always blocks symlinks)
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_ok(),
        "InRepo + Symlink should be allowed: {:?}",
        result.err()
    );
    let actions = result.unwrap();
    assert_eq!(actions.len(), 1, "should produce exactly one file action");
    assert!(
        matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Symlink,
                ..
            }
        ),
        "expected Create action with Symlink strategy for InRepo mode, got: {:?}",
        actions[0]
    );
    if let FileAction::Create {
        source, target: t, ..
    } = &actions[0]
    {
        assert!(source.ends_with("secret.yaml"));
        assert_eq!(*t, target);
    }
}

// --- global strategy with templates ---

#[test]
fn global_symlink_strategy_overridden_for_template() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(files_dir.join("app.conf.tera"), "port={{ port }}").unwrap();

    let target = config_dir.join("output").join("app.conf");

    let env = vec![EnvVar {
        name: "port".into(),
        value: "8080".into(),
    }];

    let resolved = make_resolved_profile(
        env,
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/app.conf.tera".to_string(),
                target: target.clone(),
                strategy: None, // No per-file override
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let mut fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    // Global is Symlink, but template should force Copy
    fm.set_global_strategy(FileStrategy::Symlink);

    let actions = fm.plan(&resolved.merged).unwrap();
    assert_eq!(actions.len(), 1);
    assert!(
        matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Copy,
                ..
            }
        ),
        "template file should use Copy even when global is Symlink: {:?}",
        actions[0]
    );
}

// --- Encryption + strategy validation tests ---

#[test]
fn plan_rejects_encryption_always_with_symlink_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // Create a SOPS-encrypted YAML file so is_file_encrypted succeeds
    fs::write(
        files_dir.join("secret.yaml"),
        "data: encrypted\nsops:\n  mac: abc123\n  lastmodified: \"2025-01-01\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::Always,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(result.is_err(), "Always + Symlink should be rejected");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("incompatible") || err_msg.contains("Symlink"),
        "error should mention incompatible strategy: {err_msg}"
    );
}

#[test]
fn plan_rejects_encryption_always_with_hardlink_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    fs::write(
        files_dir.join("secret.yaml"),
        "data: encrypted\nsops:\n  mac: abc123\n  lastmodified: \"2025-01-01\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Hardlink),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::Always,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(result.is_err(), "Always + Hardlink should be rejected");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("incompatible") || err_msg.contains("Hardlink"),
        "error should mention incompatible strategy: {err_msg}"
    );
}

#[test]
fn plan_allows_encryption_always_with_copy_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // Create a SOPS-encrypted YAML file
    fs::write(
        files_dir.join("secret.yaml"),
        "data: encrypted\nsops:\n  mac: abc123\n  lastmodified: \"2025-01-01\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::Always,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    // Should NOT fail on the encryption+strategy check (Always + Copy is ok).
    // It will proceed to the content comparison phase.
    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_ok(),
        "Always + Copy should be allowed, got: {:?}",
        result.err()
    );
    let actions = result.unwrap();
    assert_eq!(actions.len(), 1, "should produce exactly one file action");
    assert!(
        matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Copy,
                ..
            }
        ),
        "expected Create with Copy strategy for Always encryption, got: {:?}",
        actions[0]
    );
}

#[test]
fn plan_allows_encryption_inrepo_with_symlink_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // Create a SOPS-encrypted YAML file
    fs::write(
        files_dir.join("secret.yaml"),
        "data: encrypted\nsops:\n  mac: abc123\n  lastmodified: \"2025-01-01\"\n",
    )
    .unwrap();

    let target = config_dir.join("target").join("secret.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/secret.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Symlink),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    // InRepo + Symlink is allowed (only Always mode is incompatible with symlinks).
    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_ok(),
        "InRepo + Symlink should be allowed, got: {:?}",
        result.err()
    );
    let actions = result.unwrap();
    assert_eq!(actions.len(), 1, "should produce exactly one file action");
    assert!(
        matches!(
            &actions[0],
            FileAction::Create {
                strategy: FileStrategy::Symlink,
                ..
            }
        ),
        "expected Create with Symlink strategy for InRepo encryption, got: {:?}",
        actions[0]
    );
}

#[test]
fn plan_rejects_unencrypted_file_when_encryption_required() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    let files_dir = config_dir.join("files");
    fs::create_dir_all(&files_dir).unwrap();
    // Create a plaintext file — NOT encrypted
    fs::write(files_dir.join("plain.yaml"), "key: value\n").unwrap();

    let target = config_dir.join("target").join("plain.yaml");

    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "files/plain.yaml".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: Some(EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
    );

    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();
    let result = fm.plan(&resolved.merged);
    assert!(
        result.is_err(),
        "plaintext file with encryption required should be rejected"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("not encrypted") || err_msg.contains("encrypted"),
        "error should mention encryption: {err_msg}"
    );
}

// -----------------------------------------------------------------------
// Direct diff() method tests — all 5 branches
// -----------------------------------------------------------------------

/// Call the trait's diff method (not the inherent CfgdFileManager::diff which
/// has a different signature).
fn trait_diff(
    fm: &CfgdFileManager,
    source: &FileTree,
    target: &FileTree,
) -> cfgd_core::errors::Result<Vec<FileDiff>> {
    <CfgdFileManager as cfgd_core::providers::FileManager>::diff(fm, source, target)
}

fn make_entry(hash: &str, perms: Option<u32>, source: &str) -> FileEntry {
    FileEntry {
        content_hash: hash.to_string(),
        permissions: perms,
        is_template: false,
        source_path: PathBuf::from(source),
        origin_source: "local".to_string(),
    }
}

#[test]
fn diff_modified_when_hashes_differ() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target_path = PathBuf::from("/home/user/.config/test.txt");
    let source_path = PathBuf::from("/opt/cfgd/files/test.txt");

    let mut source = FileTree {
        files: BTreeMap::new(),
    };
    source.files.insert(
        target_path.clone(),
        make_entry("aaaa", Some(0o644), "/opt/cfgd/files/test.txt"),
    );

    let mut target = FileTree {
        files: BTreeMap::new(),
    };
    target.files.insert(
        target_path.clone(),
        make_entry("bbbb", Some(0o644), &target_path.display().to_string()),
    );

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert_eq!(diffs.len(), 1);
    assert!(
        matches!(
            &diffs[0].kind,
            FileDiffKind::Modified { source: s, .. } if *s == source_path
        ),
        "expected Modified diff, got: {:?}",
        diffs[0].kind
    );
    assert_eq!(diffs[0].target, target_path);
}

#[test]
fn diff_permissions_changed_when_hashes_match() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target_path = PathBuf::from("/home/user/.bashrc");

    let mut source = FileTree {
        files: BTreeMap::new(),
    };
    source.files.insert(
        target_path.clone(),
        make_entry("samehash", Some(0o755), "/opt/cfgd/files/.bashrc"),
    );

    let mut target = FileTree {
        files: BTreeMap::new(),
    };
    target.files.insert(
        target_path.clone(),
        make_entry("samehash", Some(0o644), &target_path.display().to_string()),
    );

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert_eq!(diffs.len(), 1);
    assert!(
        matches!(
            &diffs[0].kind,
            FileDiffKind::PermissionsChanged {
                current: 0o644,
                desired: 0o755
            }
        ),
        "expected PermissionsChanged diff, got: {:?}",
        diffs[0].kind
    );
}

#[test]
fn diff_unchanged_when_hash_and_perms_match() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target_path = PathBuf::from("/home/user/.vimrc");

    let mut source = FileTree {
        files: BTreeMap::new(),
    };
    source.files.insert(
        target_path.clone(),
        make_entry("unchanged", Some(0o644), "/opt/cfgd/files/.vimrc"),
    );

    let mut target = FileTree {
        files: BTreeMap::new(),
    };
    target.files.insert(
        target_path.clone(),
        make_entry("unchanged", Some(0o644), &target_path.display().to_string()),
    );

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert_eq!(diffs.len(), 1);
    assert!(
        matches!(&diffs[0].kind, FileDiffKind::Unchanged),
        "expected Unchanged diff, got: {:?}",
        diffs[0].kind
    );
}

#[test]
fn diff_created_when_in_source_not_target() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target_path = PathBuf::from("/home/user/.new_config");
    let source_path = PathBuf::from("/opt/cfgd/files/.new_config");

    let mut source = FileTree {
        files: BTreeMap::new(),
    };
    source.files.insert(
        target_path.clone(),
        make_entry("newhash", Some(0o644), &source_path.display().to_string()),
    );

    let target = FileTree {
        files: BTreeMap::new(),
    };

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert_eq!(diffs.len(), 1);
    assert!(
        matches!(
            &diffs[0].kind,
            FileDiffKind::Created { source: s } if *s == source_path
        ),
        "expected Created diff, got: {:?}",
        diffs[0].kind
    );
}

#[test]
fn diff_deleted_when_in_target_not_source() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target_path = PathBuf::from("/home/user/.old_config");

    let source = FileTree {
        files: BTreeMap::new(),
    };

    let mut target = FileTree {
        files: BTreeMap::new(),
    };
    target.files.insert(
        target_path.clone(),
        make_entry("oldhash", Some(0o644), &target_path.display().to_string()),
    );

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert_eq!(diffs.len(), 1);
    assert!(
        matches!(&diffs[0].kind, FileDiffKind::Deleted),
        "expected Deleted diff, got: {:?}",
        diffs[0].kind
    );
    assert_eq!(diffs[0].target, target_path);
}

#[test]
fn diff_mixed_all_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let mut source = FileTree {
        files: BTreeMap::new(),
    };
    source.files.insert(
        PathBuf::from("/a"),
        make_entry("new", Some(0o644), "/src/a"),
    );
    source.files.insert(
        PathBuf::from("/b"),
        make_entry("same", Some(0o644), "/src/b"),
    );
    source.files.insert(
        PathBuf::from("/c"),
        make_entry("created", Some(0o644), "/src/c"),
    );

    let mut target = FileTree {
        files: BTreeMap::new(),
    };
    target
        .files
        .insert(PathBuf::from("/a"), make_entry("old", Some(0o644), "/a"));
    target
        .files
        .insert(PathBuf::from("/b"), make_entry("same", Some(0o644), "/b"));
    target.files.insert(
        PathBuf::from("/d"),
        make_entry("deleted", Some(0o644), "/d"),
    );

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert_eq!(diffs.len(), 4, "expected 4 diffs, got: {diffs:?}");

    let kinds: Vec<_> = diffs.iter().map(|d| (&d.target, &d.kind)).collect();
    let a = Path::new("/a");
    let b = Path::new("/b");
    let c = Path::new("/c");
    let d = Path::new("/d");
    assert!(
        kinds
            .iter()
            .any(|(t, k)| **t == a && matches!(k, FileDiffKind::Modified { .. })),
        "expected /a to be Modified"
    );
    assert!(
        kinds
            .iter()
            .any(|(t, k)| **t == b && matches!(k, FileDiffKind::Unchanged)),
        "expected /b to be Unchanged"
    );
    assert!(
        kinds
            .iter()
            .any(|(t, k)| **t == c && matches!(k, FileDiffKind::Created { .. })),
        "expected /c to be Created"
    );
    assert!(
        kinds
            .iter()
            .any(|(t, k)| **t == d && matches!(k, FileDiffKind::Deleted)),
        "expected /d to be Deleted"
    );
}

#[test]
fn diff_permissions_none_source_produces_no_entry() {
    // When source has None permissions and target has Some, the permissions
    // differ (None != Some), but the PermissionsChanged branch requires both
    // to be Some. The if-let fails, so NO diff entry is produced for this file.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target_path = PathBuf::from("/home/user/.config");

    let mut source = FileTree {
        files: BTreeMap::new(),
    };
    source
        .files
        .insert(target_path.clone(), make_entry("eq", None, "/src"));

    let mut target = FileTree {
        files: BTreeMap::new(),
    };
    target.files.insert(
        target_path.clone(),
        make_entry("eq", Some(0o644), &target_path.display().to_string()),
    );

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    // The file is silently skipped: perms differ but both must be Some for a diff
    assert!(
        diffs.is_empty(),
        "expected no diff when source perms is None, got: {diffs:?}"
    );
}

#[test]
fn diff_empty_trees_produces_empty_diffs() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = FileTree {
        files: BTreeMap::new(),
    };
    let target = FileTree {
        files: BTreeMap::new(),
    };

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert!(diffs.is_empty());
}

// -----------------------------------------------------------------------
// Direct apply() trait tests for underexplored FileAction branches
// (Delete, SetPermissions, Skip, TOCTOU SourceChanged) +
// scan_source / scan_target + ensure_target_writable readonly cases.
// -----------------------------------------------------------------------

#[test]
fn scan_target_returns_hashed_entries() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.txt");
    fs::write(&f1, "alpha").unwrap();
    fs::write(&f2, "beta").unwrap();

    let tree = <CfgdFileManager as cfgd_core::providers::FileManager>::scan_target(
        &fm,
        &[f1.clone(), f2.clone()],
    )
    .unwrap();

    assert_eq!(tree.files.len(), 2);
    let entry1 = tree.files.get(&f1).expect("entry for a.txt");
    assert_eq!(entry1.content_hash, cfgd_core::sha256_hex(b"alpha"));
    assert_eq!(entry1.origin_source, "local");
    let entry2 = tree.files.get(&f2).expect("entry for b.txt");
    assert_eq!(entry2.content_hash, cfgd_core::sha256_hex(b"beta"));
}

#[test]
fn scan_target_skips_nonexistent_paths() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let real = dir.path().join("present.txt");
    fs::write(&real, "x").unwrap();
    let missing = dir.path().join("does-not-exist.txt");

    let tree = <CfgdFileManager as cfgd_core::providers::FileManager>::scan_target(
        &fm,
        &[real.clone(), missing],
    )
    .unwrap();

    assert_eq!(tree.files.len(), 1);
    assert!(tree.files.contains_key(&real));
}

#[test]
fn scan_source_recursively_walks_directories() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source_root = dir.path().join("src");
    fs::create_dir_all(source_root.join("nested/deep")).unwrap();
    fs::write(source_root.join("top.txt"), "1").unwrap();
    fs::write(source_root.join("nested/mid.txt"), "2").unwrap();
    fs::write(source_root.join("nested/deep/leaf.txt"), "3").unwrap();

    let layer = FileLayer {
        source_dir: source_root.clone(),
        origin_source: "test-origin".to_string(),
        priority: 100,
    };

    let tree =
        <CfgdFileManager as cfgd_core::providers::FileManager>::scan_source(&fm, &[layer]).unwrap();

    assert_eq!(tree.files.len(), 3);
    // Keys are paths relative to source_root
    assert!(tree.files.contains_key(Path::new("top.txt")));
    assert!(tree.files.contains_key(Path::new("nested/mid.txt")));
    assert!(tree.files.contains_key(Path::new("nested/deep/leaf.txt")));
    // Origin propagates from the layer
    assert_eq!(
        tree.files.get(Path::new("top.txt")).unwrap().origin_source,
        "test-origin"
    );
}

#[test]
fn scan_source_skips_nonexistent_layer() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let layer = FileLayer {
        source_dir: dir.path().join("does-not-exist"),
        origin_source: "missing".to_string(),
        priority: 1,
    };

    let tree =
        <CfgdFileManager as cfgd_core::providers::FileManager>::scan_source(&fm, &[layer]).unwrap();
    assert!(tree.files.is_empty());
}

#[test]
#[cfg(unix)]
fn scan_source_skips_symlinks_in_source_tree() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source_root = dir.path().join("src");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(source_root.join("real.txt"), "real").unwrap();

    // Symlink target outside the source tree (or anywhere) — must be skipped
    let outside = dir.path().join("outside.txt");
    fs::write(&outside, "outside").unwrap();
    symlink(&outside, source_root.join("link.txt")).unwrap();

    let layer = FileLayer {
        source_dir: source_root,
        origin_source: "local".to_string(),
        priority: 1,
    };

    let tree =
        <CfgdFileManager as cfgd_core::providers::FileManager>::scan_source(&fm, &[layer]).unwrap();
    assert_eq!(tree.files.len(), 1);
    assert!(tree.files.contains_key(Path::new("real.txt")));
    assert!(
        !tree.files.contains_key(Path::new("link.txt")),
        "symlink should be skipped"
    );
}

#[test]
fn apply_delete_removes_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target = dir.path().join("doomed.txt");
    fs::write(&target, "bye").unwrap();
    assert!(target.exists());

    let actions = vec![FileAction::Delete {
        target: target.clone(),
        origin: "local".to_string(),
    }];
    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    assert!(!target.exists());
}

#[test]
fn apply_delete_when_target_missing_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target = dir.path().join("never-existed.txt");
    let actions = vec![FileAction::Delete {
        target: target.clone(),
        origin: "local".to_string(),
    }];
    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    assert!(!target.exists());
}

#[test]
#[cfg(unix)]
fn apply_delete_removes_dangling_symlink() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    // Symlink to a missing target — `target.exists()` is false, but
    // `symlink_metadata().is_ok()` is true, so the Delete branch should fire.
    let link = dir.path().join("dangling");
    symlink(dir.path().join("nope"), &link).unwrap();
    assert!(!link.exists());
    assert!(link.symlink_metadata().is_ok());

    let actions = vec![FileAction::Delete {
        target: link.clone(),
        origin: "local".to_string(),
    }];
    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    assert!(
        link.symlink_metadata().is_err(),
        "dangling symlink should be removed"
    );
}

#[test]
#[cfg(unix)]
fn apply_set_permissions_changes_mode() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target = dir.path().join("perms.txt");
    fs::write(&target, "x").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

    let actions = vec![FileAction::SetPermissions {
        target: target.clone(),
        mode: 0o600,
        origin: "local".to_string(),
    }];
    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn apply_skip_action_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    // Ensure the Skip arm doesn't touch the target path
    let target = dir.path().join("not-touched.txt");
    let actions = vec![FileAction::Skip {
        target: target.clone(),
        reason: "private file: source missing".to_string(),
        origin: "local".to_string(),
    }];
    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    assert!(!target.exists());
}

#[test]
fn apply_create_with_stale_source_hash_returns_source_changed() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "current content").unwrap();
    let target = dir.path().join("out").join("dst.txt");

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        // Hash of completely different content — TOCTOU check must fail
        source_hash: Some(cfgd_core::sha256_hex(b"different content from plan time")),
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("expected SourceChanged error");
    let msg = err.to_string();
    assert!(
        msg.contains("source") && (msg.contains("changed") || msg.contains("modified")),
        "expected SourceChanged-style error, got: {msg}"
    );
    assert!(!target.exists(), "target should not have been written");
}

#[test]
fn apply_create_with_matching_source_hash_writes_target() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    let body = "hello toctou";
    fs::write(&source, body).unwrap();
    let target = dir.path().join("out").join("dst.txt");

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: Some(cfgd_core::sha256_hex(body.as_bytes())),
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), body);
}

#[test]
#[cfg(unix)]
fn ensure_target_writable_allows_readonly_existing_target_in_writable_parent() {
    // A 0444 target is still replaceable: apply does remove_file(target) then
    // recreates via the parent (symlink/hardlink/atomic_write), so the target
    // file's own mode never gates the write — only the parent's writability does.
    // The old mode-bit check wrongly rejected this.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("ro.txt");
    fs::write(&target, "x").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();

    ensure_target_writable(&target)
        .expect("a 0444 target inside a writable parent is replaceable, so this must succeed");

    // Restore perms so tempdir cleanup works on all platforms
    let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o644));
}

#[test]
#[cfg(unix)]
fn ensure_target_writable_errors_on_readonly_parent_directory() {
    // A 0555 parent has no write bit. For a non-root process that cannot create
    // an entry there, the probe must reject the target. Root bypasses DAC and
    // genuinely can write, so it is asserted by the no-write-bit test below.
    if cfgd_core::is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("ro_parent");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();

    let target = parent.join("dst.txt");
    let err =
        ensure_target_writable(&target).expect_err("readonly parent should reject new target");
    let msg = err.to_string();
    assert!(
        msg.contains("not writable") || msg.contains("writable"),
        "expected writability error, got: {msg}"
    );

    // Restore perms so tempdir cleanup succeeds.
    let _ = fs::set_permissions(&parent, fs::Permissions::from_mode(0o755));
}

#[test]
#[cfg(unix)]
fn ensure_target_writable_probes_real_access_not_mode_bits() {
    // Regression for the dnf5/Fedora case: Fedora's /root is mode 0550 (no write
    // bit) yet root writes there fine, while `Permissions::readonly()` reports it
    // read-only. The check must reflect the kernel's real access decision, not the
    // mode bits — so the outcome is privilege-dependent and asserted both ways.
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("no_write_bit");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o550)).unwrap();
    let target = parent.join("f.txt");

    let res = ensure_target_writable(&target);
    if cfgd_core::is_root() {
        assert!(
            res.is_ok(),
            "root can create entries in a 0550 dir; the check must not reject it: {res:?}"
        );
    } else {
        // The test process owns `parent`, but 0550 grants the owner no write bit,
        // so it genuinely cannot create a child entry.
        assert!(
            res.is_err(),
            "a non-root owner of a 0550 dir cannot create entries there"
        );
    }

    let _ = fs::set_permissions(&parent, fs::Permissions::from_mode(0o755));
}

#[test]
#[cfg(unix)]
fn apply_create_with_symlink_strategy_creates_symbolic_link() {
    // FileStrategy::Symlink arm in apply.rs ~158-164. The source file is a
    // real file; the target should be a symlink pointing to source.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("real.txt");
    fs::write(&source, "linked content").unwrap();
    let target = dir.path().join("nested").join("alias.txt");

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Symlink,
        source_hash: None,
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    let meta = target.symlink_metadata().expect("target should exist");
    assert!(meta.file_type().is_symlink(), "target must be a symlink");
    // Following the symlink reads the source's content.
    assert_eq!(fs::read_to_string(&target).unwrap(), "linked content");
}

#[test]
#[cfg(unix)]
fn apply_create_with_hardlink_strategy_creates_hard_link() {
    // FileStrategy::Hardlink arm in apply.rs ~166-170. Inode equality proves
    // the second path is a hard link to the first, not a copy.
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("orig.txt");
    fs::write(&source, "shared content").unwrap();
    let target = dir.path().join("nested").join("dup.txt");

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Hardlink,
        source_hash: None,
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    let src_meta = fs::metadata(&source).unwrap();
    let tgt_meta = fs::metadata(&target).unwrap();
    assert_eq!(
        src_meta.ino(),
        tgt_meta.ino(),
        "hardlink must share an inode with the source"
    );
    assert_eq!(
        tgt_meta.nlink(),
        2,
        "expected exactly 2 links after hardlink"
    );
}

#[test]
#[cfg(unix)]
fn apply_create_with_symlink_strategy_replaces_existing_file() {
    // The "remove existing target before deploying" arm (apply.rs ~150-154):
    // existing real file at target must be removed before the symlink is
    // created — otherwise create_symlink would fail with EEXIST.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "new content").unwrap();
    let target = dir.path().join("target.txt");
    fs::write(&target, "stale content").unwrap();
    assert!(target.exists());

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Symlink,
        source_hash: None,
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer).unwrap();

    let meta = target.symlink_metadata().unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "existing file should have been replaced with a symlink"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "new content");
}

#[test]
fn scan_target_io_error_when_path_is_a_directory() {
    // scan_target.read_to_string Err arm (apply.rs ~41-44). When the caller
    // hands a directory path that exists, fs::read_to_string fails — the Err
    // must wrap the path that triggered the failure.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let dir_path = dir.path().join("not-a-file");
    fs::create_dir(&dir_path).unwrap();

    let err = <CfgdFileManager as cfgd_core::providers::FileManager>::scan_target(
        &fm,
        std::slice::from_ref(&dir_path),
    )
    .expect_err("reading a directory as a file should error");
    let msg = err.to_string();
    assert!(
        msg.contains("not-a-file") || msg.contains("directory") || msg.contains("Is a directory"),
        "expected directory-related error, got: {msg}"
    );
}

#[test]
fn apply_create_non_local_origin_writes_file() {
    // Exercises the `file_origin = Some(origin.as_str())` arm (apply.rs ~140)
    // when origin != "local".  Uses a plain (non-template) file so the Copy
    // path is taken and no source-context lookup is required.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "content from remote source").unwrap();
    let target = dir.path().join("out").join("dst.txt");

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "remote-source".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: None,
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
        .expect("apply with non-local origin must succeed");

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "content from remote source"
    );
}

#[test]
fn apply_update_non_local_origin_writes_file() {
    // Same coverage target as the create variant above, but via the Update arm.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "updated remote content").unwrap();
    let target = dir.path().join("dst.txt");
    fs::write(&target, "old content").unwrap();

    let actions = vec![FileAction::Update {
        source: source.clone(),
        target: target.clone(),
        origin: "team-source".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: None,
        diff: String::new(),
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
        .expect("apply Update with non-local origin must succeed");

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "updated remote content"
    );
}

#[test]
fn apply_create_copy_with_source_directory_returns_io_error() {
    // Exercises the `fs::read_to_string` error closure (apply.rs ~177-179):
    // when the source path is a directory (not a .tera template), read_to_string
    // returns "Is a directory" and the error is wrapped in FileError::Io.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source_dir = dir.path().join("not-a-file");
    fs::create_dir(&source_dir).unwrap();
    let target = dir.path().join("out").join("dst.txt");

    let actions = vec![FileAction::Create {
        source: source_dir.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: None,
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("applying Create with a directory as source must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("not-a-file") || msg.contains("directory") || msg.contains("Is a directory"),
        "expected IO error mentioning the path, got: {msg}"
    );
}

#[test]
fn apply_update_copy_with_source_directory_returns_io_error() {
    // Same as the Create variant above but via the Update arm, ensuring
    // the same error closure fires for both action kinds.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source_dir = dir.path().join("src-dir");
    fs::create_dir(&source_dir).unwrap();
    let target = dir.path().join("dst.txt");
    fs::write(&target, "old").unwrap();

    let actions = vec![FileAction::Update {
        source: source_dir.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: None,
        diff: String::new(),
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("applying Update with a directory as source must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("src-dir") || msg.contains("directory") || msg.contains("Is a directory"),
        "expected IO error mentioning the path, got: {msg}"
    );
}

#[test]
fn apply_create_with_secret_ref_resolved_by_provider() {
    // Exercises the `content.contains("${secret:")` branch (apply.rs ~199-206).
    // A mock SecretProvider resolves the inline secret reference so the final
    // file contains the plaintext value.
    use cfgd_core::errors::Result as CfgdResult;
    use cfgd_core::providers::SecretProvider;
    use secrecy::SecretString;

    struct MockProvider {
        secret: String,
    }
    impl SecretProvider for MockProvider {
        fn name(&self) -> &str {
            "1password"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn resolve(&self, _reference: &str) -> CfgdResult<SecretString> {
            Ok(SecretString::from(self.secret.clone()))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let mut fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();
    fm.set_secret_providers(
        None,
        vec![Box::new(MockProvider {
            secret: "s3cr3t_value".to_string(),
        })],
    );

    let source = dir.path().join("template.txt");
    fs::write(&source, "token=${secret:1password://Vault/Item/Field}").unwrap();
    let target = dir.path().join("out").join("config.txt");

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: None,
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
        .expect("apply with secret ref must succeed");

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "token=s3cr3t_value",
        "secret ref must be resolved in the output file"
    );
}

#[test]
fn ensure_target_writable_errors_when_parent_component_is_a_file() {
    // Exercises `fs::create_dir_all` error (apply.rs ~306-308): when an
    // intermediate path component is a regular file, create_dir_all cannot
    // create the directory and returns an error.
    let dir = tempfile::tempdir().unwrap();
    let blocking_file = dir.path().join("not-a-dir");
    fs::write(&blocking_file, "i am a file").unwrap();

    // Target path whose parent requires treating `not-a-dir` as a directory.
    let target = blocking_file.join("child.txt");

    let err = ensure_target_writable(&target)
        .expect_err("create_dir_all must fail when an intermediate path component is a file");
    let msg = err.to_string();
    assert!(
        msg.contains("not-a-dir") || msg.contains("Not a directory") || msg.contains("IO error"),
        "expected create_dir_all IO error, got: {msg}"
    );
}

#[test]
fn apply_create_with_update_action_source_hash_present() {
    // Exercises the `FileAction::Update { source_hash, .. }` arm of the
    // expected_hash extraction (apply.rs ~184-185) when source_hash is Some
    // and the content matches — TOCTOU check passes, file is written.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let body = "update toctou content";
    let source = dir.path().join("src.txt");
    fs::write(&source, body).unwrap();
    let target = dir.path().join("dst.txt");
    fs::write(&target, "old content").unwrap();

    let actions = vec![FileAction::Update {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: Some(cfgd_core::sha256_hex(body.as_bytes())),
        diff: String::new(),
    }];

    let printer = test_printer();
    <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
        .expect("apply Update with matching source_hash must succeed");

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        body,
        "target must contain updated content"
    );
}

#[test]
fn apply_update_with_stale_source_hash_returns_source_changed() {
    // Exercises the `FileAction::Update { source_hash, .. }` stale-hash path
    // (apply.rs ~184-195): when source_hash is Some but doesn't match the
    // current content, SourceChanged is returned.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "current content").unwrap();
    let target = dir.path().join("dst.txt");
    fs::write(&target, "old content").unwrap();

    let actions = vec![FileAction::Update {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: Some(cfgd_core::sha256_hex(b"stale content from plan time")),
        diff: String::new(),
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("stale source_hash in Update must return SourceChanged");
    let msg = err.to_string();
    assert!(
        msg.contains("source") && (msg.contains("changed") || msg.contains("modified")),
        "expected SourceChanged-style error for Update, got: {msg}"
    );
}

#[test]
fn scan_source_multiple_layers_second_wins_for_same_relative_path() {
    // Exercises scan_source with two layers where both contain the same
    // relative filename: the second layer's entry must overwrite the first
    // because BTreeMap::insert replaces on duplicate key.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let layer1_root = dir.path().join("layer1");
    let layer2_root = dir.path().join("layer2");
    fs::create_dir_all(&layer1_root).unwrap();
    fs::create_dir_all(&layer2_root).unwrap();

    fs::write(layer1_root.join("common.txt"), "from layer1").unwrap();
    fs::write(layer2_root.join("common.txt"), "from layer2").unwrap();
    fs::write(layer1_root.join("only-in-layer1.txt"), "unique").unwrap();

    let layers = vec![
        FileLayer {
            source_dir: layer1_root.clone(),
            origin_source: "layer1".to_string(),
            priority: 100,
        },
        FileLayer {
            source_dir: layer2_root.clone(),
            origin_source: "layer2".to_string(),
            priority: 200,
        },
    ];

    let tree =
        <CfgdFileManager as cfgd_core::providers::FileManager>::scan_source(&fm, &layers).unwrap();

    assert_eq!(
        tree.files.len(),
        2,
        "common.txt should be deduplicated; only-in-layer1.txt is unique"
    );
    let common = tree
        .files
        .get(std::path::Path::new("common.txt"))
        .expect("common.txt must be in tree");
    assert_eq!(
        common.origin_source, "layer2",
        "second layer must overwrite first for the same relative path"
    );
    assert_eq!(
        common.content_hash,
        cfgd_core::sha256_hex(b"from layer2"),
        "content hash must match layer2's content"
    );
}

#[test]
fn scan_source_empty_layers_returns_empty_tree() {
    // Exercises scan_source with no layers at all — the loop body is never
    // entered and an empty FileTree is returned immediately.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let tree =
        <CfgdFileManager as cfgd_core::providers::FileManager>::scan_source(&fm, &[]).unwrap();
    assert!(
        tree.files.is_empty(),
        "empty layer list must yield empty tree"
    );
}

#[test]
fn diff_permissions_changed_ignored_when_either_side_is_none() {
    // Exercises the `if let (Some(desired), Some(current))` guard (apply.rs ~81-83):
    // when one or both sides have no Unix permissions (None), the diff falls
    // through to the Unchanged arm rather than emitting PermissionsChanged.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target_path = std::path::PathBuf::from("/home/user/.cfg");

    let mut source = FileTree {
        files: BTreeMap::new(),
    };
    // permissions: None (Windows-style entry)
    source.files.insert(
        target_path.clone(),
        make_entry("samehash", None, "/opt/cfgd/files/.cfg"),
    );

    let mut target = FileTree {
        files: BTreeMap::new(),
    };
    // Different None permissions — hashes match so we enter the permissions arm,
    // but `if let (Some, Some)` fails and Unchanged is emitted.
    target.files.insert(
        target_path.clone(),
        make_entry("samehash", None, &target_path.display().to_string()),
    );

    let diffs = trait_diff(&fm, &source, &target).unwrap();
    assert_eq!(diffs.len(), 1);
    assert!(
        matches!(&diffs[0].kind, FileDiffKind::Unchanged),
        "None permissions on both sides must yield Unchanged, got: {:?}",
        diffs[0].kind
    );
}

#[test]
fn apply_create_when_target_is_directory_returns_io_error() {
    // Exercises the `fs::remove_file` error closure (apply.rs ~152-154):
    // when the target path exists as a directory, `remove_file` fails with
    // EISDIR (Is a directory), which wraps into FileError::Io.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "content").unwrap();
    let target = dir.path().join("target-dir");
    fs::create_dir(&target).unwrap();

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: None,
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("Create with a directory at the target path must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("target-dir") || msg.contains("directory") || msg.contains("Is a directory"),
        "expected EISDIR error for directory target, got: {msg}"
    );
}

#[test]
fn apply_delete_when_target_is_directory_returns_io_error() {
    // Exercises the `fs::remove_file` error closure in the Delete arm
    // (apply.rs ~221-223): `remove_file` on a directory fails with EISDIR.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let target = dir.path().join("not-a-file-dir");
    fs::create_dir(&target).unwrap();

    let actions = vec![FileAction::Delete {
        target: target.clone(),
        origin: "local".to_string(),
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("Delete with a directory at the target path must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("not-a-file-dir")
            || msg.contains("directory")
            || msg.contains("Is a directory"),
        "expected EISDIR error when deleting a directory, got: {msg}"
    );
}

#[test]
#[cfg(unix)]
fn apply_create_symlink_with_too_long_filename_returns_io_error() {
    // Exercises the `create_symlink` error closure (apply.rs ~160-164):
    // a filename component that exceeds NAME_MAX (255 bytes on Linux) causes
    // the underlying `symlink` syscall to return ENAMETOOLONG.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "data").unwrap();
    let long_name = "x".repeat(256);
    let target = dir.path().join(long_name);

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Symlink,
        source_hash: None,
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("symlink creation with name > NAME_MAX must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("too long")
            || msg.contains("ENAMETOOLONG")
            || msg.contains("File name too long"),
        "expected ENAMETOOLONG error, got: {msg}"
    );
}

#[test]
#[cfg(unix)]
fn apply_create_hardlink_with_too_long_filename_returns_io_error() {
    // Exercises the `hard_link` error closure (apply.rs ~168-170):
    // a filename component exceeding NAME_MAX returns ENAMETOOLONG.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("orig.txt");
    fs::write(&source, "data").unwrap();
    let long_name = "y".repeat(256);
    let target = dir.path().join(long_name);

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Hardlink,
        source_hash: None,
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("hard_link with name > NAME_MAX must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("too long")
            || msg.contains("ENAMETOOLONG")
            || msg.contains("File name too long"),
        "expected ENAMETOOLONG error, got: {msg}"
    );
}

#[test]
#[cfg(unix)]
fn apply_create_copy_with_too_long_target_filename_returns_io_error() {
    // Exercises the `atomic_write` error closure (apply.rs ~210-214):
    // atomic_write creates a temp file next to the target, then renames it.
    // A filename component exceeding NAME_MAX fails at the temp-file creation
    // step, returning ENAMETOOLONG wrapped as FileError::Io.
    let dir = tempfile::tempdir().unwrap();
    let resolved = make_resolved_profile(vec![], FilesSpec::default());
    let fm = CfgdFileManager::new(dir.path(), &resolved).unwrap();

    let source = dir.path().join("src.txt");
    fs::write(&source, "content").unwrap();
    let long_name = "z".repeat(256);
    let target = dir.path().join(long_name);

    let actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: cfgd_core::config::FileStrategy::Copy,
        source_hash: None,
    }];

    let printer = test_printer();
    let err =
        <CfgdFileManager as cfgd_core::providers::FileManager>::apply(&fm, &actions, &printer)
            .expect_err("atomic_write with name > NAME_MAX must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("too long")
            || msg.contains("ENAMETOOLONG")
            || msg.contains("File name too long"),
        "expected ENAMETOOLONG error for atomic_write, got: {msg}"
    );
}

#[test]
fn file_drift_one_missing_source_reports_non_matching() {
    // A managed source that does not exist on disk must surface as a
    // non-matching drift result (not an error), so one bad entry never masks
    // drift on sibling files.
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();
    let resolved = make_resolved_profile(
        vec![],
        FilesSpec {
            managed: vec![],
            permissions: HashMap::new(),
        },
    );
    let fm = CfgdFileManager::new(config_dir, &resolved).unwrap();

    let missing_source = config_dir.join("files").join("does-not-exist.txt");
    let target = config_dir.join("target").join("out.txt");
    let result = fm.file_drift_one(&missing_source, &target, None).unwrap();

    assert!(!result.matches, "missing source must be non-matching");
    assert!(
        result.actual.contains("source not found"),
        "actual must explain the missing source, got: {}",
        result.actual
    );
    assert_eq!(result.expected, "managed source present");
}
