//! Snapshot tests for `cfgd profile show`.
//!
//! Three cases:
//!   - `profile_show/happy.{txt,json}` — multi-layer profile with env,
//!     packages (brew + cargo + simple lists), aliases, files, system keys, and
//!     secrets all populated. Exercises every section + the with_data(&ResolvedProfile)
//!     payload round-trip.
//!   - `profile_show/empty.txt` — single-layer profile with no env / packages /
//!     files / system / secrets. Exercises section_if_nonempty skipping every
//!     optional section while keeping the always-present Layers section.
//!
//! Goldens live under `tests/output_snapshots/profile_show/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_show_snapshots

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cfgd::cli::InventoryDetail;
use cfgd::cli::profile::show::build_profile_show_doc;
use cfgd_core::config::{
    AptSpec, BrewSpec, CargoSpec, EnvVar, FilesSpec, LayerPolicy, ManagedFileSpec, MergedProfile,
    PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile, SecretSpec, ShellAlias,
};
use cfgd_core::output::Printer;
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn layer(name: &str) -> ProfileLayer {
    ProfileLayer {
        source: "local".into(),
        profile_name: name.into(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec: ProfileSpec::default(),
    }
}

fn layer_with(name: &str, spec: ProfileSpec) -> ProfileLayer {
    ProfileLayer {
        spec,
        ..layer(name)
    }
}

/// What the `workstation` profile itself declares, as opposed to what the
/// `base` layer under it contributes. The default render is this spec; the
/// merged view below is what `--resolved` renders.
fn own_spec() -> ProfileSpec {
    let mut system = cfgd_core::config::SystemSettings::new();
    system.insert(
        "shellAliases".into(),
        serde_yaml::Value::String("default".into()),
    );

    ProfileSpec {
        inherits: vec!["base".into()],
        modules: vec!["dev-tools".into()],
        env: vec![
            EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
                platforms: vec![],
            },
            EnvVar {
                name: "GH_TOKEN".into(),
                value: "ghp_secret_token_value".into(),
                platforms: vec![],
            },
            EnvVar {
                name: "HOMEBREW_NO_ANALYTICS".into(),
                value: "1".into(),
                platforms: vec!["darwin".into()],
            },
        ],
        aliases: vec![ShellAlias {
            name: "gs".into(),
            command: "git status".into(),
            platforms: vec![],
        }],
        packages: Some(PackagesSpec {
            brew: Some(BrewSpec {
                file: None,
                taps: vec![],
                formulae: vec!["ripgrep".into()],
                casks: vec![],
            }),
            ..PackagesSpec::default()
        }),
        files: Some(FilesSpec {
            managed: vec![ManagedFileSpec {
                patch: None,
                source: "bashrc".into(),
                target: PathBuf::from("/home/user/.bashrc"),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        }),
        system,
        secrets: vec![SecretSpec {
            source: "op://Personal/GitHub/token".into(),
            target: Some(PathBuf::from("/home/user/.config/gh/token")),
            template: None,
            backend: None,
            envs: None,
        }],
        ..ProfileSpec::default()
    }
}

fn happy_resolved() -> ResolvedProfile {
    let mut system = std::collections::BTreeMap::new();
    system.insert(
        "shellAliases".into(),
        serde_yaml::Value::String("default".into()),
    );
    system.insert(
        "macosDefaults".into(),
        serde_yaml::Value::String("trackpad".into()),
    );

    ResolvedProfile {
        layers: vec![layer("base"), layer_with("workstation", own_spec())],
        merged: MergedProfile {
            modules: vec!["base".into(), "dev-tools".into()],
            env: vec![
                EnvVar {
                    name: "EDITOR".into(),
                    value: "nvim".into(),
                    platforms: vec![],
                },
                EnvVar {
                    name: "LANG".into(),
                    value: "en_US.UTF-8".into(),
                    platforms: vec![],
                },
            ],
            env_scope: cfgd_core::config::EnvScope::All,
            aliases: vec![
                ShellAlias {
                    name: "gs".into(),
                    command: "git status".into(),
                    platforms: vec![],
                },
                ShellAlias {
                    name: "ll".into(),
                    command: "ls -lah".into(),
                    platforms: vec![],
                },
            ],
            packages: PackagesSpec {
                brew: Some(BrewSpec {
                    file: None,
                    taps: vec!["homebrew/cask-fonts".into()],
                    formulae: vec!["ripgrep".into(), "fd".into()],
                    casks: vec!["firefox".into()],
                }),
                apt: Some(AptSpec {
                    file: None,
                    packages: vec!["curl".into(), "jq".into()],
                }),
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["cargo-edit".into()],
                }),
                pipx: vec!["black".into(), "ruff".into()],
                ..PackagesSpec::default()
            },
            files: FilesSpec {
                managed: vec![
                    ManagedFileSpec {
                        patch: None,
                        source: "bashrc".into(),
                        target: PathBuf::from("/home/user/.bashrc"),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    },
                    ManagedFileSpec {
                        patch: None,
                        source: "gitconfig".into(),
                        target: PathBuf::from("/home/user/.gitconfig"),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    },
                ],
                permissions: HashMap::new(),
            },
            system,
            secrets: vec![
                SecretSpec {
                    source: "op://Personal/GitHub/token".into(),
                    target: Some(PathBuf::from("/home/user/.config/gh/token")),
                    template: None,
                    backend: None,
                    envs: None,
                },
                SecretSpec {
                    source: "op://Personal/AWS/access-key".into(),
                    target: None,
                    template: None,
                    backend: None,
                    envs: Some(vec!["AWS_ACCESS_KEY_ID".into()]),
                },
            ],
            scripts: Default::default(),
            backups: Vec::new(),
            entry_owners: Default::default(),
            layer_sources: Default::default(),
        },
    }
}

