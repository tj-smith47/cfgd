use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::parse::parse_config;
use super::resolve::merge_layers;
use super::*;
use crate::PathDisplayExt;
use crate::deep_merge_yaml;
use crate::errors::ConfigError;
use crate::test_helpers::{SAMPLE_CONFIG_NO_ORIGIN_YAML, SAMPLE_CONFIG_YAML, SAMPLE_PROFILE_YAML};

#[test]
fn parse_yaml_config() {
    let config = parse_config(SAMPLE_CONFIG_YAML, Path::new("cfgd.yaml")).unwrap();
    assert_eq!(config.metadata.name, "test-config");
    assert_eq!(config.spec.profile.as_deref(), Some("default"));
    assert_eq!(config.spec.origin.len(), 1);
    assert_eq!(
        config.spec.origin[0].url,
        "https://github.com/test/repo.git"
    );
    assert_eq!(config.spec.origin[0].branch, "master");
}

#[test]
fn parse_config_without_origin() {
    let config = parse_config(SAMPLE_CONFIG_NO_ORIGIN_YAML, Path::new("cfgd.yaml")).unwrap();
    assert!(config.spec.origin.is_empty());
    assert!(config.spec.sources.is_empty());
}

#[test]
fn parse_config_rejects_unknown_apiversion() {
    let yaml = "apiVersion: cfgd.io/v1alpha2\nkind: Config\nmetadata:\n  name: m\nspec:\n  profile: default\n";
    let err = parse_config(yaml, Path::new("cfgd.yaml")).unwrap_err();
    assert!(err.to_string().contains("apiVersion"));
    assert!(err.to_string().contains("cfgd.io/v1alpha1")); // names the supported version
}

#[test]
fn load_profile_rejects_unknown_apiversion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default.yaml");
    std::fs::write(
        &path,
        "apiVersion: cfgd.io/v1alpha2\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();
    let err = load_profile(&path).unwrap_err();
    assert!(err.to_string().contains("apiVersion"));
    assert!(err.to_string().contains("cfgd.io/v1alpha1")); // names the supported version
}

#[test]
fn parse_profile_yaml() {
    let doc: ProfileDocument = serde_yaml::from_str(SAMPLE_PROFILE_YAML).unwrap();
    assert_eq!(doc.metadata.name, "base");
    assert_eq!(doc.spec.env.len(), 2);
    let pkgs = doc.spec.packages.as_ref().unwrap();
    let brew = pkgs.brew.as_ref().unwrap();
    assert_eq!(brew.formulae, vec!["ripgrep", "fd"]);
    assert_eq!(pkgs.cargo.as_ref().unwrap().packages, vec!["bat"]);
}

#[test]
fn merge_env_override() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            env: vec![
                EnvVar {
                    name: "editor".into(),
                    value: "vim".into(),
                },
                EnvVar {
                    name: "shell".into(),
                    value: "/bin/bash".into(),
                },
            ],
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            env: vec![EnvVar {
                name: "editor".into(),
                value: "code".into(),
            }],
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    assert_eq!(
        merged
            .env
            .iter()
            .find(|e| e.name == "editor")
            .map(|e| &e.value),
        Some(&"code".to_string())
    );
    assert_eq!(
        merged
            .env
            .iter()
            .find(|e| e.name == "shell")
            .map(|e| &e.value),
        Some(&"/bin/bash".to_string())
    );
}

#[test]
fn merge_packages_union() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into(), "exa".into()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    assert_eq!(
        merged.packages.cargo.as_ref().unwrap().packages,
        vec!["bat", "exa"]
    );
}

#[test]
fn merge_files_overlay() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "base/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "work/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    assert_eq!(merged.files.managed.len(), 1);
    assert_eq!(merged.files.managed[0].source, "work/.zshrc");
}

#[test]
fn deep_merge_yaml_maps() {
    let mut base = serde_yaml::from_str::<serde_yaml::Value>(
        r#"
            domain1:
              key1: value1
              key2: value2
            "#,
    )
    .unwrap();

    let overlay = serde_yaml::from_str::<serde_yaml::Value>(
        r#"
            domain1:
              key2: overridden
              key3: value3
            "#,
    )
    .unwrap();

    deep_merge_yaml(&mut base, &overlay);

    let map = base.as_mapping().unwrap();
    let domain = map
        .get(serde_yaml::Value::String("domain1".into()))
        .unwrap()
        .as_mapping()
        .unwrap();
    assert_eq!(
        domain.get(serde_yaml::Value::String("key1".into())),
        Some(&serde_yaml::Value::String("value1".into()))
    );
    assert_eq!(
        domain.get(serde_yaml::Value::String("key2".into())),
        Some(&serde_yaml::Value::String("overridden".into()))
    );
    assert_eq!(
        domain.get(serde_yaml::Value::String("key3".into())),
        Some(&serde_yaml::Value::String("value3".into()))
    );
}

#[test]
fn profile_resolution_with_filesystem() {
    let dir = tempfile::tempdir().unwrap();

    // Create base profile
    std::fs::write(
        dir.path().join("base.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  env:
    - name: editor
      value: vim
  packages:
    cargo:
      - bat
"#,
    )
    .unwrap();

    // Create work profile inheriting base
    std::fs::write(
        dir.path().join("work.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - base
  env:
    - name: editor
      value: code
  packages:
    cargo:
      - exa
"#,
    )
    .unwrap();

    let resolved = resolve_profile("work", dir.path()).unwrap();

    assert_eq!(resolved.layers.len(), 2);
    assert_eq!(resolved.layers[0].profile_name, "base");
    assert_eq!(resolved.layers[1].profile_name, "work");

    // editor should be overridden by work
    assert_eq!(
        resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "editor")
            .map(|e| &e.value),
        Some(&"code".to_string())
    );
    // packages should be unioned
    assert_eq!(
        resolved.merged.packages.cargo.as_ref().unwrap().packages,
        vec!["bat", "exa"]
    );
}

#[test]
fn circular_inheritance_detected() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("a.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: a
spec:
  inherits:
    - b
"#,
    )
    .unwrap();

    std::fs::write(
        dir.path().join("b.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: b
spec:
  inherits:
    - a
"#,
    )
    .unwrap();

    let result = resolve_profile("a", dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("circular"));
}

#[test]
fn config_not_found_error() {
    let result = load_config(Path::new("/nonexistent/cfgd.yaml"));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn parse_config_source_manifest() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme-corp-dev
  version: "2.1.0"
  description: "ACME Corp developer environment"
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
  policy:
    required:
      packages:
        brew:
          formulae:
            - git-secrets
            - pre-commit
    recommended:
      packages:
        brew:
          formulae:
            - k9s
      env:
        - name: EDITOR
          value: "code --wait"
    locked:
      files:
        - source: "security/policy.yaml"
          target: "~/.config/company/security-policy.yaml"
    constraints:
      noScripts: true
      noSecretsRead: true
      allowedTargetPaths:
        - "~/.config/acme/"
        - "~/.eslintrc*"
"#;
    let doc = parse_config_source(yaml).unwrap();
    assert_eq!(doc.metadata.name, "acme-corp-dev");
    assert_eq!(doc.metadata.version.as_deref(), Some("2.1.0"));
    assert_eq!(doc.spec.provides.profiles.len(), 2);

    let required_pkgs = doc.spec.policy.required.packages.as_ref().unwrap();
    let brew = required_pkgs.brew.as_ref().unwrap();
    assert_eq!(brew.formulae, vec!["git-secrets", "pre-commit"]);

    assert!(doc.spec.policy.constraints.no_scripts);
    assert_eq!(doc.spec.policy.constraints.allowed_target_paths.len(), 2);
    assert_eq!(doc.spec.policy.locked.files.len(), 1);
}

#[test]
fn parse_config_source_wrong_kind() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: not-a-source
spec:
  provides:
    profiles: []
  policy: {}
"#;
    let result = parse_config_source(yaml);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("ConfigSource"));
}

