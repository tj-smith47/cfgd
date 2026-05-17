//! Snapshot tests for `cfgd config show`.
//!
//! Three cases: a populated config that exercises every section (`happy.txt`
//! human + `happy.json` JSON regression), and a minimal config that exercises
//! `section_if_nonempty` skipping every block (`empty.txt`).
//!
//! Goldens live under `tests/output_snapshots/config_show/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test config_show_v2_snapshots

use std::collections::HashMap;
use std::path::Path;

use cfgd::cli::config_cmd::build_config_show_doc;
use cfgd_core::config::{
    CfgdConfig, ConfigMetadata, ConfigSpec, DaemonConfig, ModuleRegistryEntry,
    ModuleSecurityConfig, ModulesConfig, OriginSpec, OriginType, ReconcileConfig, SecretsConfig,
    SourceSpec, SshHostKeyPolicy, SyncConfig, ThemeConfig,
};
use cfgd_core::output_v2::Printer;
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_config() -> CfgdConfig {
    CfgdConfig {
        api_version: "cfgd.io/v1alpha1".into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "happy-host".into(),
        },
        spec: ConfigSpec {
            profile: Some("base".into()),
            origin: vec![
                OriginSpec {
                    origin_type: OriginType::Git,
                    url: "git@example.com:owner/primary.git".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: SshHostKeyPolicy::AcceptNew,
                },
                OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://cfgd.example.com".into(),
                    branch: "stable".into(),
                    auth: None,
                    ssh_strict_host_key_checking: SshHostKeyPolicy::AcceptNew,
                },
            ],
            daemon: Some(DaemonConfig {
                enabled: true,
                reconcile: Some(ReconcileConfig {
                    interval: "5m".into(),
                    on_change: true,
                    auto_apply: false,
                    policy: None,
                    drift_policy: Default::default(),
                    patches: vec![],
                }),
                sync: Some(SyncConfig {
                    auto_push: false,
                    auto_pull: true,
                    interval: "1h".into(),
                }),
                notify: None,
                windows_event_log: false,
            }),
            secrets: Some(SecretsConfig {
                backend: "sops".into(),
                sops: None,
                integrations: vec![],
            }),
            sources: vec![
                SourceSpec {
                    name: "shared".into(),
                    origin: OriginSpec {
                        origin_type: OriginType::Git,
                        url: "git@example.com:owner/shared.git".into(),
                        branch: "master".into(),
                        auth: None,
                        ssh_strict_host_key_checking: SshHostKeyPolicy::AcceptNew,
                    },
                    subscription: Default::default(),
                    sync: Default::default(),
                },
                SourceSpec {
                    name: "team".into(),
                    origin: OriginSpec {
                        origin_type: OriginType::Git,
                        url: "git@example.com:team/configs.git".into(),
                        branch: "master".into(),
                        auth: None,
                        ssh_strict_host_key_checking: SshHostKeyPolicy::AcceptNew,
                    },
                    subscription: Default::default(),
                    sync: Default::default(),
                },
            ],
            theme: Some(ThemeConfig {
                name: "dracula".into(),
                overrides: Default::default(),
            }),
            modules: Some(ModulesConfig {
                registries: vec![
                    ModuleRegistryEntry {
                        name: "official".into(),
                        url: "git@example.com:cfgd/modules.git".into(),
                    },
                    ModuleRegistryEntry {
                        name: "internal".into(),
                        url: "git@example.com:org/internal-modules.git".into(),
                    },
                ],
                security: Some(ModuleSecurityConfig {
                    require_signatures: true,
                }),
            }),
            file_strategy: Default::default(),
            security: None,
            aliases: HashMap::new(),
            ai: None,
            compliance: None,
        },
    }
}

fn empty_config() -> CfgdConfig {
    CfgdConfig {
        api_version: "cfgd.io/v1alpha1".into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "minimal".into(),
        },
        spec: ConfigSpec {
            profile: Some("base".into()),
            ..ConfigSpec::default()
        },
    }
}

#[test]
fn config_show_happy_human() {
    let cfg = happy_config();
    let path = Path::new("/etc/cfgd/cfgd.yaml");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_config_show_doc(&cfg, path));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "config_show/happy.txt");
}

#[test]
fn config_show_happy_json() {
    let cfg = happy_config();
    let path = Path::new("/etc/cfgd/cfgd.yaml");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_config_show_doc(&cfg, path));
    drop(printer);
    // Cross-check: emitted JSON shape equals direct serialization of the config.
    let expected = serde_json::to_value(&cfg).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(cfg)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "config_show/happy.json");
}

#[test]
fn config_show_empty_human() {
    let cfg = empty_config();
    let path = Path::new("/etc/cfgd/cfgd.yaml");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_config_show_doc(&cfg, path));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "config_show/empty.txt");
}
