//! Snapshot tests for `cfgd config show`.
//!
//! Three cases: a populated config that exercises every section (`happy.txt`
//! human + `happy.json` JSON regression), and a minimal config that exercises
//! `section_if_nonempty` skipping every block (`empty.txt`).
//!
//! Goldens live under `tests/output_snapshots/config_show/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test config_show_v2_snapshots

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cfgd::cli::config_cmd::build_config_show_doc;
use cfgd_core::config::{
    CfgdConfig, ConfigMetadata, ConfigSpec, DaemonConfig, ModuleRegistryEntry,
    ModuleSecurityConfig, ModulesConfig, OriginSpec, OriginType, ReconcileConfig, SecretsConfig,
    SourceSpec, SshHostKeyPolicy, SyncConfig, ThemeConfig,
};
use cfgd_core::output_v2::{OutputFormat, Printer};
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

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

fn snapshot_path(name: &str) -> PathBuf {
    Path::new(SNAPSHOT_ROOT).join(name)
}

fn assert_snapshot(name: &str, actual: &str) {
    let path = snapshot_path(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    assert_eq!(actual, expected, "snapshot mismatch: {name}");
}

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
    printer.flush();
    drop(printer);
    let actual = strip_ansi(&cap.human());
    assert_snapshot("config_show/happy.txt", &actual);
}

#[test]
fn config_show_happy_json() {
    let cfg = happy_config();
    let path = Path::new("/etc/cfgd/cfgd.yaml");
    let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);
    printer.emit(build_config_show_doc(&cfg, path));
    drop(printer);
    let raw = buf.lock().unwrap().clone();
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("emitted JSON parses as serde_json::Value");
    let pretty = serde_json::to_string_pretty(&value).unwrap();
    assert_snapshot("config_show/happy.json", &pretty);

    // Cross-check: the emitted JSON equals serializing the config directly.
    let expected = serde_json::to_value(&cfg).unwrap();
    assert_eq!(
        value, expected,
        "emit -o json must match serde_json::to_value(cfg)"
    );
}

#[test]
fn config_show_empty_human() {
    let cfg = empty_config();
    let path = Path::new("/etc/cfgd/cfgd.yaml");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_config_show_doc(&cfg, path));
    printer.flush();
    drop(printer);
    let actual = strip_ansi(&cap.human());
    assert_snapshot("config_show/empty.txt", &actual);
}