#[test]
fn source_spec_defaults() {
    let yaml = r#"
name: test-source
origin:
  type: Git
  url: https://example.com/config.git
"#;
    let spec: SourceSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.subscription.priority, 500);
    assert_eq!(spec.sync.interval, "1h");
    assert!(!spec.sync.auto_apply);
}

#[test]
fn cargo_spec_deserialize_list() {
    // Dual-form lives on the `PackagesSpec::cargo` field, the real consumer
    // path — not on `CargoSpec` itself.
    let yaml = r#"
cargo:
  - bat
  - ripgrep
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
    let cargo = spec.cargo.unwrap();
    assert_eq!(cargo.packages, vec!["bat", "ripgrep"]);
    assert!(cargo.file.is_none());
}

#[test]
fn cargo_spec_deserialize_map() {
    let yaml = r#"
cargo:
  file: Cargo.toml
  packages:
    - extra-pkg
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
    let cargo = spec.cargo.unwrap();
    assert_eq!(cargo.file.as_deref(), Some("Cargo.toml"));
    assert_eq!(cargo.packages, vec!["extra-pkg"]);
}

#[test]
fn cargo_spec_deserialize_file_only() {
    let yaml = r#"
cargo:
  file: Cargo.toml
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
    let cargo = spec.cargo.unwrap();
    assert_eq!(cargo.file.as_deref(), Some("Cargo.toml"));
    assert!(cargo.packages.is_empty());
}

#[test]
fn packages_spec_with_manifest_files() {
    let yaml = r#"
brew:
  file: Brewfile
  formulae:
    - extra-tool
apt:
  file: packages.apt.txt
npm:
  file: package.json
  global:
    - extra-global
cargo:
  file: Cargo.toml
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();

    let brew = spec.brew.as_ref().unwrap();
    assert_eq!(brew.file.as_deref(), Some("Brewfile"));
    assert_eq!(brew.formulae, vec!["extra-tool"]);

    let apt = spec.apt.as_ref().unwrap();
    assert_eq!(apt.file.as_deref(), Some("packages.apt.txt"));

    let npm = spec.npm.as_ref().unwrap();
    assert_eq!(npm.file.as_deref(), Some("package.json"));
    assert_eq!(npm.global, vec!["extra-global"]);

    let cargo = spec.cargo.as_ref().unwrap();
    assert_eq!(cargo.file.as_deref(), Some("Cargo.toml"));
}

#[test]
fn merge_manifest_file_fields() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                brew: Some(BrewSpec {
                    file: Some("Brewfile".into()),
                    formulae: vec!["git".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec!["ripgrep".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    let brew = merged.packages.brew.as_ref().unwrap();
    // file from base preserved (layer2 didn't override)
    assert_eq!(brew.file.as_deref(), Some("Brewfile"));
    // formulae unioned
    assert_eq!(brew.formulae, vec!["git", "ripgrep"]);
}

#[test]
fn parse_config_source_with_profile_details() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
    profileDetails:
      - name: acme-base
        description: "Core tools and security"
        path: profiles/base.yaml
      - name: acme-backend
        description: "Go, k8s tools"
        path: profiles/backend.yaml
        inherits:
          - acme-base
    platformProfiles:
      macos: acme-base
      debian: acme-backend
  policy: {}
"#;
    let doc = parse_config_source(yaml).unwrap();
    assert_eq!(doc.spec.provides.profile_details.len(), 2);
    assert_eq!(doc.spec.provides.profile_details[0].name, "acme-base");
    assert_eq!(
        doc.spec.provides.profile_details[0].description.as_deref(),
        Some("Core tools and security")
    );
    assert_eq!(
        doc.spec.provides.profile_details[1].inherits,
        vec!["acme-base"]
    );
    assert_eq!(doc.spec.provides.platform_profiles.len(), 2);
    assert_eq!(
        doc.spec.provides.platform_profiles.get("macos").unwrap(),
        "acme-base"
    );
}

#[test]
fn parse_os_release_debian() {
    let content = r#"PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"
NAME="Debian GNU/Linux"
VERSION_ID="12"
ID=debian
"#;
    let fields = crate::platform::parse_os_release_content(content);
    assert_eq!(
        fields.get("ID").map(|v| v.to_lowercase()).as_deref(),
        Some("debian")
    );
    assert_eq!(fields.get("VERSION_ID").map(|s| s.as_str()), Some("12"));
}

#[test]
fn parse_os_release_ubuntu() {
    let content = r#"NAME="Ubuntu"
VERSION="22.04.3 LTS (Jammy Jellyfish)"
ID=ubuntu
VERSION_ID="22.04"
"#;
    let fields = crate::platform::parse_os_release_content(content);
    assert_eq!(
        fields.get("ID").map(|v| v.to_lowercase()).as_deref(),
        Some("ubuntu")
    );
    assert_eq!(fields.get("VERSION_ID").map(|s| s.as_str()), Some("22.04"));
}

#[test]
fn parse_os_release_empty() {
    let fields = crate::platform::parse_os_release_content("");
    assert!(!fields.contains_key("ID"));
    assert!(!fields.contains_key("VERSION_ID"));
}

#[test]
fn match_platform_profile_exact_distro() {
    let mut profiles = HashMap::new();
    profiles.insert("macos".into(), "profiles/macos.yaml".into());
    profiles.insert("debian".into(), "profiles/debian.yaml".into());

    let platform = PlatformInfo {
        os: "linux".into(),
        distro: Some("debian".into()),
        distro_version: Some("12".into()),
    };
    assert_eq!(
        match_platform_profile(&platform, &profiles),
        Some("profiles/debian.yaml".into())
    );
}

#[test]
fn match_platform_profile_os_fallback() {
    let mut profiles = HashMap::new();
    profiles.insert("macos".into(), "profiles/macos.yaml".into());
    profiles.insert("linux".into(), "profiles/linux.yaml".into());

    let platform = PlatformInfo {
        os: "linux".into(),
        distro: Some("arch".into()),
        distro_version: None,
    };
    // No "arch" key, falls back to "linux"
    assert_eq!(
        match_platform_profile(&platform, &profiles),
        Some("profiles/linux.yaml".into())
    );
}

#[test]
fn match_platform_profile_no_match() {
    let mut profiles = HashMap::new();
    profiles.insert("debian".into(), "profiles/debian.yaml".into());

    let platform = PlatformInfo {
        os: "macos".into(),
        distro: None,
        distro_version: None,
    };
    assert!(match_platform_profile(&platform, &profiles).is_none());
}

#[test]
fn source_profile_names_from_details() {
    let provides = ConfigSourceProvides {
        profiles: vec!["old-name".into()],
        profile_details: vec![
            ConfigSourceProfileEntry {
                name: "base".into(),
                description: Some("Base profile".into()),
                path: None,
                inherits: vec![],
            },
            ConfigSourceProfileEntry {
                name: "backend".into(),
                description: None,
                path: None,
                inherits: vec!["base".into()],
            },
        ],
        platform_profiles: HashMap::new(),
        modules: vec![],
    };
    // profile_details takes precedence
    assert_eq!(source_profile_names(&provides), vec!["base", "backend"]);
}

#[test]
fn source_profile_names_fallback_to_profiles() {
    let provides = ConfigSourceProvides {
        profiles: vec!["alpha".into(), "beta".into()],
        profile_details: vec![],
        platform_profiles: HashMap::new(),
        modules: vec![],
    };
    assert_eq!(source_profile_names(&provides), vec!["alpha", "beta"]);
}

#[test]
fn auto_apply_policy_deserializes() {
    let yaml = r#"
newRecommended: Accept
newOptional: Notify
lockedConflict: Reject
"#;
    let policy: AutoApplyPolicyConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(policy.new_recommended, PolicyAction::Accept);
    assert_eq!(policy.new_optional, PolicyAction::Notify);
    assert_eq!(policy.locked_conflict, PolicyAction::Reject);
}

#[test]
fn reconcile_config_with_policy_deserializes() {
    let yaml = r#"
interval: 5m
onChange: true
autoApply: true
policy:
  newRecommended: Accept
  newOptional: Ignore
  lockedConflict: Notify
"#;
    let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.auto_apply);
    assert!(config.on_change);
    let policy = config.policy.unwrap();
    assert_eq!(policy.new_recommended, PolicyAction::Accept);
}

#[test]
fn reconcile_patches_deserialize() {
    let yaml = r#"
interval: 5m
patches:
  - kind: Module
    name: certificates
    interval: 1m
    driftPolicy: Auto
  - kind: Profile
    name: work
    autoApply: true
  - kind: Module
    interval: 30s
"#;
    let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.patches.len(), 3);
    assert_eq!(config.patches[0].kind, ReconcilePatchKind::Module);
    assert_eq!(config.patches[0].name.as_deref(), Some("certificates"));
    assert_eq!(config.patches[0].interval.as_deref(), Some("1m"));
    assert_eq!(config.patches[0].drift_policy, Some(DriftPolicy::Auto));
    assert!(config.patches[0].auto_apply.is_none());
    assert_eq!(config.patches[1].kind, ReconcilePatchKind::Profile);
    assert_eq!(config.patches[1].name.as_deref(), Some("work"));
    assert_eq!(config.patches[1].auto_apply, Some(true));
    assert!(config.patches[1].interval.is_none());
    // Kind-wide patch (no name)
    assert_eq!(config.patches[2].kind, ReconcilePatchKind::Module);
    assert!(config.patches[2].name.is_none());
    assert_eq!(config.patches[2].interval.as_deref(), Some("30s"));
}