fn empty_resolved() -> ResolvedProfile {
    ResolvedProfile {
        layers: vec![layer("default")],
        merged: MergedProfile::default(),
    }
}

#[test]
fn profile_show_happy_human() {
    let resolved = happy_resolved();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_show_doc(
        &resolved,
        "workstation",
        Path::new("/etc/cfgd/cfgd.yaml"),
        &[],
        printer.arrow(),
        InventoryDetail::default(),
        false,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_show/happy.txt");
}

#[test]
fn profile_show_happy_json() {
    let resolved = happy_resolved();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_show_doc(
        &resolved,
        "workstation",
        Path::new("/etc/cfgd/cfgd.yaml"),
        &[],
        printer.arrow(),
        InventoryDetail::default(),
        false,
    ));
    drop(printer);
    let expected = serde_json::json!({
        "name": "workstation",
        "resolved": serde_json::to_value(&resolved).unwrap(),
    });
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match {{name, resolved}} envelope"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_show/happy.json");
}

#[test]
fn profile_show_empty_human() {
    let resolved = empty_resolved();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_show_doc(
        &resolved,
        "default",
        Path::new("/etc/cfgd/cfgd.yaml"),
        &[],
        printer.arrow(),
        InventoryDetail::default(),
        false,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_show/empty.txt");
}

/// `--resolved` renders the merged view: every layer's contribution folded
/// together, headed by the `Layers` section naming what contributed. The
/// default render above is the profile's own declaration, and the two goldens
/// sit beside each other so the difference is readable.
#[test]
fn profile_show_resolved_renders_the_merged_view() {
    let resolved = happy_resolved();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_show_doc(
        &resolved,
        "workstation",
        Path::new("/etc/cfgd/cfgd.yaml"),
        &[],
        printer.arrow(),
        InventoryDetail::default(),
        true,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_show/resolved.txt");
}

/// A declared env VALUE is masked by default and rendered in the clear under
/// `--show-values`; the pair renders here so neither half can drift alone.
#[test]
fn profile_show_masks_declared_env_values_until_show_values() {
    let resolved = happy_resolved();

    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_show_doc(
        &resolved,
        "workstation",
        Path::new("/etc/cfgd/cfgd.yaml"),
        &[],
        printer.arrow(),
        InventoryDetail::default(),
        false,
    ));
    drop(printer);
    let masked = cap.human();
    assert!(
        !masked.contains("ghp_secret_token_value"),
        "a declared env value renders masked by default: {masked}"
    );

    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_show_doc(
        &resolved,
        "workstation",
        Path::new("/etc/cfgd/cfgd.yaml"),
        &[],
        printer.arrow(),
        InventoryDetail::of(cfgd::cli::EnvValueMasking::revealing(), false, false),
        false,
    ));
    drop(printer);
    let shown = cap.human();
    assert!(
        shown.contains("ghp_secret_token_value"),
        "--show-values renders the declared value in the clear: {shown}"
    );
}