#[test]
fn reconcile_config_without_patches_has_empty_vec() {
    let yaml = "interval: 10m\n";
    let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.patches.is_empty());
}

// --- desired_packages_for_spec ---

#[test]
fn desired_packages_brew_formulae() {
    let spec = PackagesSpec {
        brew: Some(BrewSpec {
            formulae: vec!["curl".into(), "wget".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("brew", &spec),
        vec!["curl", "wget"]
    );
}

#[test]
fn desired_packages_brew_taps() {
    let spec = PackagesSpec {
        brew: Some(BrewSpec {
            taps: vec!["homebrew/core".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("brew-tap", &spec),
        vec!["homebrew/core"]
    );
}

#[test]
fn desired_packages_brew_casks() {
    let spec = PackagesSpec {
        brew: Some(BrewSpec {
            casks: vec!["firefox".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("brew-cask", &spec),
        vec!["firefox"]
    );
}

#[test]
fn desired_packages_apt() {
    let spec = PackagesSpec {
        apt: Some(AptSpec {
            packages: vec!["git".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("apt", &spec), vec!["git"]);
}

#[test]
fn desired_packages_cargo() {
    let spec = PackagesSpec {
        cargo: Some(CargoSpec {
            packages: vec!["ripgrep".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("cargo", &spec), vec!["ripgrep"]);
}

#[test]
fn desired_packages_npm() {
    let spec = PackagesSpec {
        npm: Some(NpmSpec {
            global: vec!["typescript".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("npm", &spec), vec!["typescript"]);
}

#[test]
fn desired_packages_pipx() {
    let spec = PackagesSpec {
        pipx: vec!["black".into()],
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("pipx", &spec), vec!["black"]);
}

#[test]
fn desired_packages_snap_merges_classic() {
    let spec = PackagesSpec {
        snap: Some(SnapSpec {
            packages: vec!["core".into()],
            classic: vec!["code".into()],
        }),
        ..Default::default()
    };
    let result = desired_packages_for_spec("snap", &spec);
    assert_eq!(result, vec!["core", "code"]);
}

#[test]
fn desired_packages_snap_classic_dedup() {
    let spec = PackagesSpec {
        snap: Some(SnapSpec {
            packages: vec!["code".into()],
            classic: vec!["code".into()],
        }),
        ..Default::default()
    };
    let result = desired_packages_for_spec("snap", &spec);
    assert_eq!(result, vec!["code"]);
}

#[test]
fn desired_packages_custom_manager() {
    let spec = PackagesSpec {
        custom: vec![CustomManagerSpec {
            name: "my-mgr".into(),
            check: "which my-mgr".into(),
            list_installed: "my-mgr list".into(),
            install: "my-mgr install".into(),
            uninstall: "my-mgr remove".into(),
            update: None,
            packages: vec!["tool-a".into()],
        }],
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("my-mgr", &spec), vec!["tool-a"]);
}

#[test]
fn desired_packages_unknown_manager() {
    let spec = PackagesSpec::default();
    assert!(desired_packages_for_spec("nonexistent", &spec).is_empty());
}

#[test]
fn desired_packages_winget() {
    let spec = PackagesSpec {
        winget: vec!["Microsoft.VisualStudioCode".into(), "Git.Git".into()],
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("winget", &spec),
        vec!["Microsoft.VisualStudioCode", "Git.Git"]
    );
}

#[test]
fn desired_packages_chocolatey() {
    let spec = PackagesSpec {
        chocolatey: vec!["nodejs".into()],
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("chocolatey", &spec),
        vec!["nodejs"]
    );
}

#[test]
fn desired_packages_scoop() {
    let spec = PackagesSpec {
        scoop: vec!["ripgrep".into()],
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("scoop", &spec), vec!["ripgrep"]);
}

// --- load_config filesystem ---

#[test]
fn load_config_valid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cfgd.yaml");
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n".to_string();
    std::fs::write(&path, &yaml).unwrap();
    let cfg = load_config(&path).unwrap();
    assert_eq!(cfg.metadata.name, "test");
}

#[test]
fn load_config_valid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cfgd.toml");
    let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"test\"\n\n[spec]\nprofile = \"default\"\n";
    std::fs::write(&path, toml).unwrap();
    let cfg = load_config(&path).unwrap();
    assert_eq!(cfg.metadata.name, "test");
}

#[test]
fn load_config_missing_file() {
    let result = load_config(std::path::Path::new("/nonexistent-12345/cfgd.yaml"));
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected 'config file not found' in error, got: {msg}"
    );
    assert!(
        msg.contains("/nonexistent-12345/cfgd.yaml"),
        "expected path in error, got: {msg}"
    );
}

#[test]
fn load_config_dir_infers_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: from-yaml\nspec:\n  profile: default\n";
    std::fs::write(dir.path().join(CONFIG_FILENAME), yaml).unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.metadata.name, "from-yaml");
}

#[test]
fn load_config_dir_infers_toml() {
    let dir = tempfile::tempdir().unwrap();
    let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"from-toml\"\n\n[spec]\nprofile = \"default\"\n";
    std::fs::write(dir.path().join(CONFIG_FILENAME_TOML), toml).unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.metadata.name, "from-toml");
}

#[test]
fn load_config_dir_prefers_yaml_over_toml() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: from-yaml\nspec:\n  profile: default\n";
    let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"from-toml\"\n\n[spec]\nprofile = \"default\"\n";
    std::fs::write(dir.path().join(CONFIG_FILENAME), yaml).unwrap();
    std::fs::write(dir.path().join(CONFIG_FILENAME_TOML), toml).unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.metadata.name, "from-yaml");
}

#[test]
fn load_config_empty_dir_reports_yaml_filename() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_config(dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected 'config file not found' in error, got: {msg}"
    );
    assert!(
        msg.ends_with(CONFIG_FILENAME) || msg.contains(&format!("/{CONFIG_FILENAME}")),
        "expected error to name {CONFIG_FILENAME}, not the bare dir, got: {msg}"
    );
    assert!(
        !msg.contains("Is a directory"),
        "directory arg must not surface a raw OS read error, got: {msg}"
    );
}

// --- resolve_profile deeper inheritance ---

#[test]
fn test_ai_config_defaults() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec: {}
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    let ai = config.spec.ai.unwrap_or_default();
    assert_eq!(ai.provider, "claude");
    assert_eq!(ai.model, "claude-sonnet-4-6");
    assert_eq!(ai.api_key_env, "ANTHROPIC_API_KEY");
}

#[test]
fn test_ai_config_custom() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  ai:
    provider: claude
    model: claude-opus-4-6
    apiKeyEnv: MY_CLAUDE_KEY
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    let ai = config.spec.ai.unwrap_or_default();
    assert_eq!(ai.model, "claude-opus-4-6");
    assert_eq!(ai.api_key_env, "MY_CLAUDE_KEY");
}

#[test]
fn test_existing_config_without_ai_still_parses() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work
  theme: default
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.spec.ai.is_none());
}

#[test]
fn three_level_inheritance() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();

    let grandparent = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: grandparent\nspec:\n  inherits: []\n  modules: []\n  env:\n    - name: A\n      value: '1'\n";
    let parent = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: parent\nspec:\n  inherits:\n    - grandparent\n  modules: []\n  env:\n    - name: B\n      value: '2'\n";
    let child = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - parent\n  modules: []\n  env:\n    - name: C\n      value: '3'\n";

    std::fs::write(profiles.join("grandparent.yaml"), grandparent).unwrap();
    std::fs::write(profiles.join("parent.yaml"), parent).unwrap();
    std::fs::write(profiles.join("child.yaml"), child).unwrap();

    let resolved = resolve_profile("child", &profiles).unwrap();

    // Should have all three env vars merged
    let names: Vec<&str> = resolved
        .merged
        .env
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"B"));
    assert!(names.contains(&"C"));
}

#[test]
fn script_entry_deserialize_simple() {
    let yaml = r#""echo hello""#;
    let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
    match entry {
        ScriptEntry::Simple(s) => assert_eq!(s, "echo hello"),
        _ => panic!("expected Simple variant"),
    }
}

#[test]
fn script_entry_deserialize_full() {
    let yaml = r#"
run: scripts/check.sh
timeout: 30s
continueOnError: true
"#;
    let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
    match entry {
        ScriptEntry::Full {
            run,
            timeout,
            continue_on_error,
            ..
        } => {
            assert_eq!(run, "scripts/check.sh");
            assert_eq!(timeout, Some("30s".to_string()));
            assert_eq!(continue_on_error, Some(true));
        }
        _ => panic!("expected Full variant"),
    }
}

#[test]
fn script_spec_deserialize_all_hooks() {
    let yaml = r#"
preApply:
  - scripts/pre.sh
postApply:
  - run: scripts/post.sh
    timeout: 60s
preReconcile:
  - scripts/reconcile-pre.sh
postReconcile:
  - scripts/reconcile-post.sh
onDrift:
  - scripts/drift.sh
onChange:
  - run: systemctl restart myservice
    continueOnError: true
"#;
    let spec: ScriptSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.pre_apply.len(), 1);
    assert_eq!(spec.post_apply.len(), 1);
    assert_eq!(spec.pre_reconcile.len(), 1);
    assert_eq!(spec.post_reconcile.len(), 1);
    assert_eq!(spec.on_drift.len(), 1);
    assert_eq!(spec.on_change.len(), 1);
}

#[test]
fn script_spec_backward_compat_empty() {
    let yaml = "{}";
    let spec: ScriptSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.pre_apply.is_empty());
    assert!(spec.post_apply.is_empty());
    assert!(spec.pre_reconcile.is_empty());
    assert!(spec.post_reconcile.is_empty());
    assert!(spec.on_drift.is_empty());
    assert!(spec.on_change.is_empty());
}

// --- Encryption types ---

#[test]
fn encryption_mode_default_is_in_repo() {
    let mode = EncryptionMode::default();
    assert_eq!(mode, EncryptionMode::InRepo);
}

#[test]
fn managed_file_spec_encryption_in_repo() {
    let yaml = r#"
source: dotfiles/.zshrc
target: ~/.zshrc
encryption:
  backend: sops
  mode: InRepo
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let enc = spec.encryption.expect("encryption should be Some");
    assert_eq!(enc.backend, "sops");
    assert_eq!(enc.mode, EncryptionMode::InRepo);
}

#[test]
fn managed_file_spec_encryption_always() {
    let yaml = r#"
source: secrets/.env
target: ~/.env
encryption:
  backend: age
  mode: Always
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let enc = spec.encryption.expect("encryption should be Some");
    assert_eq!(enc.backend, "age");
    assert_eq!(enc.mode, EncryptionMode::Always);
}

#[test]
fn managed_file_spec_no_encryption() {
    let yaml = r#"
source: dotfiles/.bashrc
target: ~/.bashrc
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.encryption.is_none());
}

#[test]
fn managed_file_spec_permissions() {
    let yaml = r#"
source: dotfiles/.ssh/config
target: ~/.ssh/config
permissions: "600"
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.permissions.as_deref(), Some("600"));
}

#[test]
fn managed_file_spec_permissions_absent() {
    let yaml = r#"
source: dotfiles/.vimrc
target: ~/.vimrc
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.permissions.is_none());
}

#[test]
fn source_constraints_encryption() {
    let yaml = r#"
noScripts: true
noSecretsRead: true
allowedTargetPaths: []
allowSystemChanges: false
requireSignedCommits: false
encryption:
  requiredTargets:
    - "~/.ssh/*"
    - "~/.gnupg/*"
  backend: sops
  mode: InRepo
"#;
    let sc: SourceConstraints = serde_yaml::from_str(yaml).unwrap();
    let enc = sc.encryption.expect("encryption should be Some");
    assert_eq!(enc.required_targets.len(), 2);
    assert_eq!(enc.required_targets[0], "~/.ssh/*");
    assert_eq!(enc.required_targets[1], "~/.gnupg/*");
    assert_eq!(enc.backend.as_deref(), Some("sops"));
    assert_eq!(enc.mode, Some(EncryptionMode::InRepo));
}

#[test]
fn source_constraints_no_encryption_defaults_none() {
    let sc = SourceConstraints::default();
    assert!(sc.encryption.is_none());
}

#[test]
fn source_constraints_encryption_required_targets_only() {
    let yaml = r#"
encryption:
  requiredTargets:
    - "~/.aws/credentials"
"#;
    let sc: SourceConstraints = serde_yaml::from_str(yaml).unwrap();
    let enc = sc.encryption.expect("encryption should be Some");
    assert_eq!(enc.required_targets.len(), 1);
    assert!(enc.backend.is_none());
    assert!(enc.mode.is_none());
}

#[test]
fn module_file_entry_with_encryption() {
    let yaml = r#"
source: files/.gitconfig
target: ~/.gitconfig
encryption:
  backend: sops
  mode: InRepo
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    let enc = entry.encryption.expect("encryption should be Some");
    assert_eq!(enc.backend, "sops");
    assert_eq!(enc.mode, EncryptionMode::InRepo);
}

#[test]
fn module_file_entry_no_encryption() {
    let yaml = r#"
source: files/.tmux.conf
target: ~/.tmux.conf
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    assert!(entry.encryption.is_none());
}

#[test]
fn module_file_entry_permissions() {
    let yaml = r#"
source: files/git-helper
target: ~/.local/bin/git-helper
permissions: "755"
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(entry.permissions.as_deref(), Some("755"));
}

#[test]
fn module_file_entry_permissions_absent() {
    let yaml = r#"
source: files/.tmux.conf
target: ~/.tmux.conf
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    assert!(entry.permissions.is_none());
}

#[test]
fn encryption_spec_mode_defaults_to_in_repo_when_omitted() {
    let yaml = r#"
backend: sops
"#;
    let spec: EncryptionSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.backend, "sops");
    assert_eq!(spec.mode, EncryptionMode::InRepo);
}

#[test]
fn secret_spec_with_envs_only() {
    let yaml = r#"
source: op://vault/item/password
envs:
  - DB_PASSWORD
"#;
    let spec: SecretSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.source, "op://vault/item/password");
    assert!(spec.target.is_none());
    assert_eq!(spec.envs.as_ref().unwrap(), &["DB_PASSWORD"]);
}

#[test]
fn secret_spec_with_target_and_envs() {
    let yaml = r#"
source: secrets/api-key.enc
target: ~/.config/app/key
envs:
  - API_KEY
"#;
    let spec: SecretSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.source, "secrets/api-key.enc");
    assert_eq!(
        spec.target.unwrap(),
        std::path::PathBuf::from("~/.config/app/key")
    );
    assert_eq!(spec.envs.as_ref().unwrap(), &["API_KEY"]);
}

#[test]
fn secret_spec_with_target_only() {
    let yaml = r#"
source: secrets/credentials.enc
target: ~/.config/app/credentials
"#;
    let spec: SecretSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.source, "secrets/credentials.enc");
    assert_eq!(
        spec.target.unwrap(),
        std::path::PathBuf::from("~/.config/app/credentials")
    );
    assert!(spec.envs.is_none());
}

#[test]
fn secret_spec_neither_target_nor_envs_fails_validation() {
    let specs = vec![SecretSpec {
        source: "secrets/orphan.enc".to_string(),
        target: None,
        template: None,
        backend: None,
        envs: None,
    }];
    let result = validate_secret_specs(&specs);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must have at least one of 'target' or 'envs'"),
        "unexpected error: {}",
        err_msg
    );
}

#[test]
fn secret_spec_validation_passes_with_target() {
    let specs = vec![SecretSpec {
        source: "secrets/key.enc".to_string(),
        target: Some(std::path::PathBuf::from("~/.ssh/key")),
        template: None,
        backend: None,
        envs: None,
    }];
    validate_secret_specs(&specs).expect("validation should pass for spec with target");
    assert_eq!(specs[0].source, "secrets/key.enc");
    assert_eq!(
        specs[0].target.as_deref(),
        Some(std::path::Path::new("~/.ssh/key"))
    );
    assert!(specs[0].envs.is_none());
}

#[test]
fn secret_spec_validation_passes_with_envs() {
    let specs = vec![SecretSpec {
        source: "op://vault/item".to_string(),
        target: None,
        template: None,
        backend: None,
        envs: Some(vec!["SECRET_KEY".to_string()]),
    }];
    validate_secret_specs(&specs).expect("validation should pass for spec with envs");
    assert_eq!(specs[0].source, "op://vault/item");
    assert!(specs[0].target.is_none());
    let envs = specs[0].envs.as_ref().expect("envs should be Some");
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0], "SECRET_KEY");
}

#[test]
fn policy_items_with_secrets() {
    let yaml = r#"
secrets:
  - source: op://vault/db/password
    envs:
      - DB_PASSWORD
  - source: secrets/tls.enc
    target: /etc/tls/cert.pem
"#;
    let items: PolicyItems = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(items.secrets.len(), 2);
    assert_eq!(items.secrets[0].source, "op://vault/db/password");
    assert!(items.secrets[0].target.is_none());
    assert_eq!(items.secrets[0].envs.as_ref().unwrap(), &["DB_PASSWORD"]);
    assert_eq!(items.secrets[1].source, "secrets/tls.enc");
    assert_eq!(
        items.secrets[1].target.as_ref().unwrap(),
        &std::path::PathBuf::from("/etc/tls/cert.pem")
    );
    assert!(items.secrets[1].envs.is_none());
}

#[test]
fn policy_items_default_has_empty_secrets() {
    let items = PolicyItems::default();
    assert!(items.secrets.is_empty());
}

#[test]
fn module_spec_system_field_deserializes() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: git-setup
spec:
  system:
    git:
      user.name: Jane Doe
      user.email: jane@example.com
    sshKeys:
      - path: ~/.ssh/id_ed25519.pub
        comment: jane@example.com
"#;
    let doc: ModuleDocument = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(doc.spec.system.len(), 2);
    assert!(doc.spec.system.contains_key("git"));
    assert!(doc.spec.system.contains_key("sshKeys"));
    let git_val = &doc.spec.system["git"];
    assert_eq!(
        git_val["user.name"],
        serde_yaml::Value::String("Jane Doe".into())
    );
    assert_eq!(
        git_val["user.email"],
        serde_yaml::Value::String("jane@example.com".into())
    );
}

#[test]
fn module_spec_system_defaults_to_empty() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  packages:
    - name: neovim
"#;
    let doc: ModuleDocument = serde_yaml::from_str(yaml).unwrap();
    assert!(doc.spec.system.is_empty());
}

#[test]
fn module_system_merges_into_profile_system() {
    // Simulate what plan_system does: deep-merge module system over profile system.
    let profile_yaml = r#"
git:
  user.name: Old Name
  user.signingkey: ABC123
"#;
    let module_yaml = r#"
git:
  user.name: New Name
  user.email: new@example.com
"#;
    let mut profile_system: HashMap<String, serde_yaml::Value> =
        serde_yaml::from_str(profile_yaml).unwrap();
    let module_system: HashMap<String, serde_yaml::Value> =
        serde_yaml::from_str(module_yaml).unwrap();

    // Apply module system on top of profile system (same logic as plan_system)
    for (key, value) in &module_system {
        crate::deep_merge_yaml(
            profile_system
                .entry(key.clone())
                .or_insert(serde_yaml::Value::Null),
            value,
        );
    }

    let git = &profile_system["git"];
    // Module overrides: user.name updated
    assert_eq!(
        git["user.name"],
        serde_yaml::Value::String("New Name".into())
    );
    // Module adds: user.email
    assert_eq!(
        git["user.email"],
        serde_yaml::Value::String("new@example.com".into())
    );
    // Profile value preserved when module doesn't mention it
    assert_eq!(
        git["user.signingkey"],
        serde_yaml::Value::String("ABC123".into())
    );
}

#[test]
fn module_system_overrides_profile_on_conflict() {
    let mut profile_system: HashMap<String, serde_yaml::Value> = {
        let mut m = HashMap::new();
        m.insert(
            "git".to_string(),
            serde_yaml::from_str("user.name: Profile Name").unwrap(),
        );
        m
    };
    let module_system: HashMap<String, serde_yaml::Value> = {
        let mut m = HashMap::new();
        m.insert(
            "git".to_string(),
            serde_yaml::from_str("user.name: Module Name").unwrap(),
        );
        m
    };

    for (key, value) in &module_system {
        crate::deep_merge_yaml(
            profile_system
                .entry(key.clone())
                .or_insert(serde_yaml::Value::Null),
            value,
        );
    }

    assert_eq!(
        profile_system["git"]["user.name"],
        serde_yaml::Value::String("Module Name".into())
    );
}

// ---------------------------------------------------------------------------
// ComplianceConfig tests
// ---------------------------------------------------------------------------

#[test]
fn parse_full_compliance_config() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  compliance:
    enabled: true
    interval: 30m
    retention: 90d
    scope:
      files: true
      packages: false
      system: true
      secrets: false
      watchPaths:
        - /etc
        - /usr/local
      watchPackageManagers:
        - brew
        - apt
    export:
      format: Yaml
      path: /var/lib/cfgd/compliance/
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    let compliance = config.spec.compliance.as_ref().unwrap();
    assert!(compliance.enabled);
    assert_eq!(compliance.interval, "30m");
    assert_eq!(compliance.retention, "90d");
    assert!(compliance.scope.files);
    assert!(!compliance.scope.packages);
    assert!(compliance.scope.system);
    assert!(!compliance.scope.secrets);
    assert_eq!(compliance.scope.watch_paths, vec!["/etc", "/usr/local"]);
    assert_eq!(compliance.scope.watch_package_managers, vec!["brew", "apt"]);
    assert_eq!(compliance.export.format, ComplianceFormat::Yaml);
    assert_eq!(compliance.export.path, "/var/lib/cfgd/compliance/");
}

#[test]
fn parse_compliance_defaults_from_enabled_only() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  compliance:
    enabled: true
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    let compliance = config.spec.compliance.as_ref().unwrap();
    assert!(compliance.enabled);
    assert_eq!(compliance.interval, "1h");
    assert_eq!(compliance.retention, "30d");
    // scope bools default to true
    assert!(compliance.scope.files);
    assert!(compliance.scope.packages);
    assert!(compliance.scope.system);
    assert!(compliance.scope.secrets);
    assert!(compliance.scope.watch_paths.is_empty());
    assert!(compliance.scope.watch_package_managers.is_empty());
    // export defaults
    assert_eq!(compliance.export.format, ComplianceFormat::Json);
    assert_eq!(compliance.export.path, "~/.local/state/cfgd/compliance/");
}

#[test]
fn parse_compliance_watch_paths_and_managers() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  compliance:
    enabled: true
    scope:
      watchPaths:
        - /home/user/.config
      watchPackageManagers:
        - cargo
        - npm
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    let scope = &config.spec.compliance.as_ref().unwrap().scope;
    assert_eq!(scope.watch_paths, vec!["/home/user/.config"]);
    assert_eq!(scope.watch_package_managers, vec!["cargo", "npm"]);
}

#[test]
fn compliance_format_defaults_to_json() {
    let export = ComplianceExport::default();
    assert_eq!(export.format, ComplianceFormat::Json);
}

#[test]
fn parse_complete_cfgd_yaml_with_compliance() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: workstation
spec:
  profile: default
  compliance:
    enabled: true
    interval: 1h
    retention: 30d
    scope:
      files: true
      packages: true
      system: true
      secrets: true
    export:
      format: Json
      path: ~/.local/state/cfgd/compliance/
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    assert_eq!(config.spec.profile.as_deref(), Some("default"));
    let compliance = config.spec.compliance.as_ref().unwrap();
    assert!(compliance.enabled);
    assert_eq!(compliance.interval, "1h");
    assert_eq!(compliance.retention, "30d");
    assert!(compliance.scope.files);
    assert!(compliance.scope.packages);
    assert!(compliance.scope.system);
    assert!(compliance.scope.secrets);
    assert_eq!(compliance.export.format, ComplianceFormat::Json);
    assert_eq!(compliance.export.path, "~/.local/state/cfgd/compliance/");
}

#[test]
fn compliance_absent_when_not_specified() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    assert!(config.spec.compliance.is_none());
}

#[test]
fn yaml_anchor_limit_rejects_bomb() {
    // Generate a YAML document with excessive anchors
    let mut yaml = String::from("apiVersion: cfgd.io/v1alpha1\nkind: Config\n");
    for i in 0..300 {
        yaml.push_str(&format!("key{}: &anchor{} value{}\n", i, i, i));
    }
    let result = parse_config(&yaml, std::path::Path::new("bomb.yaml"));
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("too many YAML anchors")
    );
}

#[test]
fn yaml_anchor_limit_accepts_normal_config() {
    // A normal config with a few anchors should be fine
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#;
    let result = parse_config(yaml, std::path::Path::new("cfgd.yaml"));
    assert!(
        result.is_ok(),
        "normal config should parse: {:?}",
        result.err()
    );
    let config = result.unwrap();
    assert_eq!(config.metadata.name, "test");
    assert_eq!(config.spec.profile.as_deref(), Some("default"));
    assert_eq!(config.api_version, "cfgd.io/v1alpha1");
    assert_eq!(config.kind, "Config");
    assert!(config.spec.origin.is_empty(), "no origins configured");
    assert!(config.spec.sources.is_empty(), "no sources configured");
}

#[test]
fn env_var_rejects_invalid_name_at_deserialization() {
    let yaml = r#"
name: "MY_VAR; rm -rf /"
value: "safe"
"#;
    let result: std::result::Result<EnvVar, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("invalid env var name")
    );
}

#[test]
fn env_var_accepts_valid_name_at_deserialization() {
    let yaml = r#"
name: "MY_VAR_123"
value: "hello"
"#;
    let ev: EnvVar = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(ev.name, "MY_VAR_123");
    assert_eq!(ev.value, "hello");
}

#[test]
fn alias_rejects_invalid_name_at_deserialization() {
    let yaml = r#"
name: "my alias; evil"
command: "ls -la"
"#;
    let result: std::result::Result<ShellAlias, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("invalid alias name")
    );
}

#[test]
fn alias_accepts_valid_name_at_deserialization() {
    let yaml = r#"
name: "ll"
command: "ls -la"
"#;
    let alias: ShellAlias = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(alias.name, "ll");
    assert_eq!(alias.command, "ls -la");
}

// --- Legacy theme-override key warnings ---

#[test]
#[serial_test::serial]
#[tracing_test::traced_test]
fn legacy_theme_subheader_emits_warning() {
    let yaml = r##"
spec:
  theme:
    name: dracula
    overrides:
      subheader: "#ff79c6"
"##;
    super::parse::warn_on_legacy_theme_keys(yaml);
    assert!(
        logs_contain("theme.overrides.subheader is no longer supported"),
        "expected legacy-key warning to fire"
    );
}

#[test]
#[serial_test::serial]
#[tracing_test::traced_test]
fn legacy_icon_success_emits_rename_warning() {
    let yaml = r##"
spec:
  theme:
    name: dracula
    overrides:
      iconSuccess: "++"
"##;
    super::parse::warn_on_legacy_theme_keys(yaml);
    assert!(
        logs_contain("iconSuccess is renamed to iconOk"),
        "expected rename warning to fire"
    );
}

#[test]
#[serial_test::serial]
#[tracing_test::traced_test]
fn modern_overrides_emit_no_warning() {
    let yaml = r##"
spec:
  theme:
    name: dracula
    overrides:
      iconOk: "✔"
      running: "#00ff00"
"##;
    super::parse::warn_on_legacy_theme_keys(yaml);
    assert!(!logs_contain("no longer supported"));
    assert!(!logs_contain("renamed to"));
}

/// Resolve the primary package list a bare-list form should populate, for a
/// given manager field name, so the table test can assert both forms agree.
fn primary_list_for(spec: &PackagesSpec, manager: &str) -> Vec<String> {
    match manager {
        "brew" => spec.brew.as_ref().map(|b| b.formulae.clone()),
        "apt" => spec.apt.as_ref().map(|a| a.packages.clone()),
        "cargo" => spec.cargo.as_ref().map(|c| c.packages.clone()),
        "npm" => spec.npm.as_ref().map(|n| n.global.clone()),
        "snap" => spec.snap.as_ref().map(|s| s.packages.clone()),
        "flatpak" => spec.flatpak.as_ref().map(|f| f.packages.clone()),
        other => spec.simple_list(other).map(|s| s.to_vec()),
    }
    .unwrap_or_default()
}

/// Every manager — the 12 bare-`Vec<String>` ones and the 6 struct-backed ones
/// — must accept BOTH `manager: [a, b]` and `manager: {packages|global|formulae: [a, b]}`
/// and resolve to the identical package set. This makes the historical
/// three-shapes inconsistency unrepresentable: no manager may accept only one form.
#[test]
fn every_manager_accepts_list_and_struct_forms_identically() {
    // (field, struct-key-for-the-primary-list). Struct key is None for the
    // bare-Vec managers (their map form uses `packages:`).
    let struct_managers: &[(&str, &str)] = &[
        ("brew", "formulae"),
        ("apt", "packages"),
        ("cargo", "packages"),
        ("npm", "global"),
        ("snap", "packages"),
        ("flatpak", "packages"),
    ];
    let bare_managers: &[&str] = &[
        "pipx",
        "dnf",
        "apk",
        "pacman",
        "zypper",
        "yum",
        "pkg",
        "nix",
        "go",
        "winget",
        "chocolatey",
        "scoop",
    ];

    for (field, key) in struct_managers {
        let list_yaml = format!("{field}: [alpha, beta]\n");
        let struct_yaml = format!("{field}:\n  {key}: [alpha, beta]\n");
        let from_list: PackagesSpec = serde_yaml::from_str(&list_yaml)
            .unwrap_or_else(|e| panic!("{field} list form must parse: {e}"));
        let from_struct: PackagesSpec = serde_yaml::from_str(&struct_yaml)
            .unwrap_or_else(|e| panic!("{field} struct form must parse: {e}"));
        assert_eq!(
            primary_list_for(&from_list, field),
            vec!["alpha".to_string(), "beta".to_string()],
            "{field} list form resolved wrong set"
        );
        assert_eq!(
            primary_list_for(&from_list, field),
            primary_list_for(&from_struct, field),
            "{field} list and struct forms disagree"
        );
    }

    for field in bare_managers {
        let list_yaml = format!("{field}: [alpha, beta]\n");
        let struct_yaml = format!("{field}:\n  packages: [alpha, beta]\n");
        let from_list: PackagesSpec = serde_yaml::from_str(&list_yaml)
            .unwrap_or_else(|e| panic!("{field} list form must parse: {e}"));
        let from_struct: PackagesSpec = serde_yaml::from_str(&struct_yaml)
            .unwrap_or_else(|e| panic!("{field} struct (packages:) form must parse: {e}"));
        assert_eq!(
            primary_list_for(&from_list, field),
            vec!["alpha".to_string(), "beta".to_string()],
            "{field} list form resolved wrong set"
        );
        assert_eq!(
            primary_list_for(&from_list, field),
            primary_list_for(&from_struct, field),
            "{field} list and struct forms disagree"
        );
    }
}

/// `ALL_MANAGER_NAMES` must list exactly the names `desired_packages_for_spec`
/// handles via a dedicated match arm — never a name that silently falls through
/// to the custom-manager catch-all. A built-in name placed in `custom` must
/// resolve via its built-in arm (empty here, since the built-in field is empty),
/// NOT return the custom packages; an unknown name must hit the catch-all and
/// return the custom packages. This keeps the const and the match from drifting:
/// if `/add-package-manager` adds a name to the const without a match arm, the
/// built-in lookup would fall through and this test flips red.
#[test]
fn all_manager_names_resolve_via_builtin_arms() {
    for name in ALL_MANAGER_NAMES {
        let spec = PackagesSpec {
            custom: vec![custom_manager(name, "shadow")],
            ..Default::default()
        };
        let resolved = desired_packages_for_spec(name, &spec);
        assert!(
            resolved.is_empty(),
            "'{name}' is in ALL_MANAGER_NAMES but resolved to the custom-manager \
             fallthrough ({resolved:?}) — it has no built-in match arm in \
             desired_packages_for_spec"
        );
    }

    // Negative control: an unknown name must fall through to the custom lookup.
    let spec = PackagesSpec {
        custom: vec![custom_manager("definitely-not-a-builtin", "pkg-a")],
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("definitely-not-a-builtin", &spec),
        vec!["pkg-a".to_string()],
        "unknown manager names must resolve via the custom-manager catch-all"
    );
}

/// Minimal `CustomManagerSpec` with one package, for the manager-name drift guard.
fn custom_manager(name: &str, package: &str) -> CustomManagerSpec {
    CustomManagerSpec {
        name: name.to_string(),
        check: String::new(),
        list_installed: String::new(),
        install: String::new(),
        uninstall: String::new(),
        update: None,
        packages: vec![package.to_string()],
    }
}

/// The reported root-cause symptom: `flatpak: [app]` must now parse where it
/// previously errored with `invalid type: sequence, expected struct FlatpakSpec`.
#[test]
fn flatpak_accepts_bare_list_form() {
    let spec: PackagesSpec = serde_yaml::from_str("flatpak: [org.gnome.Calculator]\n")
        .expect("flatpak bare-list form must parse");
    assert_eq!(
        spec.flatpak.unwrap().packages,
        vec!["org.gnome.Calculator".to_string()]
    );
}

/// A bare-Vec manager given a map form with an unknown key must still error
/// (the map form preserves typo-detection; only `packages:` is accepted).
#[test]
fn bare_vec_manager_map_form_rejects_unknown_key() {
    let err = serde_yaml::from_str::<PackagesSpec>("nix:\n  pakages: [hello]\n")
        .expect_err("unknown key in nix map form must error");
    assert!(
        format!("{err}").contains("packages") || format!("{err}").contains("pakages"),
        "expected an unknown-key error, got: {err}"
    );
}

/// The struct-backed managers keep `deny_unknown_fields` on their map form.
#[test]
fn struct_manager_map_form_rejects_unknown_key() {
    let err = serde_yaml::from_str::<PackagesSpec>("flatpak:\n  packges: [x]\n")
        .expect_err("unknown key in flatpak map form must error");
    assert!(
        format!("{err}").contains("unknown field"),
        "expected unknown-field error, got: {err}"
    );
}

// --- Profile bundle layout: dual-read precedence + ambiguity ---

fn profile_yaml(name: &str, inherits: &[&str]) -> String {
    let inherits_block = if inherits.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = inherits.iter().map(|p| format!("    - {}", p)).collect();
        format!("  inherits:\n{}\n", items.join("\n"))
    };
    format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: {name}
spec:
{inherits_block}  env:
    - name: origin_{name}
      value: "1"
"#
    )
}

fn write_canonical_profile(profiles_dir: &Path, name: &str, inherits: &[&str]) -> PathBuf {
    let dir = profiles_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("profile.yaml");
    std::fs::write(&path, profile_yaml(name, inherits)).unwrap();
    path
}

fn write_legacy_profile(profiles_dir: &Path, filename: &str, name: &str) -> PathBuf {
    let path = profiles_dir.join(filename);
    std::fs::write(&path, profile_yaml(name, &[])).unwrap();
    path
}

#[test]
fn find_profile_path_canonical_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_canonical_profile(dir.path(), "work", &[]);
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), path);
}

#[test]
fn find_profile_path_legacy_yaml_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_legacy_profile(dir.path(), "work.yaml", "work");
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), path);
}

#[test]
fn find_profile_path_legacy_yml_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_legacy_profile(dir.path(), "work.yml", "work");
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), path);
}

#[test]
fn find_profile_path_missing_is_profile_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::ProfileNotFound { ref name } if name == "work"));
}

#[test]
fn find_profile_path_payload_dir_without_manifest_is_not_a_profile() {
    let dir = tempfile::tempdir().unwrap();
    // The current legacy shape: flat manifest + <name>/files/ payload dir.
    std::fs::create_dir_all(dir.path().join("work").join("files")).unwrap();
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), legacy);
}

#[test]
fn find_profile_path_canonical_plus_legacy_yaml_is_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { .. }));
    let msg = err.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&legacy.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_canonical_plus_legacy_yml_is_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { .. }), "{err}");
    let msg = err.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&legacy.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_legacy_yaml_plus_yml_is_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { .. }));
    let msg = err.to_string();
    assert!(msg.contains(&yaml.posix().to_string()), "{msg}");
    assert!(msg.contains(&yml.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_three_forms_names_every_path() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    match &err {
        ConfigError::AmbiguousProfile { name, paths } => {
            assert_eq!(name, "work");
            assert_eq!(paths, &vec![canonical.clone(), yaml.clone(), yml.clone()]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
    let msg = err.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&yaml.posix().to_string()), "{msg}");
    assert!(msg.contains(&yml.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_profile_named_profile_ranks_structurally() {
    // a bundle at profiles/profile/profile.yaml vs a flat profiles/profile.yaml:
    // canonical detection is structural (parent dir), not name-based, so the
    // pathological name 'profile' must still rank the bundle form first
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "profile", &[]);
    let flat = write_legacy_profile(dir.path(), "profile.yaml", "profile");
    let err = find_profile_path(dir.path(), "profile").unwrap_err();
    match &err {
        ConfigError::AmbiguousProfile { name, paths } => {
            assert_eq!(name, "profile");
            assert_eq!(paths, &vec![canonical, flat]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
}

#[test]
fn canonical_profile_path_is_pure_construction() {
    let path = canonical_profile_path(Path::new("/cfg/profiles"), "work");
    assert_eq!(path, Path::new("/cfg/profiles/work/profile.yaml"));
}

#[test]
fn resolve_profile_loads_canonical_form() {
    let dir = tempfile::tempdir().unwrap();
    write_canonical_profile(dir.path(), "work", &[]);
    let resolved = resolve_profile("work", dir.path()).unwrap();
    assert_eq!(resolved.layers.len(), 1);
    assert_eq!(resolved.layers[0].profile_name, "work");
}

#[test]
fn resolve_profile_inherits_across_both_forms() {
    let dir = tempfile::tempdir().unwrap();
    // legacy child inheriting a canonical parent
    write_canonical_profile(dir.path(), "base", &[]);
    std::fs::write(
        dir.path().join("work.yaml"),
        profile_yaml("work", &["base"]),
    )
    .unwrap();
    let resolved = resolve_profile("work", dir.path()).unwrap();
    assert_eq!(resolved.layers.len(), 2);
    assert_eq!(resolved.layers[0].profile_name, "base");
    assert_eq!(resolved.layers[1].profile_name, "work");

    // canonical child inheriting a legacy parent
    let dir2 = tempfile::tempdir().unwrap();
    write_legacy_profile(dir2.path(), "base.yaml", "base");
    write_canonical_profile(dir2.path(), "work", &["base"]);
    let resolved = resolve_profile("work", dir2.path()).unwrap();
    assert_eq!(resolved.layers.len(), 2);
    assert_eq!(resolved.layers[0].profile_name, "base");
    assert_eq!(resolved.layers[1].profile_name, "work");
}

#[test]
fn resolve_profile_ambiguous_parent_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    write_canonical_profile(dir.path(), "base", &[]);
    write_legacy_profile(dir.path(), "base.yaml", "base");
    std::fs::write(
        dir.path().join("work.yaml"),
        profile_yaml("work", &["base"]),
    )
    .unwrap();
    let err = resolve_profile("work", dir.path()).unwrap_err();
    assert!(err.to_string().contains("ambiguous profile 'base'"));
}

#[test]
fn scan_profiles_yields_both_forms_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy_profile(dir.path(), "zeta.yml", "zeta");
    let canonical = write_canonical_profile(dir.path(), "alpha", &[]);
    // payload dir of a legacy profile: not a profile entry
    std::fs::create_dir_all(dir.path().join("zeta-payload").join("files")).unwrap();
    // non-yaml noise: ignored
    std::fs::write(dir.path().join("README.md"), "x").unwrap();

    let entries = scan_profiles(dir.path()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "alpha");
    assert_eq!(entries[0].form, ProfileForm::Canonical);
    assert_eq!(entries[0].path, canonical);
    assert_eq!(entries[1].name, "zeta");
    assert_eq!(entries[1].form, ProfileForm::LegacyFlat);
    assert_eq!(entries[1].path, legacy);
}

#[test]
fn scan_profiles_ambiguous_name_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    write_canonical_profile(dir.path(), "work", &[]);
    write_legacy_profile(dir.path(), "work.yaml", "work");
    let err = scan_profiles(dir.path()).unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { ref name, .. } if name == "work"));
    assert!(err.to_string().contains("delete or rename one"));
}

#[test]
fn scan_profiles_legacy_extension_dupe_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    write_legacy_profile(dir.path(), "work.yaml", "work");
    write_legacy_profile(dir.path(), "work.yml", "work");
    let err = scan_profiles(dir.path()).unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { ref name, .. } if name == "work"));
}

#[test]
fn scan_profiles_ambiguity_paths_ordered_canonical_first() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    let err = scan_profiles(dir.path()).unwrap_err();
    match err {
        ConfigError::AmbiguousProfile { paths, .. } => {
            assert_eq!(paths, vec![canonical, legacy]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
}

#[test]
fn scan_profiles_ignores_uppercase_yaml_extension() {
    let dir = tempfile::tempdir().unwrap();
    write_legacy_profile(dir.path(), "work.YAML", "work");
    assert!(scan_profiles(dir.path()).unwrap().is_empty());
}

#[test]
fn scan_profiles_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let entries = scan_profiles(&dir.path().join("nope")).unwrap();
    assert!(entries.is_empty());
}

#[test]
fn scan_profiles_tolerant_yields_both_forms_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy_profile(dir.path(), "zeta.yml", "zeta");
    let canonical = write_canonical_profile(dir.path(), "alpha", &[]);
    std::fs::create_dir_all(dir.path().join("zeta-payload").join("files")).unwrap();
    std::fs::write(dir.path().join("README.md"), "x").unwrap();

    let entries = scan_profiles_tolerant(dir.path()).unwrap();
    assert_eq!(entries.len(), 2);
    let ProfileScanEntry::Found(a) = &entries[0] else {
        panic!("alpha should be unambiguous: {:?}", entries[0]);
    };
    assert_eq!(a.name, "alpha");
    assert_eq!(a.form, ProfileForm::Canonical);
    assert_eq!(a.path, canonical);
    let ProfileScanEntry::Found(z) = &entries[1] else {
        panic!("zeta should be unambiguous: {:?}", entries[1]);
    };
    assert_eq!(z.name, "zeta");
    assert_eq!(z.form, ProfileForm::LegacyFlat);
    assert_eq!(z.path, legacy);
}

#[test]
fn scan_profiles_tolerant_carries_ambiguity_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    write_legacy_profile(dir.path(), "solo.yaml", "solo");

    let entries = scan_profiles_tolerant(dir.path()).unwrap();
    assert_eq!(entries.len(), 2);
    let ProfileScanEntry::Found(solo) = &entries[0] else {
        panic!("solo should be unambiguous: {:?}", entries[0]);
    };
    assert_eq!(solo.name, "solo");
    let ProfileScanEntry::Ambiguous { name, paths, error } = &entries[1] else {
        panic!("work should be ambiguous: {:?}", entries[1]);
    };
    assert_eq!(name, "work");
    assert_eq!(paths, &vec![canonical.clone(), legacy.clone()]);
    match error {
        ConfigError::AmbiguousProfile { name, paths } => {
            assert_eq!(name, "work");
            assert_eq!(paths, &vec![canonical, legacy]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
}

#[test]
fn scan_profiles_tolerant_three_forms_still_one_ambiguous_entry() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");

    let entries = scan_profiles_tolerant(dir.path()).unwrap();
    assert_eq!(entries.len(), 1);
    let ProfileScanEntry::Ambiguous { name, paths, error } = &entries[0] else {
        panic!("work should be ambiguous: {:?}", entries[0]);
    };
    assert_eq!(name, "work");
    assert_eq!(paths, &vec![canonical.clone(), yaml.clone(), yml.clone()]);
    let msg = error.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&yaml.posix().to_string()), "{msg}");
    assert!(msg.contains(&yml.posix().to_string()), "{msg}");
}

#[test]
fn scan_profile_manifests_found_has_single_path() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "alpha", &[]);
    let legacy = write_legacy_profile(dir.path(), "zeta.yml", "zeta");

    let entries = scan_profile_manifests(dir.path()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "alpha");
    assert_eq!(entries[0].paths, vec![canonical]);
    assert!(entries[0].ambiguity.is_none());
    assert_eq!(entries[1].name, "zeta");
    assert_eq!(entries[1].paths, vec![legacy]);
    assert!(entries[1].ambiguity.is_none());
}

#[test]
fn scan_profile_manifests_ambiguous_lists_every_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");

    let entries = scan_profile_manifests(dir.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "work");
    assert_eq!(entries[0].paths, vec![canonical, yaml, yml]);
    assert!(matches!(
        entries[0].ambiguity,
        Some(ConfigError::AmbiguousProfile { ref name, .. }) if name == "work"
    ));
}

#[test]
fn scan_profile_manifests_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        scan_profile_manifests(&dir.path().join("nope"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn scan_profiles_tolerant_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let entries = scan_profiles_tolerant(&dir.path().join("nope")).unwrap();
    assert!(entries.is_empty());
}

#[cfg(unix)]
#[test]
fn scan_profiles_tolerant_unreadable_dir_errors() {
    use std::os::unix::fs::PermissionsExt;
    if crate::is_root() {
        return; // root bypasses mode bits; the denial cannot be simulated
    }
    let dir = tempfile::tempdir().unwrap();
    let pdir = dir.path().join("profiles");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::set_permissions(&pdir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let err = scan_profiles_tolerant(&pdir).unwrap_err();
    assert!(
        err.to_string().contains("failed to read"),
        "unreadable dir must be an error, not an empty list: {err}"
    );

    std::fs::set_permissions(&pdir, std::fs::Permissions::from_mode(0o755)).unwrap();
}
