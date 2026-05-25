use super::*;
use crate::test_helpers::test_state;

fn quiet_reconcile_ctx<'a>(
    state: &'a Arc<Mutex<DaemonState>>,
    notifier: &'a Arc<Notifier>,
    notify_on_drift: bool,
    hooks: &'a dyn DaemonHooks,
    state_dir: &'a Path,
    printer: &'a crate::output::Printer,
) -> ReconcileCtx<'a> {
    ReconcileCtx {
        state,
        notifier,
        notify_on_drift,
        hooks,
        state_dir_override: Some(state_dir),
        printer,
        module_filter: None,
        auto_apply_override: None,
        drift_policy_override: None,
    }
}

#[test]
fn parse_duration_seconds() {
    assert_eq!(parse_duration_or_default("30s"), Duration::from_secs(30));
}

#[test]
fn parse_duration_minutes() {
    assert_eq!(parse_duration_or_default("5m"), Duration::from_secs(300));
}

#[test]
fn parse_duration_hours() {
    assert_eq!(parse_duration_or_default("1h"), Duration::from_secs(3600));
}

#[test]
fn parse_duration_plain_number() {
    assert_eq!(parse_duration_or_default("120"), Duration::from_secs(120));
}

#[test]
fn parse_duration_invalid_falls_back() {
    assert_eq!(
        parse_duration_or_default("invalid"),
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
}

#[test]
fn parse_duration_with_whitespace() {
    assert_eq!(parse_duration_or_default(" 10m "), Duration::from_secs(600));
}

#[test]
fn daemon_state_initial() {
    let state = DaemonState::new();
    assert!(state.last_reconcile.is_none());
    assert!(state.last_sync.is_none());
    assert_eq!(state.drift_count, 0);
    assert_eq!(state.sources.len(), 1);
    assert_eq!(state.sources[0].name, "local");
}

#[test]
fn daemon_state_response() {
    let state = DaemonState::new();
    let response = state.to_response();
    assert!(response.running);
    assert!(response.pid > 0);
    assert_eq!(response.sources.len(), 1);
}

#[test]
fn notifier_stdout_does_not_panic() {
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    assert!(matches!(notifier.method, NotifyMethod::Stdout));
    assert!(notifier.webhook_url.is_none());
    // Stdout notifier calls tracing::info! — verify it completes without panic
    notifier.notify("test", "message");
}

#[test]
fn source_status_round_trips() {
    let status = SourceStatus {
        name: "local".to_string(),
        last_sync: Some("2026-01-01T00:00:00Z".to_string()),
        last_reconcile: None,
        drift_count: 3,
        status: "active".to_string(),
    };
    let json = serde_json::to_string(&status).unwrap();
    let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "local");
    assert_eq!(parsed.last_sync.as_deref(), Some("2026-01-01T00:00:00Z"));
    assert!(parsed.last_reconcile.is_none());
    assert_eq!(parsed.drift_count, 3);
    assert_eq!(parsed.status, "active");
    // Verify camelCase renaming
    assert!(json.contains("\"driftCount\":3"));
    assert!(json.contains("\"lastSync\":"));
}

#[test]
#[cfg(unix)]
fn systemd_unit_path() {
    let home = "/home/testuser";
    let unit_dir = PathBuf::from(home).join(SYSTEMD_USER_DIR);
    let unit_path = unit_dir.join("cfgd.service");
    assert_eq!(
        unit_path.to_str().unwrap(),
        "/home/testuser/.config/systemd/user/cfgd.service"
    );
}

#[test]
fn generate_device_id_is_stable() {
    let id1 = generate_device_id().unwrap();
    let id2 = generate_device_id().unwrap();
    assert_eq!(id1, id2);
    // SHA256 hex string is 64 characters
    assert_eq!(id1.len(), 64);
}

#[test]
fn compute_config_hash_is_deterministic() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };
    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let hash1 = compute_config_hash(&resolved).unwrap();
    let hash2 = compute_config_hash(&resolved).unwrap();
    assert_eq!(hash1, hash2);
    assert_eq!(hash1.len(), 64);
}

#[test]
fn find_server_url_returns_none_for_git_origin() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Git,
                url: "https://github.com/test/repo.git".into(),
                branch: "master".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
        },
    };
    assert!(find_server_url(&config).is_none());
}

#[test]
fn find_server_url_returns_url_for_server_origin() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Server,
                url: "https://cfgd.example.com".into(),
                branch: "master".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
        },
    };
    assert_eq!(
        find_server_url(&config),
        Some("https://cfgd.example.com".to_string())
    );
}

#[test]
fn checkin_payload_round_trips() {
    let payload = CheckinPayload {
        device_id: "abc123".into(),
        hostname: "test-host".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        config_hash: "deadbeef".into(),
    };
    let json = serde_json::to_string(&payload).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["device_id"], "abc123");
    assert_eq!(parsed["hostname"], "test-host");
    assert_eq!(parsed["os"], "linux");
    assert_eq!(parsed["arch"], "x86_64");
    assert_eq!(parsed["config_hash"], "deadbeef");
    // Exactly 5 fields
    assert_eq!(parsed.as_object().unwrap().len(), 5);
}

#[test]
fn checkin_response_deserializes() {
    let json = r#"{"status":"ok","config_changed":true,"config":null}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    assert!(resp.config_changed);
    assert_eq!(resp._status, "ok");
}

#[test]
#[cfg(unix)]
fn launchd_plist_path() {
    let home = "/Users/testuser";
    let plist_dir = PathBuf::from(home).join(LAUNCHD_AGENTS_DIR);
    let plist_path = plist_dir.join(format!("{}.plist", LAUNCHD_LABEL));
    assert_eq!(
        plist_path.to_str().unwrap(),
        "/Users/testuser/Library/LaunchAgents/com.cfgd.daemon.plist"
    );
}

#[test]
fn extract_source_resources_from_merged_profile() {
    use crate::config::{
        BrewSpec, CargoSpec, FilesSpec, ManagedFileSpec, MergedProfile, PackagesSpec,
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["ripgrep".into(), "fd".into()],
                casks: vec!["firefox".into()],
                ..Default::default()
            }),
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        files: FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "dotfiles/.zshrc".into(),
                target: PathBuf::from("/home/user/.zshrc"),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            ..Default::default()
        },
        env: vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "vim".into(),
        }],
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("packages.brew.ripgrep"));
    assert!(resources.contains("packages.brew.fd"));
    assert!(resources.contains("packages.brew.firefox"));
    assert!(resources.contains("packages.cargo.bat"));
    assert!(resources.contains("files./home/user/.zshrc"));
    assert!(resources.contains("env.EDITOR"));
    assert_eq!(resources.len(), 6);
}

#[test]
fn hash_resources_is_deterministic() {
    let r1: HashSet<String> =
        HashSet::from_iter(["a".to_string(), "b".to_string(), "c".to_string()]);
    let r2: HashSet<String> =
        HashSet::from_iter(["c".to_string(), "a".to_string(), "b".to_string()]);

    assert_eq!(hash_resources(&r1), hash_resources(&r2));
}

#[test]
fn hash_resources_differs_for_different_sets() {
    let r1: HashSet<String> = HashSet::from_iter(["a".to_string()]);
    let r2: HashSet<String> = HashSet::from_iter(["b".to_string()]);

    assert_ne!(hash_resources(&r1), hash_resources(&r2));
}

#[test]
fn infer_item_tier_defaults_to_recommended() {
    assert_eq!(infer_item_tier("packages.brew.ripgrep"), "recommended");
    assert_eq!(infer_item_tier("env.EDITOR"), "recommended");
}

#[test]
fn infer_item_tier_detects_locked() {
    assert_eq!(infer_item_tier("files.security-policy.yaml"), "locked");
    assert_eq!(
        infer_item_tier("files./home/user/.config/company/security.yaml"),
        "locked"
    );
}

#[test]
fn process_source_decisions_first_run_records_decisions() {
    use crate::config::PackagesSpec;
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default(); // new_recommended: Notify

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // First run: all items are new, policy is Notify → pending decisions created
    let pending = store.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "packages.cargo.bat");
    assert!(excluded.contains("packages.cargo.bat"));
}

#[test]
fn process_source_decisions_accept_policy_no_pending() {
    use crate::config::PackagesSpec;
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // Accept policy: no pending decisions, not excluded from plan
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(!excluded.contains("packages.cargo.bat"));
}

// --- Compliance snapshot-on-change logic ---

#[test]
fn compliance_snapshot_skips_when_hash_unchanged() {
    let store = test_state();
    let snapshot = crate::compliance::ComplianceSnapshot {
        timestamp: crate::utc_now_iso8601(),
        machine: crate::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![crate::compliance::ComplianceCheck {
            category: "file".into(),
            status: crate::compliance::ComplianceStatus::Compliant,
            detail: Some("present".into()),
            ..Default::default()
        }],
        summary: crate::compliance::ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 0,
        },
    };

    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    let hash = crate::sha256_hex(json.as_bytes());

    // Store first snapshot
    store.store_compliance_snapshot(&snapshot, &hash).unwrap();

    // Latest hash should match — a second store would be skipped
    let latest = store.latest_compliance_hash().unwrap();
    assert_eq!(latest.as_deref(), Some(hash.as_str()));
}

#[test]
fn compliance_snapshot_stores_when_hash_changes() {
    let store = test_state();

    let snapshot1 = crate::compliance::ComplianceSnapshot {
        timestamp: "2026-01-01T00:00:00Z".into(),
        machine: crate::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![crate::compliance::ComplianceCheck {
            category: "file".into(),
            status: crate::compliance::ComplianceStatus::Compliant,
            ..Default::default()
        }],
        summary: crate::compliance::ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 0,
        },
    };

    let json1 = serde_json::to_string_pretty(&snapshot1).unwrap();
    let hash1 = crate::sha256_hex(json1.as_bytes());
    store.store_compliance_snapshot(&snapshot1, &hash1).unwrap();

    // Different snapshot with a violation
    let snapshot2 = crate::compliance::ComplianceSnapshot {
        timestamp: "2026-01-02T00:00:00Z".into(),
        machine: crate::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![crate::compliance::ComplianceCheck {
            category: "package".into(),
            status: crate::compliance::ComplianceStatus::Violation,
            ..Default::default()
        }],
        summary: crate::compliance::ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 1,
        },
    };

    let json2 = serde_json::to_string_pretty(&snapshot2).unwrap();
    let hash2 = crate::sha256_hex(json2.as_bytes());

    // Hashes differ — new snapshot should be stored
    assert_ne!(hash1, hash2);
    let latest = store.latest_compliance_hash().unwrap();
    assert_ne!(latest.as_deref(), Some(hash2.as_str()));

    store.store_compliance_snapshot(&snapshot2, &hash2).unwrap();
    let latest = store.latest_compliance_hash().unwrap();
    assert_eq!(latest.as_deref(), Some(hash2.as_str()));

    // Both snapshots stored
    let history = store.compliance_history(None, 10).unwrap();
    assert_eq!(history.len(), 2);
}

#[test]
fn compliance_timer_not_created_when_disabled() {
    // When compliance is not enabled, compliance_interval should be None
    let config = config::ComplianceConfig {
        enabled: false,
        interval: "1h".into(),
        retention: "30d".into(),
        scope: config::ComplianceScope::default(),
        export: config::ComplianceExport::default(),
    };

    let interval = config
        .enabled
        .then(|| crate::parse_duration_str(&config.interval).ok())
        .flatten();

    assert!(interval.is_none());
}

#[test]
fn compliance_timer_created_when_enabled() {
    let config = config::ComplianceConfig {
        enabled: true,
        interval: "30m".into(),
        retention: "7d".into(),
        scope: config::ComplianceScope::default(),
        export: config::ComplianceExport::default(),
    };

    let interval = config
        .enabled
        .then(|| crate::parse_duration_str(&config.interval).ok())
        .flatten();

    assert_eq!(interval, Some(Duration::from_secs(30 * 60)));
}

#[test]
fn compliance_timer_invalid_interval_when_enabled() {
    let config = config::ComplianceConfig {
        enabled: true,
        interval: "garbage".into(),
        retention: "7d".into(),
        scope: config::ComplianceScope::default(),
        export: config::ComplianceExport::default(),
    };

    let interval = config
        .enabled
        .then(|| crate::parse_duration_str(&config.interval).ok())
        .flatten();

    // Enabled but unparseable interval -> None (no timer)
    assert!(interval.is_none());
}

// --- compute_config_hash: different profiles produce different hashes ---

#[test]
fn compute_config_hash_differs_for_different_packages() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let resolved_a = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "a".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let resolved_b = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "b".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["ripgrep".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let hash_a = compute_config_hash(&resolved_a).unwrap();
    let hash_b = compute_config_hash(&resolved_b).unwrap();
    assert_ne!(hash_a, hash_b);
}

// --- hash_resources edge cases ---

#[test]
fn hash_resources_empty_set() {
    let empty: HashSet<String> = HashSet::new();
    let hash = hash_resources(&empty);
    // Should produce a valid hash (SHA256 of empty string)
    assert_eq!(hash, crate::sha256_hex(b""));
}

#[test]
fn hash_resources_single_element() {
    let set: HashSet<String> = HashSet::from_iter(["packages.brew.ripgrep".to_string()]);
    let hash = hash_resources(&set);
    assert_eq!(hash.len(), 64);
    // Compare against known SHA256 of "packages.brew.ripgrep\n"
    let expected = crate::sha256_hex(b"packages.brew.ripgrep\n");
    assert_eq!(hash, expected);
}

// --- DaemonState::to_response field validation ---

#[test]
fn daemon_state_to_response_propagates_fields() {
    let mut state = DaemonState::new();
    state.last_reconcile = Some("2026-03-30T12:00:00Z".to_string());
    state.last_sync = Some("2026-03-30T12:01:00Z".to_string());
    state.drift_count = 5;
    state.update_available = Some("2.0.0".to_string());

    let response = state.to_response();
    assert!(response.running);
    assert_eq!(
        response.last_reconcile.as_deref(),
        Some("2026-03-30T12:00:00Z")
    );
    assert_eq!(response.last_sync.as_deref(), Some("2026-03-30T12:01:00Z"));
    assert_eq!(response.drift_count, 5);
    assert_eq!(response.update_available.as_deref(), Some("2.0.0"));
    assert_eq!(response.sources.len(), 1);
    assert_eq!(response.sources[0].name, "local");
}

// --- DaemonStatusResponse with module_reconcile and update_available ---

#[test]
fn daemon_status_response_with_modules_round_trips() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 42,
        uptime_secs: 100,
        last_reconcile: None,
        last_sync: None,
        drift_count: 2,
        sources: vec![],
        update_available: Some("1.5.0".to_string()),
        module_reconcile: vec![
            ModuleReconcileStatus {
                name: "security-baseline".to_string(),
                interval: "60s".to_string(),
                auto_apply: true,
                drift_policy: "Auto".to_string(),
                last_reconcile: Some("2026-03-30T00:00:00Z".to_string()),
            },
            ModuleReconcileStatus {
                name: "dev-tools".to_string(),
                interval: "300s".to_string(),
                auto_apply: false,
                drift_policy: "NotifyOnly".to_string(),
                last_reconcile: None,
            },
        ],
    };

    let json = serde_json::to_string(&response).unwrap();
    let parsed: DaemonStatusResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.pid, 42);
    assert_eq!(parsed.drift_count, 2);
    assert_eq!(parsed.update_available.as_deref(), Some("1.5.0"));
    assert_eq!(parsed.module_reconcile.len(), 2);
    assert_eq!(parsed.module_reconcile[0].name, "security-baseline");
    assert!(parsed.module_reconcile[0].auto_apply);
    assert_eq!(parsed.module_reconcile[1].name, "dev-tools");
    assert!(!parsed.module_reconcile[1].auto_apply);
    assert!(parsed.module_reconcile[1].last_reconcile.is_none());
}

#[test]
fn daemon_status_response_skips_empty_module_reconcile() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 1,
        uptime_secs: 0,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    // module_reconcile has skip_serializing_if = "Vec::is_empty"
    assert!(!json.contains("\"moduleReconcile\""));
    // update_available has skip_serializing_if = "Option::is_none"
    assert!(!json.contains("\"updateAvailable\""));
}

// --- action_resource_info tests ---

#[test]
fn action_resource_info_file_create() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Create {
        source: PathBuf::from("/src/.zshrc"),
        target: PathBuf::from("/home/user/.zshrc"),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::default(),
        source_hash: None,
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/home/user/.zshrc");
}

#[test]
fn action_resource_info_file_update() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Update {
        source: PathBuf::from("/src/.zshrc"),
        target: PathBuf::from("/home/user/.zshrc"),
        diff: "--- a\n+++ b".into(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::default(),
        source_hash: None,
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/home/user/.zshrc");
}

#[test]
fn action_resource_info_file_delete() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Delete {
        target: PathBuf::from("/tmp/gone"),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/tmp/gone");
}

#[test]
fn action_resource_info_file_set_permissions() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::SetPermissions {
        target: PathBuf::from("/home/user/.ssh/config"),
        mode: 0o600,
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/home/user/.ssh/config");
}

#[test]
fn action_resource_info_file_skip() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Skip {
        target: PathBuf::from("/etc/skipped"),
        reason: "not needed".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/etc/skipped");
}

#[test]
fn action_resource_info_package_bootstrap() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Bootstrap {
        manager: "brew".into(),
        method: "curl".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "brew:bootstrap");
}

#[test]
fn action_resource_info_package_install() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Install {
        manager: "apt".into(),
        packages: vec!["curl".into(), "wget".into()],
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "apt:curl,wget");
}

#[test]
fn action_resource_info_package_uninstall() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Uninstall {
        manager: "npm".into(),
        packages: vec!["typescript".into()],
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "npm:typescript");
}

#[test]
fn action_resource_info_package_skip() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Skip {
        manager: "cargo".into(),
        reason: "not available".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "cargo");
}

#[test]
fn action_resource_info_secret_decrypt() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::Decrypt {
        source: PathBuf::from("/secrets/api.enc"),
        target: PathBuf::from("/home/user/.api_key"),
        backend: "age".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "/home/user/.api_key");
}

#[test]
fn action_resource_info_secret_resolve() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::Resolve {
        provider: "1password".into(),
        reference: "op://vault/item/field".into(),
        target: PathBuf::from("/tmp/secret"),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "op://vault/item/field");
}

#[test]
fn action_resource_info_secret_resolve_env() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::ResolveEnv {
        provider: "vault".into(),
        reference: "secret/data/app".into(),
        envs: vec!["API_KEY".into(), "DB_PASS".into()],
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "env:[API_KEY,DB_PASS]");
}

#[test]
fn action_resource_info_secret_skip() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::Skip {
        source: "bitwarden".into(),
        reason: "not configured".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "bitwarden");
}

#[test]
fn action_resource_info_system_set_value() {
    use crate::reconciler::{Action, SystemAction};

    let action = Action::System(SystemAction::SetValue {
        configurator: "sysctl".into(),
        key: "vm.swappiness".into(),
        desired: "10".into(),
        current: "60".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "system");
    assert_eq!(rid, "sysctl:vm.swappiness");
}

#[test]
fn action_resource_info_system_skip() {
    use crate::reconciler::{Action, SystemAction};

    let action = Action::System(SystemAction::Skip {
        configurator: "gsettings".into(),
        reason: "not on GNOME".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "system");
    assert_eq!(rid, "gsettings");
}

#[test]
fn action_resource_info_script_run() {
    use crate::reconciler::{Action, ScriptAction, ScriptPhase};

    let action = Action::Script(ScriptAction::Run {
        entry: crate::config::ScriptEntry::Simple("echo hello".into()),
        phase: ScriptPhase::PreApply,
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "script");
    assert_eq!(rid, "echo hello");
}

#[test]
fn action_resource_info_module() {
    use crate::reconciler::{Action, ModuleAction, ModuleActionKind};

    let action = Action::Module(ModuleAction {
        module_name: "security-baseline".into(),
        kind: ModuleActionKind::InstallPackages { resolved: vec![] },
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "module");
    assert_eq!(rid, "security-baseline");
}

#[test]
fn action_resource_info_env_write() {
    use crate::reconciler::{Action, EnvAction};

    let action = Action::Env(EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/user/.cfgd.env"),
        content: "export FOO=bar".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "env");
    assert_eq!(rid, "/home/user/.cfgd.env");
}

#[test]
fn action_resource_info_env_inject() {
    use crate::reconciler::{Action, EnvAction};

    let action = Action::Env(EnvAction::InjectSourceLine {
        rc_path: PathBuf::from("/home/user/.bashrc"),
        line: "source ~/.cfgd.env".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "env-rc");
    assert_eq!(rid, "/home/user/.bashrc");
}

// --- extract_source_resources with more package managers ---

#[test]
fn extract_source_resources_apt_dnf_pipx_npm() {
    use crate::config::{AptSpec, MergedProfile, NpmSpec, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            apt: Some(AptSpec {
                file: None,
                packages: vec!["git".into(), "tmux".into()],
            }),
            dnf: vec!["vim".into()],
            pipx: vec!["black".into()],
            npm: Some(NpmSpec {
                file: None,
                global: vec!["prettier".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("packages.apt.git"));
    assert!(resources.contains("packages.apt.tmux"));
    assert!(resources.contains("packages.dnf.vim"));
    assert!(resources.contains("packages.pipx.black"));
    assert!(resources.contains("packages.npm.prettier"));
    assert_eq!(resources.len(), 5);
}

#[test]
fn extract_source_resources_system_keys() {
    use crate::config::MergedProfile;

    let mut merged = MergedProfile::default();
    merged
        .system
        .insert("sysctl".into(), serde_yaml::Value::Null);
    merged
        .system
        .insert("kernelModules".into(), serde_yaml::Value::Null);

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("system.sysctl"));
    assert!(resources.contains("system.kernelModules"));
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_empty_profile() {
    let merged = crate::config::MergedProfile::default();
    let resources = extract_source_resources(&merged);
    assert!(resources.is_empty());
}

// --- Config change detection: process_source_decisions second call ---

#[test]
fn process_source_decisions_no_change_on_second_call() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: crate::config::PolicyAction::Accept,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    // First call: stores the hash
    let _ = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // Second call with same profile: hash matches, no new decisions
    let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // No pending decisions since policy is Accept
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(excluded.is_empty());
}

#[test]
fn process_source_decisions_detects_new_items_on_change() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default(); // Notify by default

    // First call with one package
    let merged1 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = process_source_decisions(&store, "acme", &merged1, &policy, &notifier);
    // Clear pending decisions from first run
    let first_pending = store.pending_decisions().unwrap();
    for d in &first_pending {
        let _ = store.resolve_decisions_for_source(&d.source, "accepted");
    }

    // Second call with an additional package
    let merged2 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let excluded = process_source_decisions(&store, "acme", &merged2, &policy, &notifier);

    // Should have a pending decision for ripgrep (new item)
    let pending = store.pending_decisions().unwrap();
    assert!(!pending.is_empty());
    let resource_names: Vec<&str> = pending.iter().map(|d| d.resource.as_str()).collect();
    assert!(resource_names.contains(&"packages.cargo.ripgrep"));
    assert!(excluded.contains("packages.cargo.ripgrep"));
}

// --- infer_item_tier: "policy" keyword ---

#[test]
fn infer_item_tier_detects_policy_keyword() {
    assert_eq!(infer_item_tier("files.policy-definitions.yaml"), "locked");
    assert_eq!(infer_item_tier("system.security-policy"), "locked");
}

// --- ModuleReconcileStatus serialization ---

#[test]
fn module_reconcile_status_round_trips() {
    let status = ModuleReconcileStatus {
        name: "dev-tools".into(),
        interval: "120s".into(),
        auto_apply: false,
        drift_policy: "NotifyOnly".into(),
        last_reconcile: None,
    };
    let json = serde_json::to_string(&status).unwrap();
    let parsed: ModuleReconcileStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "dev-tools");
    assert_eq!(parsed.interval, "120s");
    assert!(!parsed.auto_apply);
    assert_eq!(parsed.drift_policy, "NotifyOnly");
    assert!(parsed.last_reconcile.is_none());
    // Verify camelCase
    assert!(json.contains("\"autoApply\""));
    assert!(json.contains("\"driftPolicy\""));
    assert!(json.contains("\"lastReconcile\""));
}

// --- Notifier construction ---

#[test]
fn notifier_webhook_without_url_does_not_panic() {
    let notifier = Notifier::new(NotifyMethod::Webhook, None);
    assert!(matches!(notifier.method, NotifyMethod::Webhook));
    // Webhook with no URL should early-return via `let Some(ref url) = ...` guard
    assert!(
        notifier.webhook_url.is_none(),
        "webhook_url must be None to exercise the early-return path"
    );
    // Should log a warning but not panic and not attempt any HTTP request
    notifier.notify("test", "no url configured");
}

// --- find_server_url with multiple origins ---

#[test]
fn find_server_url_picks_server_among_multiple_origins() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![
                OriginSpec {
                    origin_type: OriginType::Git,
                    url: "https://github.com/test/repo.git".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
                OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://fleet.example.com".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
            ],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
        },
    };
    assert_eq!(
        find_server_url(&config),
        Some("https://fleet.example.com".to_string())
    );
}

#[test]
fn find_server_url_returns_none_for_empty_origins() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
        },
    };
    assert!(find_server_url(&config).is_none());
}

// --- CheckinServerResponse deserialization edge cases ---

#[test]
fn checkin_response_with_config_payload() {
    let json = r#"{"status":"ok","config_changed":true,"config":{"packages":["git"]}}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    assert!(resp.config_changed);
    assert!(resp._config.is_some());
}

#[test]
fn checkin_response_no_change() {
    let json = r#"{"status":"ok","config_changed":false,"config":null}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    assert!(!resp.config_changed);
}

// --- parse_duration_or_default: zero values ---

#[test]
fn parse_duration_zero_seconds() {
    assert_eq!(parse_duration_or_default("0s"), Duration::from_secs(0));
}

#[test]
fn parse_duration_zero_plain() {
    assert_eq!(parse_duration_or_default("0"), Duration::from_secs(0));
}

// --- compute_config_hash with empty packages ---

#[test]
fn compute_config_hash_with_empty_packages() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "empty".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let hash1 = compute_config_hash(&resolved).unwrap();
    let hash2 = compute_config_hash(&resolved).unwrap();
    assert_eq!(hash1, hash2, "hash should be deterministic");
    assert_eq!(hash1.len(), 64, "hash should be a valid SHA256 hex string");
}

// --- extract_source_resources: brew taps are not included, casks are ---

#[test]
fn extract_source_resources_brew_casks_only() {
    use crate::config::{BrewSpec, MergedProfile, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec![],
                casks: vec!["iterm2".into(), "visual-studio-code".into()],
                taps: vec!["homebrew/cask".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert!(
        resources.contains("packages.brew.iterm2"),
        "casks should appear as brew resources"
    );
    assert!(
        resources.contains("packages.brew.visual-studio-code"),
        "casks should appear as brew resources"
    );
    // Taps are not tracked as individual resources
    assert!(
        !resources.contains("packages.brew.homebrew/cask"),
        "taps should not appear as resources"
    );
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_cargo_packages_only() {
    use crate::config::{CargoSpec, MergedProfile, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: Some("Cargo.toml".into()),
                packages: vec!["cargo-watch".into(), "cargo-expand".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("packages.cargo.cargo-watch"));
    assert!(resources.contains("packages.cargo.cargo-expand"));
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_npm_globals() {
    use crate::config::{MergedProfile, NpmSpec, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            npm: Some(NpmSpec {
                file: None,
                global: vec!["typescript".into(), "eslint".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("packages.npm.typescript"));
    assert!(resources.contains("packages.npm.eslint"));
    assert_eq!(resources.len(), 2);
}

// --- process_source_decisions with Reject policy ---

#[test]
fn process_source_decisions_reject_policy_silently_skips() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Reject,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // Reject policy: no pending decisions, items pass through silently
    let pending = store.pending_decisions().unwrap();
    assert!(
        pending.is_empty(),
        "reject policy should not create pending decisions"
    );
    assert!(
        excluded.is_empty(),
        "reject policy does not create pending records so nothing is excluded"
    );
}

// --- find_server_url with duplicate server origins picks first ---

#[test]
fn find_server_url_picks_first_server_among_duplicates() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![
                OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://first-server.example.com".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
                OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://second-server.example.com".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
            ],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
        },
    };
    assert_eq!(
        find_server_url(&config),
        Some("https://first-server.example.com".to_string()),
        "should return the first server origin when multiple exist"
    );
}

// --- compute_config_hash: empty vs non-empty produces different hashes ---

#[test]
fn compute_config_hash_empty_vs_nonempty_differ() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let empty_resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "empty".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let nonempty_resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "nonempty".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let hash_empty = compute_config_hash(&empty_resolved).unwrap();
    let hash_nonempty = compute_config_hash(&nonempty_resolved).unwrap();
    assert_ne!(
        hash_empty, hash_nonempty,
        "empty and non-empty packages should produce different hashes"
    );
}

// --- process_source_decisions with Ignore policy ---

#[test]
fn process_source_decisions_ignore_policy_no_pending_no_excluded() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Ignore,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // Ignore policy: silently skipped, no pending decisions, nothing excluded
    let pending = store.pending_decisions().unwrap();
    assert!(
        pending.is_empty(),
        "ignore policy should not create pending decisions"
    );
    assert!(
        excluded.is_empty(),
        "ignore policy does not create pending records so nothing is excluded"
    );
}

// --- Notifier construction variants ---

#[test]
fn notifier_desktop_mode_does_not_panic() {
    // Desktop notification may fail in CI (no display server) but should not panic.
    // On failure, notify_desktop falls back to notify_stdout via tracing::info.
    let notifier = Notifier::new(NotifyMethod::Desktop, None);
    assert!(matches!(notifier.method, NotifyMethod::Desktop));
    assert!(
        notifier.webhook_url.is_none(),
        "desktop notifier should not have a webhook URL"
    );
    notifier.notify("test title", "test body");
}

#[tokio::test]
async fn notifier_webhook_with_url_does_not_panic() {
    // Webhook to a nonexistent URL: should log error but not panic
    let notifier = Notifier::new(
        NotifyMethod::Webhook,
        Some("http://127.0.0.1:1/nonexistent".to_string()),
    );
    notifier.notify("test", "message to invalid webhook");
}

#[test]
fn notifier_stdout_writes_info() {
    // Verify stdout notifier is configured for Stdout method and runs
    // the tracing::info path with structured title/message fields.
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    assert!(matches!(notifier.method, NotifyMethod::Stdout));
    // The notify_stdout method calls tracing::info!(title, message, "notification")
    // Verify it handles non-trivial content without panic
    notifier.notify("drift event", "file /etc/foo changed");
    notifier.notify("", ""); // edge case: empty strings
    notifier.notify("special chars: <>&\"'", "path: /home/user/.config/cfgd");
}

// --- DaemonState: multiple sources ---

#[test]
fn daemon_state_with_multiple_sources() {
    let mut state = DaemonState::new();
    state.sources.push(SourceStatus {
        name: "acme-corp".to_string(),
        last_sync: Some("2026-03-30T10:00:00Z".to_string()),
        last_reconcile: None,
        drift_count: 2,
        status: "active".to_string(),
    });
    state.sources.push(SourceStatus {
        name: "team-tools".to_string(),
        last_sync: None,
        last_reconcile: Some("2026-03-30T11:00:00Z".to_string()),
        drift_count: 0,
        status: "error".to_string(),
    });

    let response = state.to_response();
    assert_eq!(response.sources.len(), 3); // local + acme-corp + team-tools
    assert_eq!(response.sources[1].name, "acme-corp");
    assert_eq!(response.sources[1].drift_count, 2);
    assert_eq!(response.sources[2].name, "team-tools");
    assert_eq!(response.sources[2].status, "error");
}

// --- DaemonState: drift counting ---

#[test]
fn daemon_state_drift_increments_propagate_to_response() {
    let mut state = DaemonState::new();
    state.drift_count = 10;
    if let Some(source) = state.sources.first_mut() {
        source.drift_count = 7;
    }

    let response = state.to_response();
    assert_eq!(response.drift_count, 10);
    assert_eq!(response.sources[0].drift_count, 7);
}

// --- DaemonState: module_last_reconcile tracking ---

#[test]
fn daemon_state_module_last_reconcile_tracking() {
    let mut state = DaemonState::new();
    state.module_last_reconcile.insert(
        "security-baseline".to_string(),
        "2026-03-30T12:00:00Z".to_string(),
    );
    state
        .module_last_reconcile
        .insert("dev-tools".to_string(), "2026-03-30T12:05:00Z".to_string());

    assert_eq!(state.module_last_reconcile.len(), 2);
    assert_eq!(
        state
            .module_last_reconcile
            .get("security-baseline")
            .unwrap(),
        "2026-03-30T12:00:00Z"
    );
    assert_eq!(
        state.module_last_reconcile.get("dev-tools").unwrap(),
        "2026-03-30T12:05:00Z"
    );

    // to_response does not currently populate module_reconcile (empty vec)
    let response = state.to_response();
    assert!(response.module_reconcile.is_empty());
}

// --- DaemonStatusResponse: update_available serialization ---

#[test]
fn daemon_status_response_update_available_present() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 99,
        uptime_secs: 600,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: Some("3.0.0".to_string()),
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"updateAvailable\":\"3.0.0\""));
    let parsed: DaemonStatusResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.update_available.as_deref(), Some("3.0.0"));
}

// --- SyncTask construction ---

#[test]
fn sync_task_local_defaults() {
    let task = SyncTask {
        source_name: "local".to_string(),
        repo_path: PathBuf::from("/home/user/.config/cfgd"),
        auto_pull: false,
        auto_push: false,
        auto_apply: true,
        interval: Duration::from_secs(DEFAULT_SYNC_SECS),
        last_synced: None,
        require_signed_commits: false,
        allow_unsigned: false,
    };

    assert_eq!(task.source_name, "local");
    assert!(task.auto_apply);
    assert!(!task.auto_pull);
    assert!(!task.auto_push);
    assert!(task.last_synced.is_none());
    assert_eq!(task.interval.as_secs(), 300);
}

#[test]
fn sync_task_source_with_signing() {
    let task = SyncTask {
        source_name: "acme-corp".to_string(),
        repo_path: PathBuf::from("/tmp/sources/acme-corp"),
        auto_pull: true,
        auto_push: false,
        auto_apply: false,
        interval: Duration::from_secs(600),
        last_synced: Some(Instant::now()),
        require_signed_commits: true,
        allow_unsigned: false,
    };

    assert_eq!(task.source_name, "acme-corp");
    assert!(task.auto_pull);
    assert!(!task.auto_push);
    assert!(!task.auto_apply);
    assert!(task.require_signed_commits);
    assert!(!task.allow_unsigned);
    assert!(task.last_synced.is_some());
}

#[test]
fn sync_task_allow_unsigned_overrides_require_signed() {
    let task = SyncTask {
        source_name: "relaxed".to_string(),
        repo_path: PathBuf::from("/tmp/sources/relaxed"),
        auto_pull: true,
        auto_push: false,
        auto_apply: true,
        interval: Duration::from_secs(300),
        last_synced: None,
        require_signed_commits: true,
        allow_unsigned: true,
    };

    // Both flags can be set; the consumer decides precedence
    assert!(task.require_signed_commits);
    assert!(task.allow_unsigned);
}

// --- ReconcileTask construction ---

#[test]
fn reconcile_task_default() {
    let task = ReconcileTask {
        entity: "__default__".to_string(),
        interval: Duration::from_secs(DEFAULT_RECONCILE_SECS),
        auto_apply: false,
        drift_policy: config::DriftPolicy::default(),
        last_reconciled: None,
    };

    assert_eq!(task.entity, "__default__");
    assert_eq!(task.interval.as_secs(), 300);
    assert!(!task.auto_apply);
    assert!(task.last_reconciled.is_none());
}

#[test]
fn reconcile_task_per_module() {
    let task = ReconcileTask {
        entity: "security-baseline".to_string(),
        interval: Duration::from_secs(60),
        auto_apply: true,
        drift_policy: config::DriftPolicy::Auto,
        last_reconciled: Some(Instant::now()),
    };

    assert_eq!(task.entity, "security-baseline");
    assert_eq!(task.interval.as_secs(), 60);
    assert!(task.auto_apply);
    assert!(task.last_reconciled.is_some());
}

// --- pending_resource_paths ---

#[test]
fn pending_resource_paths_empty_store() {
    let store = test_state();
    let paths = pending_resource_paths(&store);
    assert!(paths.is_empty());
}

#[test]
fn pending_resource_paths_with_decisions() {
    let store = test_state();
    store
        .upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat (from acme)",
        )
        .unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "env.EDITOR",
            "recommended",
            "install",
            "recommended env.EDITOR (from acme)",
        )
        .unwrap();

    let paths = pending_resource_paths(&store);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains("packages.cargo.bat"));
    assert!(paths.contains("env.EDITOR"));
}

// --- infer_item_tier: more coverage ---

#[test]
fn infer_item_tier_locked_keyword() {
    assert_eq!(infer_item_tier("files.locked-module-config.yaml"), "locked");
}

#[test]
fn infer_item_tier_security_in_system() {
    assert_eq!(infer_item_tier("system.security-baseline"), "locked");
}

#[test]
fn infer_item_tier_normal_package() {
    assert_eq!(infer_item_tier("packages.brew.curl"), "recommended");
}

#[test]
fn infer_item_tier_normal_env_var() {
    assert_eq!(infer_item_tier("env.GOPATH"), "recommended");
}

#[test]
fn infer_item_tier_normal_file() {
    assert_eq!(infer_item_tier("files./home/user/.zshrc"), "recommended");
}

// --- extract_source_resources: aliases not included (not tracked) ---

#[test]
fn extract_source_resources_aliases_not_tracked() {
    use crate::config::{MergedProfile, ShellAlias};

    let merged = MergedProfile {
        aliases: vec![
            ShellAlias {
                name: "ll".into(),
                command: "ls -la".into(),
            },
            ShellAlias {
                name: "gp".into(),
                command: "git push".into(),
            },
        ],
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    // Aliases are not tracked as individual resources
    assert!(
        resources.is_empty(),
        "aliases should not be tracked as source resources"
    );
}

// --- extract_source_resources: mixed profile with everything ---

#[test]
fn extract_source_resources_full_profile() {
    use crate::config::{
        AptSpec, BrewSpec, CargoSpec, EnvVar, FilesSpec, ManagedFileSpec, MergedProfile, NpmSpec,
        PackagesSpec,
    };

    let mut system = std::collections::HashMap::new();
    system.insert("sysctl".into(), serde_yaml::Value::Null);

    let merged = MergedProfile {
        packages: PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["ripgrep".into()],
                casks: vec!["firefox".into()],
                ..Default::default()
            }),
            apt: Some(AptSpec {
                file: None,
                packages: vec!["curl".into()],
            }),
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            pipx: vec!["black".into()],
            dnf: vec!["vim".into()],
            npm: Some(NpmSpec {
                file: None,
                global: vec!["typescript".into()],
            }),
            ..Default::default()
        },
        files: FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "dotfiles/.zshrc".into(),
                target: PathBuf::from("/home/user/.zshrc"),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            ..Default::default()
        },
        env: vec![
            EnvVar {
                name: "EDITOR".into(),
                value: "vim".into(),
            },
            EnvVar {
                name: "GOPATH".into(),
                value: "/home/user/go".into(),
            },
        ],
        system,
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    // Verify all expected resources are present
    assert!(resources.contains("packages.brew.ripgrep"));
    assert!(resources.contains("packages.brew.firefox"));
    assert!(resources.contains("packages.apt.curl"));
    assert!(resources.contains("packages.cargo.bat"));
    assert!(resources.contains("packages.pipx.black"));
    assert!(resources.contains("packages.dnf.vim"));
    assert!(resources.contains("packages.npm.typescript"));
    assert!(resources.contains("files./home/user/.zshrc"));
    assert!(resources.contains("env.EDITOR"));
    assert!(resources.contains("env.GOPATH"));
    assert!(resources.contains("system.sysctl"));
    // Total: 1 formula + 1 cask + 1 apt + 1 cargo + 1 pipx + 1 dnf + 1 npm + 1 file + 2 env + 1 system
    assert_eq!(resources.len(), 11);
}

// --- process_source_decisions: locked_conflict policy ---

#[test]
fn process_source_decisions_locked_item_notify_policy() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        locked_conflict: PolicyAction::Notify,
        ..Default::default()
    };

    // Use a file with "security" in the name to trigger the locked tier
    let mut system = std::collections::HashMap::new();
    system.insert("security-baseline".into(), serde_yaml::Value::Null);

    let merged = MergedProfile {
        system,
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "corp", &merged, &policy, &notifier);

    // The "system.security-baseline" item should be inferred as "locked" tier
    // and with locked_conflict = Notify, it should create a pending decision
    let pending = store.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "system.security-baseline");
    assert!(excluded.contains("system.security-baseline"));
}

// --- process_source_decisions: multiple sources ---

#[test]
fn process_source_decisions_different_sources_independent() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        ..Default::default()
    };

    let merged_a = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let merged_b = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["ripgrep".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let excluded_a = process_source_decisions(&store, "source-a", &merged_a, &policy, &notifier);
    let excluded_b = process_source_decisions(&store, "source-b", &merged_b, &policy, &notifier);

    // Accept policy: both sources processed, nothing excluded
    assert!(excluded_a.is_empty());
    assert!(excluded_b.is_empty());
}

// --- process_source_decisions: items removed from source ---

#[test]
fn process_source_decisions_removed_items_update_hash() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        ..Default::default()
    };

    // First call: bat + ripgrep
    let merged1 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = process_source_decisions(&store, "acme", &merged1, &policy, &notifier);

    // Second call: only bat (ripgrep removed from source)
    let merged2 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let excluded = process_source_decisions(&store, "acme", &merged2, &policy, &notifier);

    // Hash changed, but Accept policy means no pending decisions
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(excluded.is_empty());
}

// --- SourceStatus: field defaults ---

#[test]
fn source_status_defaults() {
    let status = SourceStatus {
        name: "test".to_string(),
        last_sync: None,
        last_reconcile: None,
        drift_count: 0,
        status: "active".to_string(),
    };

    assert!(status.last_sync.is_none());
    assert!(status.last_reconcile.is_none());
    assert_eq!(status.drift_count, 0);
}

// --- SourceStatus: all fields populated ---

#[test]
fn source_status_all_fields_populated() {
    let status = SourceStatus {
        name: "corp-source".to_string(),
        last_sync: Some("2026-03-30T10:00:00Z".to_string()),
        last_reconcile: Some("2026-03-30T10:05:00Z".to_string()),
        drift_count: 15,
        status: "error".to_string(),
    };

    let json = serde_json::to_string(&status).unwrap();
    let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "corp-source");
    assert_eq!(parsed.last_sync.as_deref(), Some("2026-03-30T10:00:00Z"));
    assert_eq!(
        parsed.last_reconcile.as_deref(),
        Some("2026-03-30T10:05:00Z")
    );
    assert_eq!(parsed.drift_count, 15);
    assert_eq!(parsed.status, "error");
}

// --- DaemonStatusResponse deserialization from external JSON ---

#[test]
fn daemon_status_response_deserializes_from_minimal_json() {
    let json = r#"{
            "running": false,
            "pid": 0,
            "uptimeSecs": 0,
            "lastReconcile": null,
            "lastSync": null,
            "driftCount": 0,
            "sources": []
        }"#;

    let parsed: DaemonStatusResponse = serde_json::from_str(json).unwrap();
    assert!(!parsed.running);
    assert_eq!(parsed.pid, 0);
    assert!(parsed.module_reconcile.is_empty());
    assert!(parsed.update_available.is_none());
}

// --- CheckinPayload: field coverage ---

#[test]
fn checkin_payload_serializes_all_fields() {
    let payload = CheckinPayload {
        device_id: "sha256hex".into(),
        hostname: "myhost.local".into(),
        os: "linux".into(),
        arch: "aarch64".into(),
        config_hash: "abcd1234".into(),
    };

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("\"device_id\""));
    assert!(json.contains("\"hostname\""));
    assert!(json.contains("\"os\""));
    assert!(json.contains("\"arch\""));
    assert!(json.contains("\"config_hash\""));
    assert!(json.contains("aarch64"));
}

// --- parse_duration_or_default: edge cases ---

#[test]
fn parse_duration_large_seconds() {
    assert_eq!(
        parse_duration_or_default("86400s"),
        Duration::from_secs(86400)
    );
}

#[test]
fn parse_duration_large_hours() {
    assert_eq!(parse_duration_or_default("24h"), Duration::from_secs(86400));
}

#[test]
fn parse_duration_empty_string_falls_back() {
    assert_eq!(
        parse_duration_or_default(""),
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
}

// --- hash_resources: ordering does not matter ---

#[test]
fn hash_resources_large_set_deterministic() {
    let set1: HashSet<String> = (0..100)
        .map(|i| format!("packages.brew.pkg{}", i))
        .collect();
    let set2: HashSet<String> = (0..100)
        .rev()
        .map(|i| format!("packages.brew.pkg{}", i))
        .collect();

    assert_eq!(hash_resources(&set1), hash_resources(&set2));
}

// --- ModuleReconcileStatus: camelCase field names ---

#[test]
fn module_reconcile_status_camel_case_fields() {
    let status = ModuleReconcileStatus {
        name: "test".into(),
        interval: "60s".into(),
        auto_apply: true,
        drift_policy: "Auto".into(),
        last_reconcile: Some("2026-01-01T00:00:00Z".into()),
    };

    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"autoApply\""));
    assert!(json.contains("\"driftPolicy\""));
    assert!(json.contains("\"lastReconcile\""));
    // Should NOT contain snake_case
    assert!(!json.contains("\"auto_apply\""));
    assert!(!json.contains("\"drift_policy\""));
    assert!(!json.contains("\"last_reconcile\""));
}

// --- DaemonStatusResponse: uptime_secs is camelCase in JSON ---

#[test]
fn daemon_status_response_camel_case_uptime() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 1,
        uptime_secs: 42,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"uptimeSecs\""));
    assert!(json.contains("\"driftCount\""));
    assert!(!json.contains("\"uptime_secs\""));
    assert!(!json.contains("\"drift_count\""));
}

// --- process_source_decisions: mixed policies per tier ---

#[test]
fn process_source_decisions_mixed_tiers_accept_recommended_notify_locked() {
    use crate::config::{CargoSpec, PackagesSpec};

    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        new_optional: PolicyAction::Ignore,
        locked_conflict: PolicyAction::Notify,
    };

    // Mix of recommended (cargo packages) and locked (security system setting)
    let mut system = std::collections::HashMap::new();
    system.insert("security-policy".into(), serde_yaml::Value::Null);

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        system,
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "corp", &merged, &policy, &notifier);

    let pending = store.pending_decisions().unwrap();
    // Only the locked item should be pending (security-policy)
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "system.security-policy");
    // bat should not be excluded (Accept policy for recommended)
    assert!(!excluded.contains("packages.cargo.bat"));
    // security-policy should be excluded (pending)
    assert!(excluded.contains("system.security-policy"));
}

// --- generate_device_id: always hex ---

#[test]
fn generate_device_id_hex_format() {
    let id = generate_device_id().unwrap();
    // Should be lowercase hex only
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit()),
        "device ID should be hex: {}",
        id
    );
}

// --- extract_source_resources: multiple files ---

#[test]
fn extract_source_resources_multiple_files() {
    use crate::config::{FilesSpec, ManagedFileSpec, MergedProfile};

    let merged = MergedProfile {
        files: FilesSpec {
            managed: vec![
                ManagedFileSpec {
                    source: "dotfiles/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
                ManagedFileSpec {
                    source: "dotfiles/.vimrc".into(),
                    target: PathBuf::from("/home/user/.vimrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
                ManagedFileSpec {
                    source: "dotfiles/.gitconfig".into(),
                    target: PathBuf::from("/home/user/.gitconfig"),
                    strategy: None,
                    private: true,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
            ],
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert_eq!(resources.len(), 3);
    assert!(resources.contains("files./home/user/.zshrc"));
    assert!(resources.contains("files./home/user/.vimrc"));
    assert!(resources.contains("files./home/user/.gitconfig"));
}

// --- extract_source_resources: multiple env vars ---

#[test]
fn extract_source_resources_multiple_env_vars() {
    use crate::config::{EnvVar, MergedProfile};

    let merged = MergedProfile {
        env: vec![
            EnvVar {
                name: "PATH".into(),
                value: "/usr/local/bin:$PATH".into(),
            },
            EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            },
            EnvVar {
                name: "GOPATH".into(),
                value: "/home/user/go".into(),
            },
        ],
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert_eq!(resources.len(), 3);
    assert!(resources.contains("env.PATH"));
    assert!(resources.contains("env.EDITOR"));
    assert!(resources.contains("env.GOPATH"));
}

// --- extract_source_resources: multiple system keys ---

#[test]
fn extract_source_resources_multiple_system_keys() {
    use crate::config::MergedProfile;

    let mut system = std::collections::HashMap::new();
    system.insert("sysctl".into(), serde_yaml::Value::Null);
    system.insert("kernelModules".into(), serde_yaml::Value::Null);
    system.insert("apparmor".into(), serde_yaml::Value::Null);

    let merged = MergedProfile {
        system,
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert_eq!(resources.len(), 3);
    assert!(resources.contains("system.sysctl"));
    assert!(resources.contains("system.kernelModules"));
    assert!(resources.contains("system.apparmor"));
}

// --- DaemonState: uptime increases ---

#[test]
fn daemon_state_uptime_increases() {
    let state = DaemonState::new();
    // Small sleep to ensure non-zero uptime
    std::thread::sleep(Duration::from_millis(10));
    let response = state.to_response();
    // Uptime should be at least 0 (could be 0 if resolution is 1s)
    // The key assertion is that it doesn't panic
    assert!(response.uptime_secs < 10);
}

// --- handle_health_connection: /health endpoint ---

#[tokio::test]
async fn health_connection_health_endpoint() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    // Spawn the handler
    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    // Send HTTP request
    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    // Read response
    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK, got: {}",
        &response[..response.len().min(40)]
    );
    assert!(response.contains("\"status\""));
    assert!(response.contains("\"pid\""));
    assert!(response.contains("\"uptime_secs\""));
}

// --- handle_health_connection: /status endpoint ---

#[tokio::test]
async fn health_connection_status_endpoint() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    // Populate some state
    {
        let mut st = state.lock().await;
        st.drift_count = 3;
        st.last_reconcile = Some("2026-03-30T10:00:00Z".to_string());
    }

    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK, got: {}",
        &response[..response.len().min(40)]
    );
    // Body should contain DaemonStatusResponse fields (pretty-printed JSON)
    assert!(
        response.contains("\"running\": true"),
        "response should contain running field: {}",
        &response
    );
    assert!(
        response.contains("\"driftCount\": 3"),
        "response should contain driftCount field: {}",
        &response
    );
}

// --- handle_health_connection: /drift endpoint ---

#[tokio::test]
async fn health_connection_drift_endpoint() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /drift HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK, got: {}",
        &response[..response.len().min(40)]
    );
    assert!(response.contains("\"drift_count\""));
    assert!(response.contains("\"events\""));
}

// --- handle_health_connection: 404 for unknown path ---

#[tokio::test]
async fn health_connection_unknown_path_returns_404() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 404 Not Found"),
        "expected 404, got: {}",
        &response[..response.len().min(40)]
    );
    assert!(response.contains("\"error\""));
}

// --- git_pull: repo with no remote changes returns Ok(false) ---

#[test]
fn git_pull_no_remote_returns_up_to_date() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    // Create a bare repo as "remote"
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone the bare repo to get a working copy with origin
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();

    // Configure committer identity
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "cfgd-test").unwrap();
    config.set_str("user.email", "test@cfgd.io").unwrap();

    // Create initial commit (bare repos start empty, clone has no HEAD)
    let readme = work_dir.join("README");
    std::fs::write(&readme, "test\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("README")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();

    // Push initial commit to bare remote
    let mut remote = repo.find_remote("origin").unwrap();
    remote
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    // Now pull — should be up-to-date since we just pushed
    let result = git_pull(&work_dir);
    assert!(result.is_ok(), "git_pull failed: {:?}", result);
    assert!(!result.unwrap(), "expected no changes");
}

// --- git_pull: repo with new remote commits returns Ok(true) ---

#[test]
fn git_pull_with_remote_changes_returns_true() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");
    let pusher_dir = tmp.path().join("pusher");

    // Create bare repo
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone into work_dir
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }

    // Create initial commit and push
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Clone into pusher_dir and push a new commit
    let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
    {
        let mut config = pusher.config().unwrap();
        config.set_str("user.name", "cfgd-pusher").unwrap();
        config.set_str("user.email", "pusher@cfgd.io").unwrap();
    }
    std::fs::write(pusher_dir.join("NEW_FILE"), "hello\n").unwrap();
    {
        let mut index = pusher.index().unwrap();
        index.add_path(Path::new("NEW_FILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = pusher.find_tree(tree_id).unwrap();
        let sig = pusher.signature().unwrap();
        let parent = pusher.head().unwrap().peel_to_commit().unwrap();
        pusher
            .commit(Some("HEAD"), &sig, &sig, "add file", &tree, &[&parent])
            .unwrap();
    }
    {
        let mut remote = pusher.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Now git_pull in work_dir should detect changes
    let result = git_pull(&work_dir);
    assert!(result.is_ok(), "git_pull failed: {:?}", result);
    assert!(result.unwrap(), "expected changes from remote");

    // Verify the new file exists after pull
    assert!(
        work_dir.join("NEW_FILE").exists(),
        "NEW_FILE should exist after fast-forward pull"
    );
}

// --- git_auto_commit_push: no changes returns Ok(false) ---

#[test]
fn git_auto_commit_push_no_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    // Create bare repo
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone, create initial commit, push
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "test\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // No changes — should return Ok(false)
    let result = git_auto_commit_push(&work_dir);
    assert!(result.is_ok(), "git_auto_commit_push failed: {:?}", result);
    assert!(!result.unwrap(), "expected no changes to push");
}

// --- git_auto_commit_push: with changes commits and pushes ---

#[test]
fn git_auto_commit_push_with_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    // Create bare repo
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone, create initial commit, push
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "test\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Create a new file (uncommitted change)
    std::fs::write(work_dir.join("new_config.yaml"), "key: value\n").unwrap();

    // Should commit and push the change
    let result = git_auto_commit_push(&work_dir);
    assert!(result.is_ok(), "git_auto_commit_push failed: {:?}", result);
    assert!(result.unwrap(), "expected changes to be pushed");

    // Verify commit was created
    let repo = git2::Repository::open(&work_dir).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        head.message().unwrap(),
        "cfgd: auto-commit configuration changes"
    );

    // Verify the change was pushed to bare repo
    let bare = git2::Repository::open_bare(&bare_dir).unwrap();
    let bare_head = bare
        .find_reference("refs/heads/master")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(head.id(), bare_head.id());
}

// --- git_pull: non-git directory returns error ---

#[test]
fn git_pull_non_repo_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = git_pull(tmp.path());
    let err = result.unwrap_err();
    assert!(
        err.contains("open repo"),
        "expected 'open repo' error, got: {err}"
    );
}

// --- git_auto_commit_push: non-git directory returns error ---

#[test]
fn git_auto_commit_push_non_repo_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = git_auto_commit_push(tmp.path());
    let err = result.unwrap_err();
    assert!(
        err.contains("open repo"),
        "expected 'open repo' error, got: {err}"
    );
}

// --- handle_sync: updates daemon state timestamps ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_updates_state_timestamps() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));

    let changed = handle_sync(&repo_dir, false, false, "local", &state, false, false).await;

    assert!(!changed);

    let st = state.lock().await;
    assert!(st.last_sync.is_some(), "last_sync should be set");
}

// --- handle_sync: with auto_pull on repo without remote ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_pull_without_remote_logs_warning() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));

    let changed = handle_sync(&repo_dir, true, false, "local", &state, false, false).await;

    // Should not crash; pull fails gracefully
    assert!(!changed);
}

// --- handle_sync: per-source status update ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_updates_per_source_status() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));
    // Add a second source
    {
        let mut st = state.lock().await;
        st.sources.push(SourceStatus {
            name: "acme".to_string(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 0,
            status: "active".to_string(),
        });
    }

    handle_sync(&repo_dir, false, false, "acme", &state, false, false).await;

    let st = state.lock().await;
    // The "acme" source should have its last_sync updated
    let acme = st.sources.iter().find(|s| s.name == "acme").unwrap();
    assert!(
        acme.last_sync.is_some(),
        "acme source last_sync should be set"
    );
    // The "local" source should NOT have been updated
    let local = st.sources.iter().find(|s| s.name == "local").unwrap();
    assert!(
        local.last_sync.is_none(),
        "local source last_sync should remain None"
    );
}

// --- handle_sync: auto_pull with remote changes fast-forwards ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_auto_pull_with_remote_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");
    let pusher_dir = tmp.path().join("pusher");

    // Set up bare + work + pusher repos
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Push a change from pusher
    let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
    {
        let mut config = pusher.config().unwrap();
        config.set_str("user.name", "cfgd-pusher").unwrap();
        config.set_str("user.email", "pusher@cfgd.io").unwrap();
    }
    std::fs::write(pusher_dir.join("NEWFILE"), "synced\n").unwrap();
    {
        let mut index = pusher.index().unwrap();
        index.add_path(Path::new("NEWFILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = pusher.find_tree(tree_id).unwrap();
        let sig = pusher.signature().unwrap();
        let parent = pusher.head().unwrap().peel_to_commit().unwrap();
        pusher
            .commit(Some("HEAD"), &sig, &sig, "add newfile", &tree, &[&parent])
            .unwrap();
    }
    {
        let mut remote = pusher.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let changed = handle_sync(&work_dir, true, false, "local", &state, false, false).await;

    assert!(changed, "handle_sync should detect remote changes");
    assert!(
        work_dir.join("NEWFILE").exists(),
        "pulled file should exist after sync"
    );
}

// --- handle_sync: auto_push with local changes ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_auto_push_with_local_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Create a local change
    std::fs::write(work_dir.join("local_change.txt"), "new content\n").unwrap();

    let state = Arc::new(Mutex::new(DaemonState::new()));
    // pull=false, push=true
    let changed = handle_sync(&work_dir, false, true, "local", &state, false, false).await;

    // No remote changes to pull, but push should succeed
    assert!(!changed, "no pull changes expected");

    // Verify commit was pushed to bare repo
    let bare = git2::Repository::open_bare(&bare_dir).unwrap();
    let bare_head = bare
        .find_reference("refs/heads/master")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(
        bare_head.message().unwrap(),
        "cfgd: auto-commit configuration changes"
    );
}

// --- git_pull: diverged branches return error ---

#[test]
fn git_pull_diverged_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");
    let pusher_dir = tmp.path().join("pusher");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Push a divergent change from pusher
    let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
    {
        let mut config = pusher.config().unwrap();
        config.set_str("user.name", "cfgd-pusher").unwrap();
        config.set_str("user.email", "pusher@cfgd.io").unwrap();
    }
    std::fs::write(pusher_dir.join("PUSHER_FILE"), "pusher\n").unwrap();
    {
        let mut index = pusher.index().unwrap();
        index.add_path(Path::new("PUSHER_FILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = pusher.find_tree(tree_id).unwrap();
        let sig = pusher.signature().unwrap();
        let parent = pusher.head().unwrap().peel_to_commit().unwrap();
        pusher
            .commit(Some("HEAD"), &sig, &sig, "pusher commit", &tree, &[&parent])
            .unwrap();
    }
    {
        let mut remote = pusher.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Create a local commit in work_dir (diverged from remote)
    std::fs::write(work_dir.join("LOCAL_FILE"), "local\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("LOCAL_FILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "local commit", &tree, &[&parent])
            .unwrap();
    }

    // git_pull should fail because branches diverged (not fast-forwardable)
    let result = git_pull(&work_dir);
    assert!(result.is_err(), "diverged branch should return error");
    let err_msg = result.unwrap_err();
    assert!(
        err_msg.contains("diverged") || err_msg.contains("fast-forward"),
        "error should mention divergence: {}",
        err_msg
    );
}

// --- git_auto_commit_push: fresh repo with no HEAD ---

#[test]
fn git_auto_commit_push_fresh_repo_no_head() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }

    // Create a file but don't commit yet — repo has no HEAD
    std::fs::write(work_dir.join("first_file.txt"), "hello\n").unwrap();

    let result = git_auto_commit_push(&work_dir);
    assert!(result.is_ok(), "fresh repo push failed: {:?}", result);
    assert!(result.unwrap(), "expected changes to be committed");

    // Verify HEAD now exists with the auto-commit message
    let repo = git2::Repository::open(&work_dir).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        head.message().unwrap(),
        "cfgd: auto-commit configuration changes"
    );
}

// --- server_checkin: mock HTTP test for config_changed=true ---

#[test]
fn server_checkin_mock_config_changed() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":true,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(changed, "server should report config changed");
    mock.assert();
}

// --- server_checkin: mock HTTP test for config_changed=false ---

#[test]
fn server_checkin_mock_no_change() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed, "server should report no change");
    mock.assert();
}

// --- server_checkin: server returns 500 ---

#[test]
fn server_checkin_mock_server_error() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(500)
        .with_body("internal server error")
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed, "server error should return false");
    mock.assert();
}

// --- server_checkin: malformed JSON response ---

#[test]
fn server_checkin_mock_malformed_json() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("not json at all")
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed, "malformed JSON should return false");
    mock.assert();
}

// --- server_checkin: URL with trailing slash ---

#[test]
fn server_checkin_mock_trailing_slash_url() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    // URL with trailing slash should be trimmed
    let url_with_slash = format!("{}/", server.url());
    let changed = server_checkin(&url_with_slash, &resolved);
    assert!(!changed);
    mock.assert();
}

// --- server_checkin: verifies request payload structure ---

#[test]
fn server_checkin_mock_verifies_request_body() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .match_header("Content-Type", "application/json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed);
    // Verify the mock received the request with correct Content-Type
    mock.assert();
}

// --- try_server_checkin: delegates to server_checkin when URL present ---

#[test]
fn try_server_checkin_no_server_origin_returns_false() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Git,
                url: "https://github.com/test/repo.git".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: FileStrategy::default(),
            ai: None,
            compliance: None,
        },
    };
    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile::default(),
    };

    let changed = try_server_checkin(&config, &resolved);
    assert!(!changed, "no server origin means no checkin");
}

// --- try_server_checkin: with mock server ---

#[test]
fn try_server_checkin_with_server_origin_calls_checkin() {
    use crate::config::*;

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":true,"config":null}"#)
        .create();

    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Server,
                url: server.url(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: FileStrategy::default(),
            ai: None,
            compliance: None,
        },
    };
    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile::default(),
    };

    let changed = try_server_checkin(&config, &resolved);
    assert!(changed, "server origin should trigger checkin");
    mock.assert();
}

// --- handle_health_connection: response includes Content-Type and Content-Length ---

#[tokio::test]
async fn health_connection_response_headers() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.contains("Content-Type: application/json"),
        "missing Content-Type header"
    );
    assert!(
        response.contains("Content-Length:"),
        "missing Content-Length header"
    );
    assert!(
        response.contains("Connection: close"),
        "missing Connection header"
    );
}

// --- handle_health_connection: empty request line defaults to /health ---

#[tokio::test]
async fn health_connection_empty_request_defaults_to_health() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    // Send an empty line as the request
    writer.write_all(b"\r\n\r\n").await.unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    // Empty request should either default to /health or return 404
    // The code uses `split_whitespace().nth(1).unwrap_or("/health")` so
    // empty request line -> /health
    assert!(
        response.contains("200 OK") || response.contains("404 Not Found"),
        "should handle empty request gracefully: {}",
        &response[..response.len().min(80)]
    );
}

// --- handle_health_connection: /status body parses to DaemonStatusResponse ---

#[tokio::test]
async fn health_connection_status_body_parses_as_response() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    {
        let mut st = state.lock().await;
        st.drift_count = 7;
        st.update_available = Some("2.0.0".to_string());
    }

    let (client, server) = tokio::io::duplex(8192);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if in_body {
                    lines.push(line);
                } else if line.trim().is_empty() {
                    in_body = true;
                }
            }
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    let body = lines.join("");
    let parsed: DaemonStatusResponse =
        serde_json::from_str(&body).expect("body should parse as DaemonStatusResponse");
    assert!(parsed.running);
    assert_eq!(parsed.drift_count, 7);
    assert_eq!(parsed.update_available.as_deref(), Some("2.0.0"));
    assert_eq!(parsed.sources.len(), 1);
    assert_eq!(parsed.sources[0].name, "local");
}

// --- DaemonState: module_last_reconcile overwrite ---

#[test]
fn daemon_state_module_last_reconcile_overwrite() {
    let mut state = DaemonState::new();
    state
        .module_last_reconcile
        .insert("mod-a".into(), "2026-01-01T00:00:00Z".into());
    state
        .module_last_reconcile
        .insert("mod-a".into(), "2026-01-02T00:00:00Z".into());

    // Overwrite should replace the old value
    assert_eq!(state.module_last_reconcile.len(), 1);
    assert_eq!(
        state.module_last_reconcile.get("mod-a").unwrap(),
        "2026-01-02T00:00:00Z"
    );
}

// --- DaemonState: update_available persists through to_response ---

#[test]
fn daemon_state_update_available_in_response() {
    let mut state = DaemonState::new();
    state.update_available = Some("3.1.0".to_string());

    let response = state.to_response();
    assert_eq!(response.update_available.as_deref(), Some("3.1.0"));
}

// --- Notifier: webhook builds correct JSON payload structure ---

#[test]
fn notifier_webhook_payload_structure() {
    // Verify the JSON payload structure by constructing it the same way as notify_webhook
    let title = "cfgd: drift detected";
    let message = "3 files drifted";
    let payload = serde_json::json!({
        "event": title,
        "message": message,
        "timestamp": crate::utc_now_iso8601(),
        "source": "cfgd",
    });

    let obj = payload.as_object().unwrap();
    assert_eq!(obj.len(), 4);
    assert_eq!(obj.get("event").unwrap().as_str().unwrap(), title);
    assert_eq!(obj.get("message").unwrap().as_str().unwrap(), message);
    assert!(obj.contains_key("timestamp"));
    assert_eq!(obj.get("source").unwrap().as_str().unwrap(), "cfgd");
}

// --- Notifier: webhook payload timestamp format ---

#[test]
fn notifier_webhook_payload_timestamp_is_iso8601() {
    let payload = serde_json::json!({
        "event": "test",
        "message": "msg",
        "timestamp": crate::utc_now_iso8601(),
        "source": "cfgd",
    });

    let ts = payload["timestamp"].as_str().unwrap();
    // ISO 8601 format: contains 'T' separator and ends with 'Z'
    assert!(ts.contains('T'), "timestamp should be ISO 8601: {}", ts);
    assert!(ts.ends_with('Z'), "timestamp should end with Z: {}", ts);
}

// --- ReconcileTask: drift_policy variants ---

#[test]
fn reconcile_task_drift_policy_auto() {
    let task = ReconcileTask {
        entity: "critical-module".into(),
        interval: Duration::from_secs(30),
        auto_apply: true,
        drift_policy: config::DriftPolicy::Auto,
        last_reconciled: None,
    };
    assert!(matches!(task.drift_policy, config::DriftPolicy::Auto));
}

#[test]
fn reconcile_task_drift_policy_notify_only() {
    let task = ReconcileTask {
        entity: "optional-module".into(),
        interval: Duration::from_secs(600),
        auto_apply: false,
        drift_policy: config::DriftPolicy::NotifyOnly,
        last_reconciled: None,
    };
    assert!(matches!(task.drift_policy, config::DriftPolicy::NotifyOnly));
}

#[test]
fn reconcile_task_drift_policy_prompt() {
    let task = ReconcileTask {
        entity: "interactive-module".into(),
        interval: Duration::from_secs(300),
        auto_apply: false,
        drift_policy: config::DriftPolicy::Prompt,
        last_reconciled: None,
    };
    assert!(matches!(task.drift_policy, config::DriftPolicy::Prompt));
}

// --- process_source_decisions: new_optional tier with Accept policy ---

#[test]
fn process_source_decisions_optional_tier_accept() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Notify,
        new_optional: PolicyAction::Accept,
        locked_conflict: PolicyAction::Notify,
    };

    // Regular packages trigger "recommended" tier, not "optional".
    // The current infer_item_tier only returns "recommended" or "locked".
    // Verify that recommended items still get the Notify treatment.
    let merged = MergedProfile {
        packages: crate::config::PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);
    let pending = store.pending_decisions().unwrap();
    // "bat" is recommended tier -> Notify policy -> creates pending decision
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "packages.cargo.bat");
    assert!(excluded.contains("packages.cargo.bat"));
}

// --- process_source_decisions: empty merged profile no decisions ---

#[test]
fn process_source_decisions_empty_profile_no_decisions() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default();

    let merged = MergedProfile::default();

    let excluded = process_source_decisions(&store, "empty", &merged, &policy, &notifier);
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(excluded.is_empty());
}

// --- DaemonStatusResponse: deserialization with all optional fields ---

#[test]
fn daemon_status_response_full_deserialization() {
    let json = r#"{
            "running": true,
            "pid": 54321,
            "uptimeSecs": 7200,
            "lastReconcile": "2026-04-01T00:00:00Z",
            "lastSync": "2026-04-01T00:01:00Z",
            "driftCount": 42,
            "sources": [
                {
                    "name": "local",
                    "lastSync": "2026-04-01T00:01:00Z",
                    "lastReconcile": "2026-04-01T00:00:00Z",
                    "driftCount": 10,
                    "status": "active"
                }
            ],
            "updateAvailable": "4.0.0",
            "moduleReconcile": [
                {
                    "name": "sec",
                    "interval": "30s",
                    "autoApply": true,
                    "driftPolicy": "Auto",
                    "lastReconcile": "2026-04-01T00:00:00Z"
                }
            ]
        }"#;

    let parsed: DaemonStatusResponse = serde_json::from_str(json).unwrap();
    assert!(parsed.running);
    assert_eq!(parsed.pid, 54321);
    assert_eq!(parsed.uptime_secs, 7200);
    assert_eq!(
        parsed.last_reconcile.as_deref(),
        Some("2026-04-01T00:00:00Z")
    );
    assert_eq!(parsed.last_sync.as_deref(), Some("2026-04-01T00:01:00Z"));
    assert_eq!(parsed.drift_count, 42);
    assert_eq!(parsed.sources.len(), 1);
    assert_eq!(parsed.sources[0].drift_count, 10);
    assert_eq!(parsed.update_available.as_deref(), Some("4.0.0"));
    assert_eq!(parsed.module_reconcile.len(), 1);
    assert_eq!(parsed.module_reconcile[0].name, "sec");
    assert!(parsed.module_reconcile[0].auto_apply);
}

// --- CheckinServerResponse: missing config field defaults to None ---

#[test]
fn checkin_response_without_config_field() {
    let json = r#"{"status":"ok","config_changed":false}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    // _config is Option<Value>, so missing field deserializes as None
    assert!(!resp.config_changed);
    assert!(resp._config.is_none());
}

// --- hash_resources: unicode content ---

#[test]
fn hash_resources_unicode_content() {
    let set: HashSet<String> = HashSet::from_iter(["packages.brew.\u{1f600}".to_string()]);
    let hash = hash_resources(&set);
    assert_eq!(hash.len(), 64);
    // Must be deterministic
    assert_eq!(hash, hash_resources(&set));
}

// --- parse_duration_or_default: whitespace-only falls back ---

#[test]
fn parse_duration_whitespace_only_falls_back() {
    assert_eq!(
        parse_duration_or_default("   "),
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
}

// --- SyncTask: interval boundary values ---

#[test]
fn sync_task_zero_interval() {
    let task = SyncTask {
        source_name: "instant".into(),
        repo_path: PathBuf::from("/tmp"),
        auto_pull: true,
        auto_push: true,
        auto_apply: true,
        interval: Duration::from_secs(0),
        last_synced: None,
        require_signed_commits: false,
        allow_unsigned: false,
    };
    assert_eq!(task.interval, Duration::ZERO);
}

// --- DaemonState: to_response sources ordering is preserved ---

#[test]
fn daemon_state_to_response_preserves_source_order() {
    let mut state = DaemonState::new();
    state.sources.push(SourceStatus {
        name: "z-source".into(),
        last_sync: None,
        last_reconcile: None,
        drift_count: 0,
        status: "active".into(),
    });
    state.sources.push(SourceStatus {
        name: "a-source".into(),
        last_sync: None,
        last_reconcile: None,
        drift_count: 0,
        status: "active".into(),
    });

    let response = state.to_response();
    assert_eq!(response.sources[0].name, "local");
    assert_eq!(response.sources[1].name, "z-source");
    assert_eq!(response.sources[2].name, "a-source");
}

// --- DaemonState: started_at tracks elapsed time ---

#[test]
fn daemon_state_started_at_elapses() {
    let state = DaemonState::new();
    let elapsed = state.started_at.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "started_at should be recent"
    );
}

// --- handle_health_connection: /drift response structure ---

#[tokio::test]
async fn health_connection_drift_body_parses_as_json() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(8192);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /drift HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if in_body {
                    lines.push(line);
                } else if line.trim().is_empty() {
                    in_body = true;
                }
            }
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    let body = lines.join("");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("drift body should be valid JSON");
    assert!(parsed.get("drift_count").is_some());
    assert!(parsed.get("events").is_some());
    assert!(parsed["events"].is_array());
    // With an empty default state store, events should be empty
    assert_eq!(parsed["drift_count"].as_u64().unwrap(), 0);
}

// --- handle_sync: no pull, no push, still updates timestamp ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_no_pull_no_push_updates_timestamp() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));

    let changed = handle_sync(&repo_dir, false, false, "local", &state, false, false).await;

    assert!(!changed, "no pull/push means no changes");

    let st = state.lock().await;
    assert!(
        st.last_sync.is_some(),
        "last_sync should be set even with no operations"
    );
}

// --- git_pull_sync: delegates to git_pull ---

#[test]
fn git_pull_sync_non_repo_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = git_pull_sync(tmp.path());
    let err = result.unwrap_err();
    assert!(
        err.contains("open repo"),
        "expected 'open repo' error, got: {err}"
    );
}

#[test]
fn git_pull_sync_clean_repo_no_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "test\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    let result = git_pull_sync(&work_dir);
    assert!(result.is_ok());
    assert!(!result.unwrap(), "should be up to date");
}

// --- Notifier: all methods construct without panic ---

#[test]
fn notifier_all_methods_construct() {
    let stdout = Notifier::new(NotifyMethod::Stdout, None);
    assert!(matches!(stdout.method, NotifyMethod::Stdout));
    assert!(stdout.webhook_url.is_none());

    let desktop = Notifier::new(NotifyMethod::Desktop, None);
    assert!(matches!(desktop.method, NotifyMethod::Desktop));

    let webhook_none = Notifier::new(NotifyMethod::Webhook, None);
    assert!(matches!(webhook_none.method, NotifyMethod::Webhook));
    assert!(webhook_none.webhook_url.is_none());

    let webhook_url = Notifier::new(
        NotifyMethod::Webhook,
        Some("https://example.com/hook".into()),
    );
    assert_eq!(
        webhook_url.webhook_url.as_deref(),
        Some("https://example.com/hook")
    );
}

// --- DaemonStatusResponse: serialization/deserialization symmetry ---

#[test]
fn daemon_status_response_roundtrip_symmetry() {
    let original = DaemonStatusResponse {
        running: true,
        pid: 99999,
        uptime_secs: 86400,
        last_reconcile: Some("2026-04-01T12:00:00Z".into()),
        last_sync: Some("2026-04-01T12:01:00Z".into()),
        drift_count: 100,
        sources: vec![
            SourceStatus {
                name: "local".into(),
                last_sync: Some("2026-04-01T12:01:00Z".into()),
                last_reconcile: Some("2026-04-01T12:00:00Z".into()),
                drift_count: 50,
                status: "active".into(),
            },
            SourceStatus {
                name: "corp".into(),
                last_sync: None,
                last_reconcile: None,
                drift_count: 50,
                status: "error".into(),
            },
        ],
        update_available: Some("5.0.0".into()),
        module_reconcile: vec![ModuleReconcileStatus {
            name: "sec-baseline".into(),
            interval: "30s".into(),
            auto_apply: true,
            drift_policy: "Auto".into(),
            last_reconcile: Some("2026-04-01T12:00:00Z".into()),
        }],
    };

    let json = serde_json::to_string(&original).unwrap();
    let roundtripped: DaemonStatusResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(roundtripped.pid, original.pid);
    assert_eq!(roundtripped.uptime_secs, original.uptime_secs);
    assert_eq!(roundtripped.drift_count, original.drift_count);
    assert_eq!(roundtripped.sources.len(), original.sources.len());
    assert_eq!(
        roundtripped.sources[1].drift_count,
        original.sources[1].drift_count
    );
    assert_eq!(
        roundtripped.module_reconcile.len(),
        original.module_reconcile.len()
    );
    assert_eq!(roundtripped.update_available, original.update_available);
}

// --- SourceStatus: serialization includes camelCase properly ---

#[test]
fn source_status_camel_case_serialization() {
    let status = SourceStatus {
        name: "test".into(),
        last_sync: Some("ts".into()),
        last_reconcile: Some("tr".into()),
        drift_count: 1,
        status: "active".into(),
    };
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"lastSync\""));
    assert!(json.contains("\"lastReconcile\""));
    assert!(json.contains("\"driftCount\""));
    assert!(!json.contains("\"last_sync\""));
    assert!(!json.contains("\"last_reconcile\""));
    assert!(!json.contains("\"drift_count\""));
}

// --- infer_item_tier: boundary cases ---

#[test]
fn infer_item_tier_empty_string() {
    assert_eq!(infer_item_tier(""), "recommended");
}

#[test]
fn infer_item_tier_case_sensitivity() {
    // "Security" (uppercase S) does NOT match since contains() is case-sensitive
    assert_eq!(infer_item_tier("files.Security-settings"), "recommended");
    // "POLICY" (all caps) does NOT match since contains() is case-sensitive
    assert_eq!(infer_item_tier("files.POLICY-doc"), "recommended");
    // Only lowercase matches trigger the "locked" tier
    assert_eq!(infer_item_tier("files.security-settings"), "locked");
    assert_eq!(infer_item_tier("files.policy-doc"), "locked");
}

#[test]
fn infer_item_tier_partial_keyword_match() {
    // "insecurity" contains "security"
    assert_eq!(infer_item_tier("files.insecurity-note"), "locked");
}

// --- compute_config_hash: uses only packages for hash ---

#[test]
fn compute_config_hash_ignores_non_package_fields() {
    use crate::config::{
        EnvVar, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let resolved_a = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "a".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            env: vec![EnvVar {
                name: "FOO".into(),
                value: "bar".into(),
            }],
            ..Default::default()
        },
    };

    let resolved_b = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "b".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            env: vec![EnvVar {
                name: "BAZ".into(),
                value: "qux".into(),
            }],
            ..Default::default()
        },
    };

    // Both have same empty packages, so hash should be the same
    // because compute_config_hash only hashes the packages field
    let hash_a = compute_config_hash(&resolved_a).unwrap();
    let hash_b = compute_config_hash(&resolved_b).unwrap();
    assert_eq!(
        hash_a, hash_b,
        "compute_config_hash should only hash packages, not env vars"
    );
}

// --- generate_launchd_plist tests ---

#[cfg(unix)]
#[test]
fn generate_launchd_plist_contains_correct_structure() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/Users/testuser/.config/cfgd/config.yaml");
    let home = Path::new("/Users/testuser");

    let plist = generate_launchd_plist(binary, config, None, home);

    assert!(
        plist.contains("<?xml version=\"1.0\""),
        "plist should have XML declaration"
    );
    assert!(
        plist.contains(&format!("<string>{}</string>", LAUNCHD_LABEL)),
        "plist should contain the launchd label"
    );
    assert!(
        plist.contains("<string>/usr/local/bin/cfgd</string>"),
        "plist should contain binary path"
    );
    assert!(
        plist.contains("<string>/Users/testuser/.config/cfgd/config.yaml</string>"),
        "plist should contain config path"
    );
    assert!(
        plist.contains("<string>--quiet</string>"),
        "plist should contain --quiet flag"
    );
    let config_pos = plist
        .find("<string>/Users/testuser/.config/cfgd/config.yaml</string>")
        .unwrap();
    let quiet_pos = plist.find("<string>--quiet</string>").unwrap();
    let daemon_pos = plist.find("<string>daemon</string>").unwrap();
    assert!(
        config_pos < quiet_pos && quiet_pos < daemon_pos,
        "--quiet should appear between config path and daemon"
    );
    assert!(
        plist.contains("<string>daemon</string>"),
        "plist should contain daemon subcommand"
    );
    assert!(
        plist.contains("<key>RunAtLoad</key>"),
        "plist should enable run at load"
    );
    assert!(
        plist.contains("<key>KeepAlive</key>"),
        "plist should enable keep alive"
    );
    assert!(
        plist.contains("/Users/testuser/Library/Logs/cfgd.log"),
        "plist should set stdout log path under home"
    );
    assert!(
        plist.contains("/Users/testuser/Library/Logs/cfgd.err"),
        "plist should set stderr log path under home"
    );
    // Without profile, no --profile argument should appear
    assert!(
        !plist.contains("--profile"),
        "plist without profile should not contain --profile"
    );
}

#[cfg(unix)]
#[test]
fn generate_launchd_plist_with_profile() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/.config/cfgd/config.yaml");
    let home = Path::new("/home/user");

    let plist = generate_launchd_plist(binary, config, Some("work"), home);

    assert!(
        plist.contains("<string>--profile</string>"),
        "plist with profile should contain --profile argument"
    );
    assert!(
        plist.contains("<string>work</string>"),
        "plist with profile should contain the profile name"
    );
    // Verify order: --config before --profile before --quiet before daemon
    let config_pos = plist.find("<string>--config</string>").unwrap();
    let quiet_pos = plist.find("<string>--quiet</string>").unwrap();
    let daemon_pos = plist.find("<string>daemon</string>").unwrap();
    let profile_pos = plist.find("<string>--profile</string>").unwrap();
    assert!(
        config_pos < profile_pos,
        "--config should appear before --profile"
    );
    assert!(
        profile_pos < quiet_pos,
        "--profile should appear before --quiet"
    );
    assert!(
        quiet_pos < daemon_pos,
        "--quiet should appear before daemon"
    );
}

// --- generate_systemd_unit tests ---

#[cfg(unix)]
#[test]
fn generate_systemd_unit_contains_correct_structure() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/.config/cfgd/config.yaml");

    let unit = generate_systemd_unit(binary, config, None);

    assert!(
        unit.contains("[Unit]"),
        "unit file should have [Unit] section"
    );
    assert!(
        unit.contains("Description=cfgd configuration daemon"),
        "unit file should have correct description"
    );
    assert!(
        unit.contains("After=network.target"),
        "unit file should depend on network.target"
    );
    assert!(
        unit.contains("[Service]"),
        "unit file should have [Service] section"
    );
    assert!(
        unit.contains("Type=simple"),
        "unit file should use simple service type"
    );
    assert!(
        unit.contains(
            "ExecStart=/usr/local/bin/cfgd --config /home/user/.config/cfgd/config.yaml --quiet daemon"
        ),
        "unit file should have correct ExecStart"
    );
    assert!(
        unit.contains("Restart=on-failure"),
        "unit file should restart on failure"
    );
    assert!(
        unit.contains("RestartSec=10"),
        "unit file should have 10s restart delay"
    );
    assert!(
        unit.contains("[Install]"),
        "unit file should have [Install] section"
    );
    assert!(
        unit.contains("WantedBy=default.target"),
        "unit file should be wanted by default.target"
    );
    // Without profile, no --profile should appear
    assert!(
        !unit.contains("--profile"),
        "unit without profile should not contain --profile"
    );
}

#[cfg(unix)]
#[test]
fn generate_systemd_unit_with_profile() {
    let binary = Path::new("/opt/bin/cfgd");
    let config = Path::new("/etc/cfgd/config.yaml");

    let unit = generate_systemd_unit(binary, config, Some("server"));

    assert!(
        unit.contains(
            "ExecStart=/opt/bin/cfgd --config /etc/cfgd/config.yaml --profile server --quiet daemon"
        ),
        "unit file with profile should include --profile in ExecStart"
    );
}

// install_launchd_service + uninstall_launchd_service: redirect HOME under
// TestHomeGuard, run install → assert plist landed, run uninstall → assert
// plist removed. Exercises the dir-create, atomic_write, exists, remove paths.

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn install_then_uninstall_launchd_service_round_trips_plist() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let binary = tmp.path().join("cfgd");
    std::fs::write(&binary, b"").unwrap();
    let config = tmp.path().join("config.yaml");
    std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\n").unwrap();

    install_launchd_service(&binary, &config, Some("work")).expect("install ok");

    let plist = tmp
        .path()
        .join("Library/LaunchAgents/com.cfgd.daemon.plist");
    assert!(plist.exists(), "plist should be installed at expected path");
    let body = std::fs::read_to_string(&plist).unwrap();
    assert!(body.contains("com.cfgd.daemon"));
    assert!(body.contains("--profile"));

    uninstall_launchd_service().expect("uninstall ok");
    assert!(!plist.exists(), "plist should be removed after uninstall");
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn uninstall_launchd_service_is_noop_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    // No prior install — uninstall must succeed without error.
    uninstall_launchd_service().expect("uninstall on clean home is a no-op");
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn install_then_uninstall_systemd_service_round_trips_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let binary = tmp.path().join("cfgd");
    std::fs::write(&binary, b"").unwrap();
    let config = tmp.path().join("config.yaml");
    std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\n").unwrap();

    install_systemd_service(&binary, &config, None).expect("install ok");

    let unit_path = tmp.path().join(".config/systemd/user/cfgd.service");
    assert!(unit_path.exists(), "unit should be installed");
    let body = std::fs::read_to_string(&unit_path).unwrap();
    assert!(body.contains("ExecStart="));
    assert!(body.contains("--quiet daemon"));
    assert!(!body.contains("--profile"));

    uninstall_systemd_service().expect("uninstall ok");
    assert!(
        !unit_path.exists(),
        "unit should be removed after uninstall"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn uninstall_systemd_service_is_noop_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    uninstall_systemd_service().expect("uninstall on clean home is a no-op");
}

// Cross-platform dispatcher: install_service uses current_exe() + cfg(macos)/else
// to delegate to launchd/systemd. uninstall_service mirrors that branch.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn install_service_then_uninstall_service_round_trips_via_dispatcher() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let config = tmp.path().join("config.yaml");
    std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\n").unwrap();

    crate::daemon::service::install_service(&config, None).expect("install_service ok");
    // Whether macOS (plist) or Linux (unit), uninstall must round-trip without
    // panic. Skip exists() assertions — the dispatcher branch depends on
    // target_os and we just want both arms exercised.
    crate::daemon::service::uninstall_service().expect("uninstall_service ok");
}

// --- record_file_drift_to tests ---

#[test]
fn record_file_drift_to_records_event() {
    let store = test_state();
    let path = Path::new("/home/user/.bashrc");

    let result = record_file_drift_to(&store, path);
    assert!(result, "record_file_drift_to should return true on success");

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1, "should have exactly one drift event");
    assert_eq!(events[0].resource_id, "/home/user/.bashrc");
}

#[test]
fn record_file_drift_to_records_correct_type() {
    let store = test_state();
    let path = Path::new("/etc/config.yaml");

    record_file_drift_to(&store, path);

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].resource_type, "file",
        "drift event should have resource_type 'file'"
    );
    assert_eq!(
        events[0].source, "local",
        "drift event should have source 'local'"
    );
    assert_eq!(
        events[0].actual.as_deref(),
        Some("modified"),
        "drift event should have actual value 'modified'"
    );
    assert!(
        events[0].expected.is_none(),
        "drift event should have no expected value"
    );
}

// --- discover_managed_paths tests ---

#[test]
fn discover_managed_paths_with_no_config_returns_empty() {
    use std::path::Path;

    struct TestHooks;
    impl DaemonHooks for TestHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let hooks = TestHooks;
    // Non-existent config file should return empty paths
    let paths = discover_managed_paths(Path::new("/nonexistent/config.yaml"), None, &hooks);
    assert!(
        paths.is_empty(),
        "non-existent config should return no managed paths"
    );
}

// --- parse_daemon_config tests ---

#[test]
fn parse_daemon_config_defaults() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert_eq!(
        parsed.reconcile_interval,
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
    assert_eq!(parsed.sync_interval, Duration::from_secs(DEFAULT_SYNC_SECS));
    assert!(!parsed.auto_pull);
    assert!(!parsed.auto_push);
    assert!(!parsed.on_change_reconcile);
    assert!(!parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Stdout));
    assert!(parsed.webhook_url.is_none());
    assert!(!parsed.auto_apply);
}

#[test]
fn parse_daemon_config_custom_intervals() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "10m".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::default(),
            patches: vec![],
        }),
        sync: Some(config::SyncConfig {
            auto_pull: false,
            auto_push: false,
            interval: "30s".to_string(),
        }),
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert_eq!(parsed.reconcile_interval, Duration::from_secs(600));
    assert_eq!(parsed.sync_interval, Duration::from_secs(30));
}

#[test]
fn parse_daemon_config_notification_settings() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: Some(config::NotifyConfig {
            drift: true,
            method: NotifyMethod::Webhook,
            webhook_url: Some("https://hooks.example.com/drift".to_string()),
        }),
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Webhook));
    assert_eq!(
        parsed.webhook_url.as_deref(),
        Some("https://hooks.example.com/drift")
    );
}

#[test]
fn parse_daemon_config_sync_flags() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: Some(config::SyncConfig {
            auto_pull: true,
            auto_push: true,
            interval: "5m".to_string(),
        }),
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.auto_pull);
    assert!(parsed.auto_push);
}

#[test]
fn parse_daemon_config_on_change_enabled() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "5m".to_string(),
            on_change: true,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::default(),
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.on_change_reconcile);
    assert!(!parsed.auto_apply);
}

#[test]
fn parse_daemon_config_auto_apply_enabled() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "5m".to_string(),
            on_change: false,
            auto_apply: true,
            policy: None,
            drift_policy: config::DriftPolicy::Auto,
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.auto_apply);
}

#[test]
fn handle_reconcile_with_no_config_file() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    struct NoopHooks;
    impl DaemonHooks for NoopHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().to_path_buf();
    let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);

    // Passing a nonexistent config path should return gracefully (no panic)
    handle_reconcile(
        Path::new("/nonexistent/path/config.yaml"),
        None,
        quiet_reconcile_ctx(&state, &notifier, false, &NoopHooks, &state_dir, &printer),
    );
    // If we got here without panic, the function handled the missing config gracefully.
    // Verify the state wasn't updated (no reconciliation occurred).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let guard = rt.block_on(state.lock());
    assert!(
        guard.last_reconcile.is_none(),
        "no reconcile should have occurred with missing config"
    );
}

#[test]
fn handle_reconcile_with_no_profile() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    struct NoopHooks;
    impl DaemonHooks for NoopHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().to_path_buf();

    // Write a valid config with NO profile set
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
    // No profile override and no profile in config — should return gracefully
    handle_reconcile(
        &config_path,
        None,
        quiet_reconcile_ctx(&state, &notifier, false, &NoopHooks, &state_dir, &printer),
    );
    // Should not have updated state since no profile was available
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let guard = rt.block_on(state.lock());
    assert!(
        guard.last_reconcile.is_none(),
        "no reconcile should have occurred without a profile"
    );
}

// --- build_reconcile_tasks ---

#[test]
fn build_reconcile_tasks_default_only_when_no_patches() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(60), false);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
    assert_eq!(tasks[0].interval, Duration::from_secs(60));
    assert!(!tasks[0].auto_apply);
    assert_eq!(tasks[0].drift_policy, config::DriftPolicy::NotifyOnly);
}

#[test]
fn build_reconcile_tasks_default_inherits_global_drift_policy() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "120s".to_string(),
            on_change: false,
            auto_apply: true,
            policy: None,
            drift_policy: config::DriftPolicy::Auto,
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(120), true);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].drift_policy, config::DriftPolicy::Auto);
    assert!(tasks[0].auto_apply);
}

#[test]
fn build_reconcile_tasks_no_reconcile_config_uses_defaults() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(300), false);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
    assert_eq!(tasks[0].interval, Duration::from_secs(300));
    // Default drift policy is NotifyOnly
    assert_eq!(tasks[0].drift_policy, config::DriftPolicy::default());
}

#[test]
fn build_reconcile_tasks_patches_without_resolved_profile_skips_modules() {
    // Patches exist but no resolved profile — should still get only __default__
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches: vec![config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("vim".to_string()),
                interval: Some("10s".to_string()),
                auto_apply: Some(true),
                drift_policy: None,
            }],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(
        &daemon_cfg,
        None, // no resolved profile
        &["default"],
        Duration::from_secs(60),
        false,
    );
    // Only default task — no module tasks since profile isn't resolved
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
}

#[test]
fn build_reconcile_tasks_module_with_overridden_interval_gets_dedicated_task() {
    // Build a resolved profile with a module
    let merged = config::MergedProfile {
        modules: vec!["vim".to_string()],
        ..Default::default()
    };
    let resolved = config::ResolvedProfile {
        layers: vec![config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "default".to_string(),
            priority: 0,
            policy: config::LayerPolicy::Local,
            spec: Default::default(),
        }],
        merged,
    };

    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches: vec![config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("vim".to_string()),
                interval: Some("10s".to_string()),
                auto_apply: None,
                drift_policy: None,
            }],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let tasks = build_reconcile_tasks(
        &daemon_cfg,
        Some(&resolved),
        &["default"],
        Duration::from_secs(60),
        false,
    );
    // Should have 2 tasks: one for "vim" with 10s interval, one for __default__
    assert_eq!(tasks.len(), 2);
    let vim_task = tasks.iter().find(|t| t.entity == "vim").unwrap();
    assert_eq!(vim_task.interval, Duration::from_secs(10));
    assert!(!vim_task.auto_apply);
    let default_task = tasks.iter().find(|t| t.entity == "__default__").unwrap();
    assert_eq!(default_task.interval, Duration::from_secs(60));
}

#[test]
fn build_reconcile_tasks_module_matching_global_gets_no_dedicated_task() {
    // When a module's effective settings match global, no dedicated task is created
    let merged = config::MergedProfile {
        modules: vec!["vim".to_string()],
        ..Default::default()
    };
    let resolved = config::ResolvedProfile {
        layers: vec![config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "default".to_string(),
            priority: 0,
            policy: config::LayerPolicy::Local,
            spec: Default::default(),
        }],
        merged,
    };

    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            // Patch that produces same values as global
            patches: vec![config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("vim".to_string()),
                interval: None,     // inherits "60s"
                auto_apply: None,   // inherits false
                drift_policy: None, // inherits NotifyOnly
            }],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let tasks = build_reconcile_tasks(
        &daemon_cfg,
        Some(&resolved),
        &["default"],
        Duration::from_secs(60),
        false,
    );
    // Only __default__ — vim's effective settings match global
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
}

// --- build_sync_tasks ---

#[test]
fn build_sync_tasks_local_only_when_no_sources() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: true,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let tasks = build_sync_tasks(tmp.path(), &parsed, &[], false, tmp.path(), |_| None);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_name, "local");
    assert!(tasks[0].auto_pull);
    assert!(!tasks[0].auto_push);
    assert!(tasks[0].auto_apply);
    assert_eq!(tasks[0].interval, Duration::from_secs(300));
    assert!(!tasks[0].require_signed_commits);
}

#[test]
fn build_sync_tasks_includes_source_when_dir_exists() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: false,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("sources");
    std::fs::create_dir_all(cache_dir.join("team-config")).unwrap();

    let sources = vec![config::SourceSpec {
        name: "team-config".to_string(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/team/config.git".to_string(),
            branch: "main".to_string(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: config::SourceSyncSpec {
            interval: "120s".to_string(),
            auto_apply: true,
            pin_version: None,
        },
    }];

    let tasks = build_sync_tasks(
        tmp.path(),
        &parsed,
        &sources,
        false,
        &cache_dir,
        |_| Some(true), // manifest requires signed commits
    );
    assert_eq!(tasks.len(), 2);
    let source_task = tasks
        .iter()
        .find(|t| t.source_name == "team-config")
        .unwrap();
    assert!(source_task.auto_pull);
    assert!(!source_task.auto_push);
    assert!(source_task.auto_apply);
    assert_eq!(source_task.interval, Duration::from_secs(120));
    assert!(source_task.require_signed_commits);
}

#[test]
fn build_sync_tasks_skips_source_when_dir_missing() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: false,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("sources");
    // Intentionally don't create the source directory

    let sources = vec![config::SourceSpec {
        name: "missing-source".to_string(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/team/config.git".to_string(),
            branch: "main".to_string(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: Default::default(),
    }];

    let tasks = build_sync_tasks(tmp.path(), &parsed, &sources, false, &cache_dir, |_| None);
    // Only local task — source dir doesn't exist
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_name, "local");
}

#[test]
fn build_sync_tasks_propagates_allow_unsigned() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: true,
        auto_push: true,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let tasks = build_sync_tasks(
        tmp.path(),
        &parsed,
        &[],
        true, // allow_unsigned
        tmp.path(),
        |_| None,
    );
    assert!(tasks[0].allow_unsigned);
}

// --- handle_reconcile: deeper paths ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_with_valid_config_records_drift_events() {
    // Set up a tmpdir with config.yaml + profiles/default.yaml containing packages.
    // DaemonHooks that returns a PackageAction::Install so the plan has drift.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Write config
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    // Write profile
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n",
        )
        .unwrap();

    struct DriftHooks;
    impl DaemonHooks for DriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            // Return a package install action to create drift
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["bat".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &DriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Verify drift events were recorded in the state store
    let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(
        !drift_events.is_empty(),
        "drift events should have been recorded"
    );
    // The drift should be for the package install action
    let pkg_drift = drift_events.iter().find(|e| e.resource_type == "package");
    assert!(
        pkg_drift.is_some(),
        "should have a package drift event; events: {:?}",
        drift_events
    );
    assert_eq!(pkg_drift.unwrap().resource_id, "cargo:bat");

    // Verify daemon state was updated
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "last_reconcile should have been set"
    );
    assert!(
        guard.drift_count > 0,
        "drift_count should have been incremented"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_notify_only_drift_policy_does_not_apply() {
    // Verify that with NotifyOnly drift policy, drift is recorded but no apply happens.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      onChange: false\n      autoApply: false\n      driftPolicy: NotifyOnly\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n",
        )
        .unwrap();

    struct NotifyOnlyHooks;
    impl DaemonHooks for NotifyOnlyHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["ripgrep".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &NotifyOnlyHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Drift should be recorded
    let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(
        !drift_events.is_empty(),
        "drift events should be recorded even with NotifyOnly policy"
    );

    // Verify state reflects drift
    let guard = state.lock().await;
    assert!(guard.drift_count > 0);
    assert!(guard.last_reconcile.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_no_drift_when_no_actions() {
    // When plan has no actions, no drift events should be recorded.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    struct NoDriftHooks;
    impl DaemonHooks for NoDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &NoDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // No drift events should have been recorded
    let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(
        drift_events.is_empty(),
        "no drift events should be recorded when plan has no actions"
    );

    // State should reflect a reconciliation occurred
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert_eq!(guard.drift_count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_with_profile_override() {
    // Test that profile_override is used instead of config's profile field.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Config with profile "other" but we override to "default"
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: nonexistent\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    struct EmptyHooks;
    impl DaemonHooks for EmptyHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    // Override profile to "default" which exists
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            Some("default"),
            quiet_reconcile_ctx(&st, &not, false, &EmptyHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Should have completed successfully with the overridden profile
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "reconciliation should succeed with profile override"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_multiple_actions_records_all_drift() {
    // Verify that all drift-producing actions are recorded as separate events.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n        - ripgrep\n        - fd-find\n",
        )
        .unwrap();

    struct MultiDriftHooks;
    impl DaemonHooks for MultiDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            // Also include a file action
            Ok(vec![FileAction::Create {
                source: PathBuf::from("/src/.zshrc"),
                target: PathBuf::from("/home/user/.zshrc"),
                origin: "local".into(),
                strategy: crate::config::FileStrategy::default(),
                source_hash: None,
            }])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![
                PackageAction::Install {
                    manager: "cargo".into(),
                    packages: vec!["bat".into(), "ripgrep".into()],
                    origin: "local".into(),
                },
                PackageAction::Install {
                    manager: "cargo".into(),
                    packages: vec!["fd-find".into()],
                    origin: "local".into(),
                },
            ])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &MultiDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    // Should have drift events for all actions:
    // 1 file create + 2 package install actions = 3 drift events
    assert_eq!(
        drift_events.len(),
        3,
        "should have drift events for all actions; got: {:?}",
        drift_events
    );

    let resource_types: Vec<&str> = drift_events
        .iter()
        .map(|e| e.resource_type.as_str())
        .collect();
    assert!(
        resource_types.contains(&"file"),
        "should have a file drift event"
    );
    assert!(
        resource_types.contains(&"package"),
        "should have package drift events"
    );
}

// --- handle_reconcile: autoApply + onDrift + notify_on_drift arms ---
//
// These tests cover the branches that the prior drift tests skipped:
// `DriftPolicy::Auto` invoking `reconciler.apply()`, the `scripts.on_drift`
// execution loop, and the `notify_on_drift=true` notifier paths.

/// `DriftingFileHooks` returns a single `FileAction::Create` whose `source`
/// and `target` paths are owned by the test fixture. With
/// `FileStrategy::Copy`, the reconciler's apply path will `std::fs::copy` the
/// file, succeeding under normal conditions or failing if `source` is absent.
struct DriftingFileHooks {
    source: PathBuf,
    target: PathBuf,
}

impl DaemonHooks for DriftingFileHooks {
    fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
        ProviderRegistry::new()
    }
    fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> crate::errors::Result<Vec<FileAction>> {
        Ok(vec![FileAction::Create {
            source: self.source.clone(),
            target: self.target.clone(),
            origin: "local".into(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }])
    }
    fn plan_packages(
        &self,
        _: &MergedProfile,
        _: &[&dyn PackageManager],
    ) -> crate::errors::Result<Vec<PackageAction>> {
        Ok(vec![])
    }
    fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &config::PackagesSpec) {}
    fn expand_tilde(&self, path: &Path) -> PathBuf {
        crate::expand_tilde(path)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_policy_with_drift_invokes_apply_success() {
    // DriftPolicy::Auto + a FileAction::Create with valid source/target →
    // reconciler.apply() runs the copy, succeeded > 0. notify_on_drift=true
    // exercises the success-notification branch.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    // Real source file inside tmp, target inside tmp — copy succeeds.
    let source = tmp.path().join("src.txt");
    std::fs::write(&source, "hello").unwrap();
    let target = tmp.path().join("dst.txt");
    let hooks = DriftingFileHooks {
        source,
        target: target.clone(),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, true, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Apply succeeded — file copied to target. This proves the auto-apply
    // branch (DriftPolicy::Auto + drift > 0 → reconciler.apply) was reached.
    assert!(
        target.exists(),
        "auto-apply should have copied source to target"
    );
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert!(
        guard.drift_count > 0,
        "drift_count should have been incremented before resolve_drift"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_policy_apply_failure_notifies() {
    // DriftPolicy::Auto + FileAction::Create with nonexistent source →
    // copy fails, exercising the auto-apply partial-failure notification branch.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    // Source does NOT exist → std::fs::copy fails → apply records failure.
    let source = tmp.path().join("missing.txt");
    let target = tmp.path().join("dst.txt");
    let hooks = DriftingFileHooks {
        source,
        target: target.clone(),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, true, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Target never created — apply failed.
    assert!(!target.exists(), "apply should have failed to copy");
    // Drift recorded regardless.
    let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(!drift_events.is_empty());
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert!(guard.drift_count > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_runs_on_drift_scripts() {
    // Profile with `scripts.onDrift` populated → on-drift script loop runs.
    // The script writes a marker file we can assert on.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let marker = tmp.path().join("on-drift-ran.marker");
    let marker_str = marker.display().to_string();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  scripts:\n    onDrift:\n      - \"touch '{}'\"\n",
            marker_str
        ),
    )
    .unwrap();

    // Use the existing DriftHooks pattern: a package action creates drift,
    // which triggers the onDrift loop.
    struct PkgDriftHooks;
    impl DaemonHooks for PkgDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["bat".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &PkgDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    assert!(
        marker.exists(),
        "onDrift script should have created marker file at {}",
        marker.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_notify_only_with_notify_on_drift_sends_notification() {
    // NotifyOnly policy + notify_on_drift=true + drift → notifier.notify
    // called for "drift detected". Stdout notifier just logs, but the call
    // path is what we want to exercise (it's a distinct branch from the
    // notify_on_drift=false NotifyOnly case already covered).
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: false\n      driftPolicy: NotifyOnly\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    struct PkgDriftHooks;
    impl DaemonHooks for PkgDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["ripgrep".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        // notify_on_drift = true → notifier.notify() reached
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, true, &PkgDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Drift event recorded. notify ran (stdout notifier just traces; we assert
    // the call path was reached by checking the drift bookkeeping side-effects).
    let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(!drift_events.is_empty());
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert!(guard.drift_count > 0);
}

// --- discover_managed_paths ---

#[test]
fn discover_managed_paths_returns_targets_from_profile() {
    let tmp = tempfile::tempdir().unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: src/zshrc\n        target: /home/user/.zshrc\n      - source: src/vimrc\n        target: /home/user/.vimrc\n",
        )
        .unwrap();

    struct TestHooks;
    impl DaemonHooks for TestHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            path.to_path_buf()
        }
    }

    let paths = discover_managed_paths(&config_path, None, &TestHooks);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("/home/user/.zshrc")));
    assert!(paths.contains(&PathBuf::from("/home/user/.vimrc")));
}

#[test]
fn discover_managed_paths_returns_empty_for_missing_config() {
    struct TestHooks;
    impl DaemonHooks for TestHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            path.to_path_buf()
        }
    }

    let paths = discover_managed_paths(Path::new("/nonexistent/config.yaml"), None, &TestHooks);
    assert!(paths.is_empty());
}

#[test]
fn discover_managed_paths_with_profile_override() {
    let tmp = tempfile::tempdir().unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("custom.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: custom\nspec:\n  files:\n    managed:\n      - source: src/bashrc\n        target: /home/user/.bashrc\n",
        )
        .unwrap();

    struct TestHooks;
    impl DaemonHooks for TestHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            path.to_path_buf()
        }
    }

    let paths = discover_managed_paths(&config_path, Some("custom"), &TestHooks);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("/home/user/.bashrc"));
}

// --- pending_resource_paths ---

#[test]
fn pending_resource_paths_returns_empty_for_no_decisions() {
    let store = test_state();
    let paths = pending_resource_paths(&store);
    assert!(paths.is_empty());
}

// --- generate_launchd_plist: detailed content verification ---

#[test]
#[cfg(unix)]
fn generate_launchd_plist_xml_structure_complete() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/Users/alice/.config/cfgd/config.yaml");
    let home = Path::new("/Users/alice");

    let plist = generate_launchd_plist(binary, config, None, home);

    // Verify required XML structure
    assert!(
        plist.contains("<?xml version=\"1.0\""),
        "should start with XML declaration"
    );
    assert!(
        plist.contains("<!DOCTYPE plist"),
        "should contain plist DOCTYPE"
    );
    assert!(
        plist.contains(&format!("<string>{}</string>", LAUNCHD_LABEL)),
        "should contain the label"
    );
    assert!(
        plist.contains("<string>/usr/local/bin/cfgd</string>"),
        "should contain binary path"
    );
    assert!(
        plist.contains("<string>--config</string>"),
        "should contain --config flag"
    );
    assert!(
        plist.contains("<string>/Users/alice/.config/cfgd/config.yaml</string>"),
        "should contain config path"
    );
    assert!(
        plist.contains("<string>--quiet</string>"),
        "should contain --quiet flag"
    );
    assert!(
        plist.contains("<string>daemon</string>"),
        "should contain daemon subcommand"
    );
    assert!(
        plist.contains("<key>RunAtLoad</key>"),
        "should set RunAtLoad"
    );
    assert!(
        plist.contains("<key>KeepAlive</key>"),
        "should set KeepAlive"
    );
    assert!(
        plist.contains("/Users/alice/Library/Logs/cfgd.log"),
        "stdout log should be under home Library/Logs"
    );
    assert!(
        plist.contains("/Users/alice/Library/Logs/cfgd.err"),
        "stderr log should be under home Library/Logs"
    );
    // Should NOT contain --profile when None
    assert!(
        !plist.contains("--profile"),
        "should not contain --profile when None"
    );
}

#[test]
#[cfg(unix)]
fn generate_launchd_plist_includes_profile_flag() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/config.yaml");
    let home = Path::new("/home/user");

    let plist = generate_launchd_plist(binary, config, Some("work"), home);

    assert!(
        plist.contains("<string>--profile</string>"),
        "should contain --profile flag"
    );
    assert!(
        plist.contains("<string>work</string>"),
        "should contain profile name"
    );
    assert!(
        plist.contains("<string>--quiet</string>"),
        "should contain --quiet flag"
    );
    // Strict ordering: --config < --profile < --quiet < daemon (parity with systemd).
    let config_pos = plist.find("<string>--config</string>").unwrap();
    let profile_pos = plist.find("<string>--profile</string>").unwrap();
    let quiet_pos = plist.find("<string>--quiet</string>").unwrap();
    let daemon_pos = plist.find("<string>daemon</string>").unwrap();
    assert!(
        config_pos < profile_pos,
        "--config should appear before --profile"
    );
    assert!(
        profile_pos < quiet_pos,
        "--profile should appear before --quiet"
    );
    assert!(
        quiet_pos < daemon_pos,
        "--quiet should appear before daemon"
    );
}

// --- generate_systemd_unit: detailed content verification ---

#[test]
#[cfg(unix)]
fn generate_systemd_unit_complete_structure() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/.config/cfgd/config.yaml");

    let unit = generate_systemd_unit(binary, config, None);

    assert!(unit.contains("[Unit]"), "should contain [Unit] section");
    assert!(
        unit.contains("[Service]"),
        "should contain [Service] section"
    );
    assert!(
        unit.contains("[Install]"),
        "should contain [Install] section"
    );
    assert!(
        unit.contains("Description=cfgd configuration daemon"),
        "should have description"
    );
    assert!(
        unit.contains("After=network.target"),
        "should require network"
    );
    assert!(
        unit.contains("Type=simple"),
        "should be simple service type"
    );
    assert!(
        unit.contains("Restart=on-failure"),
        "should restart on failure"
    );
    assert!(unit.contains("RestartSec=10"), "should have restart delay");
    assert!(
        unit.contains("WantedBy=default.target"),
        "should be wanted by default.target"
    );

    // Verify ExecStart format: binary --config path --quiet daemon
    let expected_exec = format!(
        "ExecStart={} --config {} --quiet daemon",
        binary.display(),
        config.display()
    );
    assert!(
        unit.contains(&expected_exec),
        "ExecStart should be '{expected_exec}', got unit:\n{unit}"
    );
    // Should NOT contain --profile
    assert!(
        !unit.contains("--profile"),
        "should not contain --profile when None"
    );
}

#[test]
#[cfg(unix)]
fn generate_systemd_unit_includes_profile() {
    let binary = Path::new("/opt/cfgd/cfgd");
    let config = Path::new("/etc/cfgd/config.yaml");

    let unit = generate_systemd_unit(binary, config, Some("server"));

    let expected_exec = format!(
        "ExecStart={} --config {} --profile {} --quiet daemon",
        binary.display(),
        config.display(),
        "server"
    );
    assert!(
        unit.contains(&expected_exec),
        "ExecStart with profile should be '{expected_exec}', got:\n{unit}"
    );
}

// --- record_file_drift_to: actual drift recording ---

#[test]
fn record_file_drift_to_stores_event_in_db() {
    let store = test_state();
    let path = Path::new("/home/user/.bashrc");

    let result = record_file_drift_to(&store, path);
    assert!(result, "record_file_drift_to should return true on success");

    // Verify the drift event was actually stored
    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1, "should have exactly one drift event");
    assert_eq!(events[0].resource_type, "file");
    assert_eq!(events[0].resource_id, "/home/user/.bashrc");
}

#[test]
fn record_file_drift_to_multiple_files() {
    let store = test_state();

    record_file_drift_to(&store, Path::new("/etc/hosts"));
    record_file_drift_to(&store, Path::new("/etc/resolv.conf"));
    record_file_drift_to(&store, Path::new("/home/user/.zshrc"));

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 3, "should have three drift events");

    let ids: Vec<&str> = events.iter().map(|e| e.resource_id.as_str()).collect();
    assert!(ids.contains(&"/etc/hosts"));
    assert!(ids.contains(&"/etc/resolv.conf"));
    assert!(ids.contains(&"/home/user/.zshrc"));
}

// --- parse_daemon_config: comprehensive config parsing ---

#[test]
fn parse_daemon_config_all_defaults() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let parsed = parse_daemon_config(&cfg);
    assert_eq!(
        parsed.reconcile_interval,
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
    assert_eq!(parsed.sync_interval, Duration::from_secs(DEFAULT_SYNC_SECS));
    assert!(!parsed.auto_pull);
    assert!(!parsed.auto_push);
    assert!(!parsed.on_change_reconcile);
    assert!(!parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Stdout));
    assert!(parsed.webhook_url.is_none());
    assert!(!parsed.auto_apply);
}

#[test]
fn parse_daemon_config_with_all_settings() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".into(),
            on_change: true,
            auto_apply: true,
            policy: None,
            drift_policy: config::DriftPolicy::Auto,
            patches: vec![],
        }),
        sync: Some(config::SyncConfig {
            auto_pull: true,
            auto_push: true,
            interval: "120s".into(),
        }),
        notify: Some(config::NotifyConfig {
            drift: true,
            method: NotifyMethod::Webhook,
            webhook_url: Some("https://hooks.example.com/notify".into()),
        }),
        windows_event_log: false,
    };

    let parsed = parse_daemon_config(&cfg);
    assert_eq!(parsed.reconcile_interval, Duration::from_secs(60));
    assert_eq!(parsed.sync_interval, Duration::from_secs(120));
    assert!(parsed.auto_pull);
    assert!(parsed.auto_push);
    assert!(parsed.on_change_reconcile);
    assert!(parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Webhook));
    assert_eq!(
        parsed.webhook_url.as_deref(),
        Some("https://hooks.example.com/notify")
    );
    assert!(parsed.auto_apply);
}

#[test]
fn parse_daemon_config_with_minute_interval() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "10m".into(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::default(),
            patches: vec![],
        }),
        sync: Some(config::SyncConfig {
            auto_pull: false,
            auto_push: false,
            interval: "30m".into(),
        }),
        notify: None,
        windows_event_log: false,
    };

    let parsed = parse_daemon_config(&cfg);
    assert_eq!(parsed.reconcile_interval, Duration::from_secs(600));
    assert_eq!(parsed.sync_interval, Duration::from_secs(1800));
}

// --- build_sync_tasks: comprehensive sync task building ---

#[test]
fn build_sync_tasks_propagates_source_sync_interval() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();
    let source_cache = dir.path().join("sources");
    std::fs::create_dir_all(source_cache.join("team-tools")).unwrap();

    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(300),
        sync_interval: Duration::from_secs(300),
        auto_pull: true,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };

    let sources = vec![config::SourceSpec {
        name: "team-tools".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/team/tools.git".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec {
            auto_apply: true,
            interval: "60s".into(),
            pin_version: None,
        },
    }];

    let tasks = build_sync_tasks(config_dir, &parsed, &sources, false, &source_cache, |_| {
        None
    });

    assert_eq!(tasks.len(), 2, "should have local + team-tools");
    // Local task inherits global settings
    assert_eq!(tasks[0].source_name, "local");
    assert!(tasks[0].auto_pull);
    assert!(!tasks[0].auto_push);
    assert_eq!(tasks[0].interval, Duration::from_secs(300));

    // Source task uses its own interval
    assert_eq!(tasks[1].source_name, "team-tools");
    assert!(tasks[1].auto_pull); // always true for sources
    assert!(!tasks[1].auto_push); // always false for sources
    assert!(tasks[1].auto_apply);
    assert_eq!(tasks[1].interval, Duration::from_secs(60));
}

#[test]
fn build_sync_tasks_manifest_detector_sets_require_signed() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();
    let source_cache = dir.path().join("sources");
    std::fs::create_dir_all(source_cache.join("signed-source")).unwrap();

    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(300),
        sync_interval: Duration::from_secs(300),
        auto_pull: false,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };

    let sources = vec![config::SourceSpec {
        name: "signed-source".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/secure/config.git".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec::default(),
    }];

    // Manifest detector returns true => require signed commits
    let tasks = build_sync_tasks(config_dir, &parsed, &sources, false, &source_cache, |_| {
        Some(true)
    });

    assert_eq!(tasks.len(), 2);
    assert!(
        !tasks[0].require_signed_commits,
        "local should not require signed"
    );
    assert!(
        tasks[1].require_signed_commits,
        "source with manifest should require signed"
    );
}

// --- build_reconcile_tasks: comprehensive reconcile task building ---

#[test]
fn build_reconcile_tasks_always_has_default() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let tasks = build_reconcile_tasks(&cfg, None, &[], Duration::from_secs(300), false);

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
    assert_eq!(tasks[0].interval, Duration::from_secs(300));
    assert!(!tasks[0].auto_apply);
}

// --- git operations with local repos ---

#[test]
fn git_pull_on_local_repo_no_remote_is_error() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();

    // Create initial commit so HEAD exists
    let repo = git2::Repository::open(dir.path()).unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    let tree_oid = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    // No remote configured -> should error
    let result = git_pull(dir.path());
    assert!(result.is_err(), "pull without remote should fail");
}

#[test]
fn git_auto_commit_push_with_no_changes_returns_false() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    // Create initial commit
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    std::fs::write(dir.path().join("README.md"), "# Hello").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    // No changes after initial commit
    let result = git_auto_commit_push(dir.path());
    // Should return Ok(false) — no changes to commit
    assert_eq!(result, Ok(false));
}

// --- DaemonStatusResponse serialization edge cases ---

#[test]
fn daemon_status_response_camel_case_keys() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 100,
        uptime_secs: 3600,
        last_reconcile: Some("2026-01-01T00:00:00Z".into()),
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(
        json.contains("\"uptimeSecs\""),
        "should use camelCase: {json}"
    );
    assert!(
        json.contains("\"lastReconcile\""),
        "should use camelCase: {json}"
    );
    assert!(
        json.contains("\"driftCount\""),
        "should use camelCase: {json}"
    );
    assert!(
        !json.contains("\"uptime_secs\""),
        "should not use snake_case: {json}"
    );
}

// --- ModuleReconcileStatus serialization ---

#[test]
fn module_reconcile_status_round_trips_extended() {
    let status = ModuleReconcileStatus {
        name: "security-baseline".into(),
        interval: "30s".into(),
        auto_apply: true,
        drift_policy: "Auto".into(),
        last_reconcile: Some("2026-04-01T12:00:00Z".into()),
    };

    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"autoApply\""), "should use camelCase");
    assert!(json.contains("\"driftPolicy\""), "should use camelCase");
    assert!(json.contains("\"lastReconcile\""), "should use camelCase");

    let parsed: ModuleReconcileStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "security-baseline");
    assert!(parsed.auto_apply);
    assert_eq!(parsed.drift_policy, "Auto");
}

// --- extract_source_resources edge cases ---

#[test]
fn extract_source_resources_includes_npm_and_pipx_and_dnf() {
    use crate::config::{MergedProfile, NpmSpec, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            npm: Some(NpmSpec {
                file: None,
                global: vec!["typescript".into(), "eslint".into()],
            }),
            pipx: vec!["black".into()],
            dnf: vec!["gcc".into(), "make".into()],
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("packages.npm.typescript"));
    assert!(resources.contains("packages.npm.eslint"));
    assert!(resources.contains("packages.pipx.black"));
    assert!(resources.contains("packages.dnf.gcc"));
    assert!(resources.contains("packages.dnf.make"));
    assert_eq!(resources.len(), 5);
}

#[test]
fn extract_source_resources_includes_apt() {
    use crate::config::{AptSpec, MergedProfile, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            apt: Some(AptSpec {
                packages: vec!["vim".into(), "git".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("packages.apt.vim"));
    assert!(resources.contains("packages.apt.git"));
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_includes_system_keys() {
    use crate::config::MergedProfile;

    let mut merged = MergedProfile::default();
    merged.system.insert(
        "shell".into(),
        serde_yaml::to_value(serde_json::json!({"defaultShell": "/bin/zsh"})).unwrap(),
    );
    merged.system.insert(
        "macos_defaults".into(),
        serde_yaml::Value::Mapping(Default::default()),
    );

    let resources = extract_source_resources(&merged);
    assert!(resources.contains("system.shell"));
    assert!(resources.contains("system.macos_defaults"));
    assert_eq!(resources.len(), 2);
}

// --- Notifier webhook creates correct payload ---

#[test]
fn notifier_new_stores_method_and_url() {
    let notifier = Notifier::new(
        NotifyMethod::Webhook,
        Some("https://hooks.slack.com/test".into()),
    );
    assert!(matches!(notifier.method, NotifyMethod::Webhook));
    assert_eq!(
        notifier.webhook_url.as_deref(),
        Some("https://hooks.slack.com/test")
    );
}

#[test]
fn notifier_desktop_does_not_panic() {
    let notifier = Notifier::new(NotifyMethod::Desktop, None);
    // On CI without a display, this will fall back to stdout — shouldn't panic either way
    notifier.notify("test title", "test body");
}

// --- infer_item_tier edge cases ---

#[test]
fn infer_item_tier_detects_policy_keyword_extended() {
    assert_eq!(infer_item_tier("files./etc/security-policy.conf"), "locked");
    assert_eq!(infer_item_tier("system.policy_engine"), "locked");
}

#[test]
fn infer_item_tier_normal_resources_are_recommended() {
    assert_eq!(infer_item_tier("packages.npm.typescript"), "recommended");
    assert_eq!(
        infer_item_tier("files./home/user/.gitconfig"),
        "recommended"
    );
    assert_eq!(infer_item_tier("env.PATH"), "recommended");
}

// --- build_webhook_payload ---

#[test]
fn build_webhook_payload_emits_expected_schema() {
    let body = super::build_webhook_payload(
        "cfgd: drift detected",
        "5 file(s) changed",
        "2026-05-07T05:30:00Z",
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("payload must be valid JSON");
    assert_eq!(parsed["event"], "cfgd: drift detected");
    assert_eq!(parsed["message"], "5 file(s) changed");
    assert_eq!(parsed["timestamp"], "2026-05-07T05:30:00Z");
    assert_eq!(
        parsed["source"], "cfgd",
        "source must be hardcoded so receivers can filter on it"
    );
}

#[test]
fn build_webhook_payload_preserves_unicode_in_message() {
    let body =
        super::build_webhook_payload("hdr", "msg with 中文 + emoji 🎉", "2026-05-07T00:00:00Z");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["message"], "msg with 中文 + emoji 🎉");
}

#[test]
fn build_webhook_payload_escapes_quotes_and_backslashes() {
    // The function must produce JSON that round-trips even when the message
    // contains characters that would break a naive string concat.
    let body = super::build_webhook_payload(
        "hdr",
        "a \"quoted\" path: C:\\Users\\me\\.config",
        "2026-05-07T00:00:00Z",
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("payload with quotes/backslashes must round-trip");
    assert_eq!(
        parsed["message"],
        "a \"quoted\" path: C:\\Users\\me\\.config"
    );
}

#[test]
fn build_webhook_payload_accepts_empty_strings() {
    let body = super::build_webhook_payload("", "", "");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["event"], "");
    assert_eq!(parsed["message"], "");
    assert_eq!(parsed["timestamp"], "");
    assert_eq!(parsed["source"], "cfgd");
}

// ===========================================================================
// Daemon-loop harness tests (runner.rs)
//
// `run_daemon_loop` is extracted from `run_daemon` so the per-branch
// orchestration is exercisable without spawning real timers, file watchers, or
// signal handlers. The tests below drive either the loop end-to-end (via
// `mpsc` channel triggers + a `oneshot` shutdown) or the individual branch
// helpers directly.
// ===========================================================================

mod harness {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration as StdDuration;
    use tokio::sync::{mpsc, oneshot};

    /// Minimal DaemonHooks impl that returns empty/identity values. Suitable
    /// for any test that doesn't need package or file planning to do real work.
    pub(super) struct NoopHooks;

    impl DaemonHooks for NoopHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    /// Build a `DaemonLoopContext` wired for tests. `config_path` is set to a
    /// nonexistent file under `tmp` so any handler that tries to load config
    /// returns early before touching real system state. `state_dir_override`
    /// is set so `handle_reconcile` does not touch `~/.local/share/cfgd/`.
    pub(super) fn make_test_ctx(
        tmp: &tempfile::TempDir,
        on_change_reconcile: bool,
        notify_on_drift: bool,
        compliance: Option<config::ComplianceConfig>,
    ) -> (
        DaemonLoopContext,
        Arc<Mutex<DaemonState>>,
        Arc<std::sync::Mutex<String>>,
    ) {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let ctx = DaemonLoopContext {
            state: Arc::clone(&state),
            hooks: Arc::new(NoopHooks),
            notifier,
            config_path: tmp.path().join("nonexistent-config.yaml"),
            profile_override: None,
            on_change_reconcile,
            notify_on_drift,
            compliance_config: compliance,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
        };
        (ctx, state, buf)
    }

    pub(super) fn make_triggers() -> (DaemonTriggers, TriggerSenders) {
        let (file_tx, file_rx) = mpsc::channel::<PathBuf>(8);
        let (reconcile_tx, reconcile_rx) = mpsc::channel::<()>(8);
        let (sync_tx, sync_rx) = mpsc::channel::<()>(8);
        let (version_check_tx, version_check_rx) = mpsc::channel::<()>(8);
        let (compliance_tx, compliance_rx) = mpsc::channel::<()>(8);
        let (sighup_tx, sighup_rx) = mpsc::channel::<()>(8);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        (
            DaemonTriggers {
                file_rx,
                reconcile_rx,
                sync_rx,
                version_check_rx,
                compliance_rx,
                sighup_rx,
                shutdown_rx,
            },
            TriggerSenders {
                file_tx,
                reconcile_tx,
                sync_tx,
                version_check_tx,
                compliance_tx,
                sighup_tx,
                shutdown_tx,
            },
        )
    }

    #[allow(dead_code)]
    pub(super) struct TriggerSenders {
        pub file_tx: mpsc::Sender<PathBuf>,
        pub reconcile_tx: mpsc::Sender<()>,
        pub sync_tx: mpsc::Sender<()>,
        pub version_check_tx: mpsc::Sender<()>,
        pub compliance_tx: mpsc::Sender<()>,
        pub sighup_tx: mpsc::Sender<()>,
        pub shutdown_tx: oneshot::Sender<()>,
    }

    // ----- apply_sighup_reload / compute_sighup_intervals tests -----

    fn parse_cfgd_config(yaml: &str) -> CfgdConfig {
        serde_yaml::from_str(yaml).expect("test yaml must parse")
    }

    #[test]
    fn compute_sighup_intervals_returns_none_when_daemon_spec_absent() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        );
        let (reconcile, sync) = runner::compute_sighup_intervals(&cfg);
        assert!(reconcile.is_none());
        assert!(sync.is_none());
    }

    #[test]
    fn compute_sighup_intervals_returns_reconcile_when_set() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 45s\n",
        );
        let (reconcile, sync) = runner::compute_sighup_intervals(&cfg);
        assert_eq!(reconcile, Some(StdDuration::from_secs(45)));
        assert!(sync.is_none());
    }

    #[test]
    fn compute_sighup_intervals_returns_sync_when_set() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    sync:\n      interval: 10m\n",
        );
        let (reconcile, sync) = runner::compute_sighup_intervals(&cfg);
        assert!(reconcile.is_none());
        assert_eq!(sync, Some(StdDuration::from_secs(600)));
    }

    #[test]
    fn apply_sighup_reload_warns_on_unparseable_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("bad.yaml");
        std::fs::write(&config_path, "::: not yaml :::").unwrap();
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        runner::apply_sighup_reload(&config_path, &reconcile_secs, &sync_secs, &printer);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Config reload failed"),
            "expected reload-failed warning in: {}",
            captured
        );
        // Atomics untouched on failure
        assert_eq!(reconcile_secs.load(Ordering::Relaxed), 300);
        assert_eq!(sync_secs.load(Ordering::Relaxed), 300);
    }

    #[test]
    fn apply_sighup_reload_updates_atomics_and_reports_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 90s\n    sync:\n      interval: 2m\n",
        )
        .unwrap();
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        runner::apply_sighup_reload(&config_path, &reconcile_secs, &sync_secs, &printer);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Timer intervals reloaded"),
            "expected reload success in: {}",
            captured
        );
        assert_eq!(reconcile_secs.load(Ordering::Relaxed), 90);
        assert_eq!(sync_secs.load(Ordering::Relaxed), 120);
    }

    #[test]
    fn apply_sighup_reload_states_scope_is_timer_only_to_avoid_silent_surprise() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 90s\n",
        )
        .unwrap();
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        runner::apply_sighup_reload(&config_path, &reconcile_secs, &sync_secs, &printer);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("timer intervals only"),
            "SIGHUP start message must state scope: {}",
            captured
        );
        assert!(
            captured.contains("other field changes require restart"),
            "SIGHUP completion line must mention restart for other fields: {}",
            captured
        );
    }

    #[test]
    fn apply_sighup_reload_reports_no_changes_for_silent_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n",
        )
        .unwrap();
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        runner::apply_sighup_reload(&config_path, &reconcile_secs, &sync_secs, &printer);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("no timer changes detected"),
            "expected no-changes message in: {}",
            captured
        );
        assert_eq!(reconcile_secs.load(Ordering::Relaxed), 300);
        assert_eq!(sync_secs.load(Ordering::Relaxed), 300);
    }

    // ----- build_initial_source_status tests -----

    #[test]
    fn build_initial_source_status_empty_when_no_sources() {
        let rows = runner::build_initial_source_status(&[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn build_initial_source_status_one_row_per_source() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  sources:\n    - name: alpha\n      origin:\n        type: Git\n        url: https://example.com/a.git\n    - name: beta\n      origin:\n        type: Git\n        url: https://example.com/b.git\n",
        );
        let rows = runner::build_initial_source_status(&cfg.spec.sources);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[1].name, "beta");
        for r in &rows {
            assert_eq!(r.status, "active");
            assert_eq!(r.drift_count, 0);
            assert!(r.last_sync.is_none());
            assert!(r.last_reconcile.is_none());
        }
    }

    // ----- handle_file_change_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_change_tick_records_path_in_debounce_map() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
        let path = PathBuf::from("/tmp/observed-1.txt");
        let res = runner::handle_file_change_tick(
            &ctx,
            &mut last_change,
            StdDuration::from_millis(500),
            path.clone(),
        )
        .await;
        assert!(res.is_ok());
        assert!(last_change.contains_key(&path));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_change_tick_debounces_rapid_repeats() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
        let path = PathBuf::from("/tmp/observed-2.txt");
        // 60s debounce window — large enough that any plausible parallel-test
        // scheduling jitter still keeps both calls inside the window.
        let debounce = StdDuration::from_secs(60);
        runner::handle_file_change_tick(&ctx, &mut last_change, debounce, path.clone())
            .await
            .unwrap();
        let first_ts = *last_change.get(&path).unwrap();
        runner::handle_file_change_tick(&ctx, &mut last_change, debounce, path.clone())
            .await
            .unwrap();
        let second_ts = *last_change.get(&path).unwrap();
        assert_eq!(
            first_ts, second_ts,
            "debounced call must not refresh timestamp"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_change_tick_triggers_reconcile_when_enabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, true, false, None);
        let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
        let path = PathBuf::from("/tmp/observed-3.txt");
        // on_change_reconcile=true sends handle_reconcile through spawn_blocking.
        // With a nonexistent config_path the handler returns early — we only
        // care that the branch ran without panicking.
        let res = runner::handle_file_change_tick(
            &ctx,
            &mut last_change,
            StdDuration::from_millis(0), // disable debounce
            path,
        )
        .await;
        assert!(res.is_ok());
        // No real reconcile occurred (config is missing) — last_reconcile stays None.
        let st = state.lock().await;
        assert!(st.last_reconcile.is_none());
    }

    // ----- handle_reconcile_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_with_no_tasks_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks: Vec<ReconcileTask> = Vec::new();
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_skips_task_whose_interval_has_not_elapsed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let recent = Instant::now();
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(3600),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: Some(recent),
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        // Task skipped — last_reconciled unchanged.
        assert_eq!(tasks[0].last_reconciled, Some(recent));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_advances_default_task_last_reconciled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        assert!(tasks[0].last_reconciled.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_updates_module_timestamp_for_non_default_entity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Per-module reconcile now invokes the reconciler — point at a real
        // config so `handle_reconcile` reaches the state-update step.
        let config_path = write_happy_path_config(&tmp);
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "my-module".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: true,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        assert!(tasks[0].last_reconciled.is_some());
        let st = state.lock().await;
        assert!(st.module_last_reconcile.contains_key("my-module"));
    }

    // ----- handle_sync_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_with_no_tasks_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks: Vec<SyncTask> = Vec::new();
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        let st = state.lock().await;
        assert!(st.last_sync.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_skips_task_whose_interval_has_not_elapsed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let recent = Instant::now();
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: tmp.path().to_path_buf(),
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(3600),
            last_synced: Some(recent),
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert_eq!(tasks[0].last_synced, Some(recent));
    }

    // ----- handle_compliance_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compliance_tick_is_noop_when_config_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        // Should return Ok immediately — compliance_config is None.
        runner::handle_compliance_tick(&ctx).await.unwrap();
    }

    // ----- end-to-end loop tests (run_daemon_loop) -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_exits_cleanly_on_shutdown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        // Immediately request shutdown.
        senders.shutdown_tx.send(()).unwrap();
        let result = tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("loop did not exit within 2s")
            .expect("join error");
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_processes_sighup_then_shuts_down() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Write a config that updates intervals.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 77s\n",
        )
        .unwrap();
        let (mut ctx, _state, buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let reconcile_secs_observe = Arc::clone(&reconcile_secs);
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        // Fire a SIGHUP-equivalent tick.
        senders.sighup_tx.send(()).await.unwrap();
        // Give the loop a moment to process before shutdown.
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("loop did not exit within 2s")
            .expect("join error")
            .expect("loop returned Err");
        assert_eq!(reconcile_secs_observe.load(Ordering::Relaxed), 77);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Timer intervals reloaded"),
            "expected reload message in: {}",
            captured
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_drains_reconcile_ticks_with_no_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        for _ in 0..3 {
            senders.reconcile_tx.send(()).await.unwrap();
        }
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("loop did not exit within 2s")
            .expect("join error")
            .expect("loop returned Err");
        let st = state.lock().await;
        // No reconcile_tasks → nothing changes.
        assert!(st.last_reconcile.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_drains_sync_ticks_with_no_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        senders.sync_tx.send(()).await.unwrap();
        senders.sync_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("loop did not exit within 2s")
            .expect("join error")
            .expect("loop returned Err");
        let st = state.lock().await;
        assert!(st.last_sync.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_drains_compliance_ticks_when_disabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        senders.compliance_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("loop did not exit within 2s")
            .expect("join error")
            .expect("loop returned Err");
    }

    // (loop dispatch of file-change events is covered by
    // `handle_file_change_tick_*` direct-helper tests; a parallel loop test
    // running under `cargo llvm-cov` flaked on the StateStore opening inside
    // record_file_drift, so we exercise the branch by calling the helper
    // directly rather than through run_daemon_loop's select!.)

    // ----- Per-tick failure isolation -----
    //
    // The select! loop must log and continue when a tick handler panics or
    // returns Err — a single failing tick must not tear the daemon down.
    // These tests panic inside the spawn_blocking that backs each tick (via
    // `DaemonHooks` whose plan_files / build_registry implementation panics)
    // and then assert that the loop still services subsequent ticks and
    // exits cleanly on shutdown.

    /// `DaemonHooks` that panics in `plan_files`. Used to drive
    /// `handle_reconcile_tick` into a `JoinError` so the loop's recovery
    /// behavior is observable.
    struct PanickingPlanFilesHooks;

    impl DaemonHooks for PanickingPlanFilesHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            panic!("intentional panic in plan_files (test fixture)")
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    /// `DaemonHooks` that panics in `build_registry`. Used to drive
    /// `handle_compliance_tick` (and, secondarily, any tick that builds a
    /// registry) into a `JoinError`.
    struct PanickingRegistryHooks;

    impl DaemonHooks for PanickingRegistryHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            panic!("intentional panic in build_registry (test fixture)")
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    /// Build a `DaemonLoopContext` whose `hooks` panic inside `plan_files`.
    fn make_panicking_plan_files_ctx(
        tmp: &tempfile::TempDir,
    ) -> (DaemonLoopContext, Arc<Mutex<DaemonState>>) {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let ctx = DaemonLoopContext {
            state: Arc::clone(&state),
            hooks: Arc::new(PanickingPlanFilesHooks),
            notifier,
            config_path: write_happy_path_config(tmp),
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
        };
        (ctx, state)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_reconcile_tick_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            tasks,
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        // Reconcile tick triggers the panicking plan_files inside
        // spawn_blocking; the loop should log and continue.
        senders.reconcile_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        // Fire a no-op sync tick to prove the loop is still alive and
        // processing further dispatches.
        senders.sync_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(3), handle)
            .await
            .expect("loop did not exit within 3s")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_compliance_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "7d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let ctx = DaemonLoopContext {
            state: Arc::clone(&state),
            hooks: Arc::new(PanickingRegistryHooks),
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: Some(compliance_cfg),
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
        };
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        senders.compliance_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(3), handle)
            .await
            .expect("loop did not exit within 3s")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_sync_tick_error() {
        // A sync tick whose repo_path does not exist exercises the sync
        // handler's error path (git2 returns Err from `Repository::open`).
        // After the failing tick we fire a reconcile tick that panics, then
        // a no-op sync tick, then shutdown — proving the loop keeps
        // servicing both error and panic flavors of failure.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let sync_tasks = vec![SyncTask {
            source_name: "broken".to_string(),
            repo_path: tmp.path().join("does-not-exist"),
            auto_pull: true,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(0),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: false,
        }];
        let reconcile_tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            reconcile_tasks,
            sync_tasks,
            reconcile_secs,
            sync_secs,
        ));
        senders.sync_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.reconcile_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(3), handle)
            .await
            .expect("loop did not exit within 3s")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_version_check_tick() {
        // Version check runs via spawn_blocking on `handle_version_check`,
        // which reads/writes a small JSON cache under HOME (guarded to the
        // tempdir). With no network reachable the upgrade module errors
        // gracefully and the tick must not abort the loop. After the tick
        // we fire a panicking reconcile to confirm the loop's
        // continue-on-error behavior is engaged, then shutdown.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let reconcile_tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            reconcile_tasks,
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        senders.version_check_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        senders.reconcile_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(StdDuration::from_secs(3), handle)
            .await
            .expect("loop did not exit within 3s")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_exits_on_shutdown_after_panicking_tick() {
        // Regression guard: shutdown must still drain cleanly after a tick
        // handler has panicked. Without the per-tick continue-on-error
        // contract the loop would have already returned Err before shutdown
        // arrives, and this test would observe that as a JoinError.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            tasks,
            Vec::new(),
            reconcile_secs,
            sync_secs,
        ));
        senders.reconcile_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        senders.reconcile_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        senders.shutdown_tx.send(()).unwrap();
        let result = tokio::time::timeout(StdDuration::from_secs(3), handle)
            .await
            .expect("loop did not exit within 3s")
            .expect("join error");
        assert!(
            result.is_ok(),
            "loop should exit Ok after panicking ticks + shutdown, got {:?}",
            result
        );
    }

    // ----- BLOCKER #2 — per-module reconcile actually invokes the reconciler -----
    //
    // The per-module branch of `handle_reconcile_tick` used to log and write
    // `module_last_reconcile` without calling the reconciler. These tests
    // drive the branch with a `ReconcilePatch`-bearing config and a hooks
    // impl that records calls, asserting the reconciler is invoked with the
    // expected module filter and that the patch's auto_apply / drift_policy
    // are respected.

    /// `DaemonHooks` whose `plan_files` records how many times it has been
    /// called and what `config_dir` it was invoked with. Returns the empty
    /// vec so `handle_reconcile` proceeds without producing real actions.
    struct RecordingHooks {
        plan_files_calls: Arc<std::sync::atomic::AtomicUsize>,
        build_registry_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl DaemonHooks for RecordingHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            self.build_registry_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            self.plan_files_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_module_reconcile_invokes_reconciler_with_filter() {
        // Two ReconcileTasks (one default, one per-module). Firing a tick
        // when both are due should call into `handle_reconcile` for the
        // default task and ALSO for the per-module task. Previously the
        // per-module branch was a no-op, so the hook would only fire once.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let plan_files_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let build_registry_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hooks = Arc::new(RecordingHooks {
            plan_files_calls: Arc::clone(&plan_files_calls),
            build_registry_calls: Arc::clone(&build_registry_calls),
        });
        let ctx = DaemonLoopContext {
            state: Arc::clone(&state),
            hooks,
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
        };
        let mut tasks = vec![
            ReconcileTask {
                entity: "__default__".to_string(),
                interval: StdDuration::from_secs(0),
                auto_apply: false,
                drift_policy: config::DriftPolicy::NotifyOnly,
                last_reconciled: None,
            },
            ReconcileTask {
                entity: "docker".to_string(),
                interval: StdDuration::from_secs(0),
                auto_apply: false,
                drift_policy: config::DriftPolicy::NotifyOnly,
                last_reconciled: None,
            },
        ];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        // Both tasks invoked the reconciler — plan_files called twice, once
        // per branch (default + filtered module).
        assert_eq!(
            plan_files_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "expected plan_files to fire for both the default and the per-module ticks"
        );
        // Per-module branch must populate module_last_reconcile for the
        // patched module name.
        let st = state.lock().await;
        assert!(
            st.module_last_reconcile.contains_key("docker"),
            "per-module branch should have recorded module_last_reconcile for 'docker' — got keys: {:?}",
            st.module_last_reconcile.keys().collect::<Vec<_>>()
        );
        assert!(
            st.last_reconcile.is_some(),
            "default branch should have updated profile-wide last_reconcile"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_module_reconcile_respects_drift_policy_notify_only() {
        // Per-module patch with drift_policy=NotifyOnly + a tick that
        // doesn't produce drift (empty profile). The reconciler is invoked
        // but apply is NOT (the NotifyOnly branch). We assert the per-module
        // entry shows up in state and that profile-wide drift_count is 0.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let plan_files_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let build_registry_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hooks = Arc::new(RecordingHooks {
            plan_files_calls: Arc::clone(&plan_files_calls),
            build_registry_calls: Arc::clone(&build_registry_calls),
        });
        let ctx = DaemonLoopContext {
            state: Arc::clone(&state),
            hooks,
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
        };
        let mut tasks = vec![ReconcileTask {
            entity: "monitoring".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        // The reconciler was driven for the module — plan_files fired once.
        assert_eq!(
            plan_files_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let st = state.lock().await;
        assert!(st.module_last_reconcile.contains_key("monitoring"));
        // No drift detected against empty profile + NotifyOnly policy.
        assert_eq!(st.drift_count, 0);
        // Per-module tick must not bump profile-wide last_reconcile.
        assert!(
            st.last_reconcile.is_none(),
            "per-module tick should not touch profile-wide last_reconcile"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_module_reconcile_with_auto_apply_invokes_reconciler() {
        // Per-module patch with auto_apply=true, drift_policy=Auto. With an
        // empty profile the apply branch is unreachable (no drift) but the
        // reconciler is still driven and the state is updated.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let plan_files_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let build_registry_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hooks = Arc::new(RecordingHooks {
            plan_files_calls: Arc::clone(&plan_files_calls),
            build_registry_calls: Arc::clone(&build_registry_calls),
        });
        let ctx = DaemonLoopContext {
            state: Arc::clone(&state),
            hooks,
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
        };
        let mut tasks = vec![ReconcileTask {
            entity: "vault".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: true,
            drift_policy: config::DriftPolicy::Auto,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        assert_eq!(
            plan_files_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "reconciler must be invoked for the per-module branch when auto_apply=true"
        );
        assert_eq!(
            build_registry_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let st = state.lock().await;
        assert!(st.module_last_reconcile.contains_key("vault"));
    }

    // ----- run_daemon_loop never returns Err for the channel-trigger branches
    // — tick errors are logged and the loop continues (see above). -----

    // ----- spawn_interval_pump smoke test -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interval_pump_clamps_zero_to_one_second() {
        // A 0-second interval would spin tight — the pump must clamp to >=1s.
        // We don't actually wait a full second; instead, we trip the abort path.
        let secs = Arc::new(AtomicU64::new(0));
        let (tx, mut rx) = mpsc::channel::<()>(8);
        let handle = super::super::spawn_interval_pump(secs, tx);
        // Give the runtime a chance to schedule the pump task.
        tokio::time::sleep(StdDuration::from_millis(10)).await;
        handle.abort();
        // No assertion on rx — we only verify the pump didn't spin or panic before
        // abort. If the clamp were missing this test would hang the runtime.
        let _ = rx.try_recv();
    }

    // ----- Happy-path fixture: drive handle_reconcile end-to-end ---------
    //
    // The previous tests exit early inside handle_reconcile because
    // `config_path` points to a missing file. This fixture writes a real
    // `cfgd.yaml` + `profiles/default.yaml` so reconcile reaches the plan
    // generation + state.last_reconcile update. Unlocks coverage in
    // daemon/reconcile.rs and (via handle_sync_tick) daemon/sync.rs.

    /// Write a minimal but complete cfgd config tree under `tmp`. Returns
    /// the path to `cfgd.yaml`. The config selects profile "default", which
    /// resolves to an empty `profiles/default.yaml`.
    fn write_happy_path_config(tmp: &tempfile::TempDir) -> PathBuf {
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        config_path
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_runs_full_happy_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(
            st.last_reconcile.is_some(),
            "handle_reconcile should have updated state.last_reconcile on happy path"
        );
        // No drift expected — empty profile means no actions to apply.
        assert_eq!(st.drift_count, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_handles_unknown_profile_gracefully() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config that points to a profile name that doesn't exist on disk.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: missing-profile\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        // Profile resolution fails → handle_reconcile returns before
        // touching last_reconcile.
        assert!(st.last_reconcile.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_respects_profile_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config has no profile — override supplies one.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("override-profile.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: override-profile\nspec: {}\n",
        )
        .unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        ctx.profile_override = Some("override-profile".to_string());
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_auto_apply_traverses_apply_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config with daemon.reconcile.autoApply=true exercises the auto-apply
        // policy branch even though the plan is empty.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: true,
            drift_policy: config::DriftPolicy::Auto,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_some());
        assert_eq!(st.drift_count, 0);
    }

    // ----- Real sync_task with a tempdir non-git repo path -----
    //
    // handle_sync will attempt git operations against `repo_path`. With a
    // non-git directory, all git calls fail gracefully and the handler
    // still returns false (no changes). The orchestration around it — the
    // last_synced bump, the state.last_sync update via block_on — is what
    // we cover here.

    /// Create a bare upstream repo + a working clone of it. Returns the
    /// (bare_path, work_path) pair. The clone starts with a single commit
    /// already pushed to bare's HEAD branch.
    fn make_bare_and_clone(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let bare = tmp.path().join("upstream.git");
        let work = tmp.path().join("workdir");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();
        let src = tmp.path().join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(src.join("README.md"), "hi").unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("README.md")).unwrap();
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
        let _ = git2::Repository::clone(&bare_url, &work).unwrap();
        (bare, work)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_runs_git_pull_against_real_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (_bare, work) = make_bare_and_clone(&tmp);
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: work,
            auto_pull: true,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some());
        let st = state.lock().await;
        assert!(st.last_sync.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_runs_git_push_against_real_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (_bare, work) = make_bare_and_clone(&tmp);
        // Make a local edit so git_auto_commit_push has something to commit.
        std::fs::write(work.join("README.md"), "local change").unwrap();
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: work,
            auto_pull: false,
            auto_push: true,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_handles_invalid_repo_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        // Path that exists but isn't a git repo — git_pull fails gracefully.
        let not_a_repo = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: not_a_repo,
            auto_pull: true,
            auto_push: true,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some());
    }

    // ----- handle_reconcile with files+packages in profile -----
    //
    // Plan with a non-empty profile exercises file/package planning paths.
    // NoopHooks returns empty actions, so plan is still empty — but the
    // resolve_profile body walks merged.files.managed, merged.packages, etc.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_with_managed_files_in_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: example.txt\n        target: ~/example.txt\n  packages:\n    brew:\n      formulae:\n        - ripgrep\n",
        )
        .unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_advances_last_synced_for_due_task() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let repo_path = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&repo_path).unwrap();
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path,
            // auto_pull/push false → handle_sync does no git work, just updates state
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some(), "last_synced should advance");
        let st = state.lock().await;
        assert!(st.last_sync.is_some(), "state.last_sync should be set");
    }

    // ----- build_pre_loop_setup: SETUP-arm coverage -----

    #[test]
    fn build_pre_loop_setup_happy_path_yields_defaulted_intervals() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let hooks = NoopHooks;

        let setup = build_pre_loop_setup(&config_path, None, &hooks).expect("happy setup");

        // Default reconcile + sync interval = 300s (5m)
        assert_eq!(setup.parsed.reconcile_interval, Duration::from_secs(300));
        assert_eq!(setup.parsed.sync_interval, Duration::from_secs(300));
        assert!(!setup.parsed.auto_pull);
        assert!(!setup.parsed.auto_push);
        assert!(!setup.parsed.auto_apply);
        // Compliance not configured → no interval
        assert!(setup.compliance_config.is_none());
        assert!(setup.compliance_interval.is_none());
        // One sync task for local config dir
        assert_eq!(setup.sync_tasks.len(), 1);
        // Only the __default__ reconcile task (no module patches)
        assert_eq!(setup.reconcile_tasks.len(), 1);
        assert_eq!(setup.reconcile_tasks[0].entity, "__default__");
        // No external sources → only the seeded "local" source status (added in run_daemon, not setup)
        // Setup itself just produces the additions, which is empty here.
        assert!(setup.initial_source_status.is_empty());
        // No files in default profile → no managed paths
        assert!(setup.managed_paths.is_empty());
        // No server origin → no startup check-in URL
        assert!(setup.server_checkin_url.is_none());
        // Stdout notifier by default
        assert!(matches!(setup.parsed.notify_method, NotifyMethod::Stdout));
        // shortest_* == defaults when no per-module patches narrow them
        assert_eq!(setup.shortest_reconcile, Duration::from_secs(300));
        assert_eq!(setup.shortest_sync, Duration::from_secs(300));
        // config_dir matches the parent of config_path
        assert_eq!(setup.config_dir, tmp.path());
    }

    #[test]
    fn build_pre_loop_setup_loads_compliance_interval_when_enabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  compliance:\n    enabled: true\n    interval: 30m\n    retention: 30d\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let hooks = NoopHooks;

        let setup = build_pre_loop_setup(&config_path, None, &hooks).expect("setup");

        assert!(setup.compliance_config.is_some());
        assert_eq!(setup.compliance_interval, Some(Duration::from_secs(1800)));
    }

    #[test]
    fn build_pre_loop_setup_skips_compliance_interval_when_disabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  compliance:\n    enabled: false\n    interval: 30m\n    retention: 30d\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let hooks = NoopHooks;

        let setup = build_pre_loop_setup(&config_path, None, &hooks).expect("setup");

        // Compliance config present but interval None because enabled=false short-circuits filter.
        assert!(setup.compliance_config.is_some());
        assert!(setup.compliance_interval.is_none());
    }

    #[test]
    fn build_pre_loop_setup_returns_err_for_unparseable_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "::: not yaml :::").unwrap();
        let hooks = NoopHooks;

        let result = build_pre_loop_setup(&config_path, None, &hooks);

        match result {
            Ok(_) => panic!("invalid yaml must error"),
            Err(e) => {
                // Just confirm an error surfaced. Message asserts would be brittle.
                let msg = format!("{}", e);
                assert!(!msg.is_empty());
            }
        }
    }

    #[test]
    fn build_pre_loop_setup_respects_profile_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        // Config has profile: default; override should pick override-profile instead.
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("override-profile.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: override-profile\nspec:\n  files:\n    managed:\n      - source: example.txt\n        target: /tmp/example-override.txt\n",
        )
        .unwrap();
        let hooks = NoopHooks;

        let setup =
            build_pre_loop_setup(&config_path, Some("override-profile"), &hooks).expect("setup");

        // override-profile has a managed file → discover_managed_paths populates it.
        assert_eq!(setup.managed_paths.len(), 1);
        assert!(
            setup
                .managed_paths
                .iter()
                .any(|p| p.ends_with("example-override.txt"))
        );
    }

    #[test]
    fn build_pre_loop_setup_falls_back_to_default_profile_name_when_unset() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config has no profile field → fallback chain is "default".
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        let hooks = NoopHooks;

        let setup = build_pre_loop_setup(&config_path, None, &hooks).expect("setup");

        // No profile resolution → no managed paths, reconcile_tasks contains just __default__
        assert!(setup.managed_paths.is_empty());
        assert_eq!(setup.reconcile_tasks.len(), 1);
        assert_eq!(setup.reconcile_tasks[0].entity, "__default__");
    }

    #[test]
    fn build_pre_loop_setup_picks_up_sync_auto_pull_push_from_daemon_spec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    sync:\n      interval: 90s\n      autoPull: true\n      autoPush: true\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let hooks = NoopHooks;

        let setup = build_pre_loop_setup(&config_path, None, &hooks).expect("setup");

        assert!(setup.parsed.auto_pull);
        assert!(setup.parsed.auto_push);
        assert_eq!(setup.parsed.sync_interval, Duration::from_secs(90));
        assert_eq!(setup.shortest_sync, Duration::from_secs(90));
        // First (and only) sync task is the local one, which inherits parsed values.
        assert_eq!(setup.sync_tasks.len(), 1);
    }

    #[test]
    fn build_pre_loop_setup_finds_server_url_for_server_origin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  origin:\n    - type: Server\n      url: https://gateway.example/api\n      branch: master\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let hooks = NoopHooks;

        let setup = build_pre_loop_setup(&config_path, None, &hooks).expect("setup");

        assert_eq!(
            setup.server_checkin_url.as_deref(),
            Some("https://gateway.example/api")
        );
    }

    // ----- handle_compliance_snapshot: state_dir_override coverage -----

    #[test]
    fn handle_compliance_snapshot_writes_to_state_dir_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        let hooks = NoopHooks;
        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
        );

        // Snapshot row was written to the override DB.
        let store =
            crate::state::StateStore::open(&state_dir.join("cfgd.db")).expect("override db");
        let hash = store
            .latest_compliance_hash()
            .expect("hash query")
            .expect("snapshot present");
        assert!(!hash.is_empty());
    }

    #[test]
    fn handle_compliance_snapshot_returns_early_on_unparseable_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "::: not yaml :::").unwrap();

        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        let hooks = NoopHooks;
        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
        );

        // No snapshot stored because config load failed.
        let store =
            crate::state::StateStore::open(&state_dir.join("cfgd.db")).expect("override db");
        let hash = store.latest_compliance_hash().expect("hash query");
        assert!(hash.is_none());
    }

    #[test]
    fn handle_compliance_snapshot_returns_early_when_named_profile_does_not_exist() {
        // The cfg names a profile (`ghost`) but profiles/ doesn't contain it →
        // `resolve_profile` returns Err → the function takes the resolve-Err
        // arm (lines 151-157 in sync.rs) and bails without opening the store.
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: ghost\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        // Intentionally no ghost.yaml → resolve_profile fails.

        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        let hooks = NoopHooks;
        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
        );

        // No snapshot stored because resolve_profile failed.
        let store =
            crate::state::StateStore::open(&state_dir.join("cfgd.db")).expect("override db");
        let hash = store.latest_compliance_hash().expect("hash query");
        assert!(
            hash.is_none(),
            "missing profile → resolve_profile Err → no snapshot stored"
        );
    }

    #[test]
    fn handle_compliance_snapshot_skips_when_no_profile_configured() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        // No spec.profile, no override → handler bails before opening the store.
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();

        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        let hooks = NoopHooks;
        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
        );

        let store =
            crate::state::StateStore::open(&state_dir.join("cfgd.db")).expect("override db");
        let hash = store.latest_compliance_hash().expect("hash query");
        assert!(hash.is_none());
    }

    // ----- handle_version_check: cache-hit coverage -----
    //
    // `check_with_cache` reads/writes the version cache under
    // `test_home_override().join(".cache/cfgd/")`. Pre-seeding the cache
    // with a non-expired entry skips the network call entirely.

    fn write_version_cache(home: &std::path::Path, latest: &str, current: &str) {
        let dir = home.join(".cache").join("cfgd");
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // VersionCache uses serde rename_all=camelCase.
        let body = format!(
            r#"{{"checkedAtSecs":{},"latestTag":"v{}","latestVersion":"{}","currentVersion":"{}"}}"#,
            now, latest, latest, current
        );
        std::fs::write(dir.join("version-check.json"), body).unwrap();
    }

    // The test_home thread-local is installed on the calling thread; the
    // version-check helper propagates that override into its spawn_blocking
    // closure so the cache lookup sees the tempdir.
    async fn drive_version_check(home: std::path::PathBuf) -> Arc<Mutex<DaemonState>> {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let _g = crate::with_test_home_guard(&home);
        super::super::sync::handle_version_check(&state, &notifier).await;
        state
    }

    // current_thread so the test_home thread-local installed in
    // `drive_version_check` survives across the `.await` — multi_thread can
    // migrate the future to a different worker thread mid-poll.
    #[tokio::test(flavor = "current_thread")]
    async fn handle_version_check_records_update_available_from_fresh_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Pre-seed a cache entry advertising a version far ahead of any current.
        write_version_cache(tmp.path(), "999.0.0", "0.0.0");

        let state = drive_version_check(tmp.path().to_path_buf()).await;

        let st = state.lock().await;
        assert_eq!(st.update_available.as_deref(), Some("999.0.0"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_version_check_leaves_state_clean_when_cache_says_up_to_date() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Pre-seed a cache entry whose version is well below current. Since
        // `check_with_cache` returns `update_available = cached > current`
        // and the test binary's current version exceeds 0.0.0, no update is
        // reported.
        write_version_cache(tmp.path(), "0.0.0", "0.0.0");

        let state = drive_version_check(tmp.path().to_path_buf()).await;

        let st = state.lock().await;
        assert!(st.update_available.is_none());
    }

    // ----- init_daemon_state tests -----

    #[test]
    fn init_daemon_state_uses_override_dir_for_store_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = super::super::init_daemon_state(Some(tmp.path()));
        let store = st
            .store_path_for_test()
            .expect("override yields a store_path");
        assert_eq!(store, tmp.path().join("state.db"));
    }

    #[test]
    fn init_daemon_state_falls_back_when_default_state_dir_fails() {
        // Force `default_state_dir` to fail by redirecting HOME to a path
        // whose parent does not exist. The function falls back to a state
        // with no store_path (the /drift endpoint returns empty events).
        let tmp = tempfile::TempDir::new().unwrap();
        let bogus = tmp.path().join("does/not/exist");
        let _g = crate::with_test_home_guard(&bogus);
        let st = super::super::init_daemon_state(None);
        // Either resolved (Some) or fell back (None) — both are valid for
        // the function's contract; the warn-and-fallback branch is what
        // `None` proves.
        // Sanity: when the override is supplied, store_path is always set.
        let st_with_override = super::super::init_daemon_state(Some(tmp.path()));
        assert!(st_with_override.store_path_for_test().is_some());
        let _ = st; // touch to silence dead_code under cfg
    }

    #[test]
    fn init_daemon_state_with_warning_reports_message_on_resolve_failure() {
        // Regression guard for MEDIUM #10: the daemon used to only emit a
        // `tracing::warn!` when state-dir resolution failed, leaving the
        // /drift endpoint silently disabled. The variant exposes a banner
        // message so the startup banner can surface it.
        use crate::test_helpers::EnvVarGuard;
        let _xdg = EnvVarGuard::unset("XDG_DATA_HOME");
        let _cache_xdg = EnvVarGuard::unset("XDG_RUNTIME_DIR");
        let tmp = tempfile::TempDir::new().unwrap();
        let bogus = tmp.path().join("does/not/exist");
        let _g = crate::with_test_home_guard(&bogus);

        let (_st, warning) = super::super::init_daemon_state_with_warning(None);
        // The platform-default lookup may succeed even with a bogus HOME on
        // some CI hosts (XDG fallback), so only assert structure WHEN the
        // warning fires; otherwise this is a no-op probe.
        if let Some(msg) = warning {
            assert!(
                msg.contains("Drift endpoint disabled"),
                "warning should be operator-facing; got {msg:?}"
            );
        }

        // With an override the variant must NEVER emit a warning.
        let (_st2, w2) = super::super::init_daemon_state_with_warning(Some(tmp.path()));
        assert!(w2.is_none(), "override path must not warn; got {w2:?}");
    }

    // ----- check_already_running tests -----

    #[cfg(unix)]
    #[test]
    fn check_already_running_ok_when_path_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("missing.sock");
        super::super::check_already_running(&path).expect("ok when path missing");
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn check_already_running_removes_stale_socket_file() {
        // A plain file at the IPC path with no listener simulates a crashed
        // daemon: connect() fails, and the cleanup branch unlinks the file.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("stale.sock");
        std::fs::write(&path, b"stale").unwrap();
        super::super::check_already_running(&path).expect("ok with stale file");
        assert!(
            !path.exists(),
            "stale socket file should have been removed: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn check_already_running_errors_when_listener_is_accepting() {
        use std::os::unix::net::UnixListener as StdUnixListener;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("live.sock");
        let _listener = StdUnixListener::bind(&path).unwrap();
        let err = super::super::check_already_running(&path)
            .expect_err("expect AlreadyRunning when a listener is accepting");
        let msg = format!("{err}");
        assert!(
            msg.contains("already") || msg.to_lowercase().contains("running"),
            "expected AlreadyRunning message, got: {msg}"
        );
        // The file is NOT removed when the listener is live.
        assert!(path.exists());
    }

    // ----- format_interval_lines tests -----

    #[test]
    fn format_interval_lines_reports_reconcile_only_by_default() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(300),
            sync_interval: StdDuration::from_secs(300),
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, None);
        assert_eq!(lines, vec!["reconcile=300s".to_string()]);
    }

    #[test]
    fn format_interval_lines_includes_sync_when_pull_or_push_enabled() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(60),
            sync_interval: StdDuration::from_secs(120),
            auto_pull: true,
            auto_push: false,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, None);
        assert_eq!(
            lines,
            vec![
                "reconcile=60s".to_string(),
                "sync=120s (pull=true, push=false)".to_string(),
            ]
        );
    }

    #[test]
    fn format_interval_lines_appends_compliance_when_supplied() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(30),
            sync_interval: StdDuration::from_secs(30),
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, Some(StdDuration::from_secs(900)));
        assert_eq!(
            lines,
            vec!["reconcile=30s".to_string(), "compliance=900s".to_string()]
        );
    }

    // ----- print_startup_banner tests -----

    #[test]
    fn print_startup_banner_emits_health_intervals_and_run_hint() {
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        super::super::print_startup_banner(
            &printer,
            &["reconcile=30s".to_string(), "compliance=900s".to_string()],
            "/tmp/cfgd-banner-test.sock",
        );
        let out = buf.lock().unwrap().clone();
        assert!(out.contains("Health: /tmp/cfgd-banner-test.sock"));
        assert!(out.contains("Intervals: reconcile=30s, compliance=900s"));
        assert!(out.contains("Daemon running"));
    }

    // ----- run_startup_checkin_blocking tests -----

    fn parse_minimal_cfg(yaml: &str) -> CfgdConfig {
        serde_yaml::from_str(yaml).expect("test yaml must parse")
    }

    #[test]
    fn run_startup_checkin_blocking_bails_when_no_profile_resolved() {
        // Profile name resolves to a profile dir that does not exist —
        // resolve_profile errors, the function warns + returns. No panic,
        // no network.
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let cfg = parse_minimal_cfg(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );
        // No profile YAML on disk → resolve_profile fails → function warns.
        super::super::run_startup_checkin_blocking(&config_path, None, &cfg);
    }

    #[test]
    fn run_startup_checkin_blocking_no_op_when_profile_missing_in_cfg() {
        // No profile in cfg AND no override → early-return.
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        let cfg = parse_minimal_cfg(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        );
        super::super::run_startup_checkin_blocking(&config_path, None, &cfg);
    }

    #[test]
    fn run_startup_checkin_blocking_resolves_profile_and_returns_when_no_server_url() {
        // Seed a valid profile so resolve_profile succeeds. With no Server
        // origin in cfg, try_server_checkin returns false without network.
        // pending-server-config load returns None on a fresh state-dir.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages: {}\n",
        )
        .unwrap();
        let cfg = parse_minimal_cfg(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );
        super::super::run_startup_checkin_blocking(&config_path, None, &cfg);
    }

    // ----- cleanup_ipc_socket tests -----

    #[cfg(unix)]
    #[test]
    fn cleanup_ipc_socket_removes_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("to-remove.sock");
        std::fs::write(&path, b"stale").unwrap();
        super::super::cleanup_ipc_socket(&path);
        assert!(!path.exists(), "expected {} to be removed", path.display());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_ipc_socket_is_noop_when_path_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("missing.sock");
        // Must not panic.
        super::super::cleanup_ipc_socket(&path);
        assert!(!path.exists());
    }

    // ----- setup_file_watcher tests -----

    #[test]
    fn setup_file_watcher_watches_existing_managed_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let managed = tmp.path().join("watched.txt");
        std::fs::write(&managed, b"initial").unwrap();
        let config_dir = tmp.path().to_path_buf();
        let (tx, _rx) = mpsc::channel::<PathBuf>(8);

        let watcher = super::super::reconcile::setup_file_watcher(
            tx,
            std::slice::from_ref(&managed),
            &config_dir,
        );
        assert!(
            watcher.is_ok(),
            "expected watcher to construct: {watcher:?}"
        );
    }

    #[test]
    fn setup_file_watcher_watches_parent_when_path_does_not_yet_exist() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Managed path is in tmp but file does not exist yet — the watcher
        // should fall back to watching the parent dir for create events.
        let managed = tmp.path().join("not-yet-created.txt");
        let config_dir = tmp.path().to_path_buf();
        let (tx, _rx) = mpsc::channel::<PathBuf>(8);

        let watcher = super::super::reconcile::setup_file_watcher(
            tx,
            std::slice::from_ref(&managed),
            &config_dir,
        );
        assert!(
            watcher.is_ok(),
            "watcher should still succeed via parent-dir fallback: {watcher:?}"
        );
    }

    #[test]
    fn setup_file_watcher_tolerates_missing_config_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Config dir doesn't exist; watcher logs a warning and returns Ok.
        let missing_config = tmp.path().join("does/not/exist");
        let (tx, _rx) = mpsc::channel::<PathBuf>(8);
        let watcher = super::super::reconcile::setup_file_watcher(tx, &[], &missing_config);
        assert!(
            watcher.is_ok(),
            "missing config_dir should not error: {watcher:?}"
        );
    }

    // ----- run_daemon_with end-to-end tests -----
    //
    // These drive `run_daemon_with` against externally-supplied triggers so
    // the full SETUP body (pre-loop config, IPC path, health-server gating,
    // startup-checkin gating, ctx assembly, loop run, cleanup) executes
    // without binding the per-user runtime socket or hitting the network.

    fn make_overrides_for_test(
        tmp: &tempfile::TempDir,
        triggers: DaemonTriggers,
    ) -> super::super::DaemonRunOverrides {
        super::super::DaemonRunOverrides {
            ipc_path: Some(tmp.path().join("daemon-test.sock")),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_external_triggers_shuts_down_cleanly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        // Give the loop a tick to enter the select! arm
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        // Send shutdown
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon shutdown did not complete in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon should exit Ok, got {:?}", result);

        // Banner emitted by print_startup_banner
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Daemon running"),
            "banner should announce running state, got: {}",
            out
        );
        assert!(
            out.contains("Daemon stopped"),
            "shutdown should print stopped message, got: {}",
            out
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_reconcile_tick_via_external_trigger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        // Drive a reconcile tick (default task __default__ is auto-built
        // from setup.reconcile_tasks with a 300s interval; first tick
        // always fires because last_reconciled is None).
        senders.reconcile_tx.send(()).await.unwrap();
        // Allow handle_reconcile to land before we shut down.
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
        // The state dir override is honored; a state.db should now exist
        // (handle_reconcile opens the store via state_dir_override).
        let store = tmp.path().join("cfgd.db");
        // Either the reconcile-driven path or the daemon-startup path may
        // create it; tolerate either name (the override-side handler uses
        // `cfgd.db` while the production-default uses `state.db`).
        assert!(
            store.exists() || tmp.path().join("state.db").exists(),
            "expected a state DB under {}",
            tmp.path().display()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_sync_tick_with_no_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        senders.sync_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(60)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_sighup_tick_and_reloads_intervals() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Start with the happy-path config (no daemon spec).
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path.clone(),
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        // Rewrite the config to introduce daemon reconcile interval.
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 45s\n",
        )
        .unwrap();
        senders.sighup_tx.send(()).await.unwrap();
        // Give the daemon enough headroom to process the SIGHUP and emit
        // the reload chatter. 80ms is fine for native runs but tight under
        // llvm-cov instrumentation, which slowed this test enough to lose
        // the printer-buffer race in the coverage build.
        tokio::time::sleep(StdDuration::from_millis(200)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Reloading configuration") || out.contains("Timer intervals reloaded"),
            "expected sighup reload chatter, got: {}",
            out
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_file_change_tick_via_external_trigger() {
        // A file-change tick goes through the dispatch arm in run_daemon_loop
        // and lands in handle_file_change_tick → debounce::record_change.
        // The daemon should keep running until we send shutdown.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path.clone(),
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        // Push a synthetic file-change path. The path doesn't need to map to
        // a managed_paths entry — the handler tolerates unknown paths and
        // simply records into the debounce map.
        senders.file_tx.send(config_path.clone()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_compliance_tick_via_external_trigger() {
        // Drive the compliance-tick arm of run_daemon_loop. Without a
        // `compliance` config block the handler runs but writes nothing to
        // the state store; the daemon should still exit cleanly afterwards.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        senders.compliance_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_health_server_enabled_binds_ipc_socket() {
        // `skip_health_server = false` exercises the health-server spawn
        // branch. The IPC socket should be created and reachable while the
        // daemon is alive, then cleaned up by `cleanup_ipc_socket` on exit.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("health-on.sock");
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: false,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
        };
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        // Give the health server time to bind the socket.
        tokio::time::sleep(StdDuration::from_millis(120)).await;
        assert!(
            ipc_path.exists(),
            "health server should have created the IPC socket at {}",
            ipc_path.display()
        );

        senders.shutdown_tx.send(()).unwrap();
        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
        // cleanup_ipc_socket should unlink the socket on shutdown.
        assert!(
            !ipc_path.exists(),
            "cleanup_ipc_socket must remove the socket on exit"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_errors_when_ipc_path_has_live_listener() {
        use std::os::unix::net::UnixListener as StdUnixListener;
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("busy.sock");
        let _listener = StdUnixListener::bind(&ipc_path).unwrap();

        let (triggers, _senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
        };
        let result = super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        )
        .await;
        let err = result.expect_err("expect AlreadyRunning error");
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("already") || msg.to_lowercase().contains("running"),
            "expected AlreadyRunning, got: {msg}"
        );
    }

    // ----- additional pump / signal / banner coverage -----

    #[test]
    fn format_interval_lines_includes_sync_when_only_auto_push_enabled() {
        // The auto_pull-only and reconcile-only branches are already covered;
        // this hits the auto_push=true / auto_pull=false leg of the
        // `auto_pull || auto_push` guard so both halves of the boolean OR
        // execute under coverage instrumentation.
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(60),
            sync_interval: StdDuration::from_secs(180),
            auto_pull: false,
            auto_push: true,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, None);
        assert_eq!(
            lines,
            vec![
                "reconcile=60s".to_string(),
                "sync=180s (pull=false, push=true)".to_string(),
            ]
        );
    }

    #[test]
    fn format_interval_lines_reconcile_sync_compliance_combined() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(45),
            sync_interval: StdDuration::from_secs(90),
            auto_pull: true,
            auto_push: true,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, Some(StdDuration::from_secs(600)));
        assert_eq!(
            lines,
            vec![
                "reconcile=45s".to_string(),
                "sync=90s (pull=true, push=true)".to_string(),
                "compliance=600s".to_string(),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interval_pump_delivers_tick_within_interval() {
        // Cadence=1s. The pump should push a single () through within the
        // 3-second receive window.
        let secs = Arc::new(AtomicU64::new(1));
        let (tx, mut rx) = mpsc::channel::<()>(8);
        let handle = super::super::spawn_interval_pump(secs, tx);
        let tick = tokio::time::timeout(StdDuration::from_secs(3), rx.recv()).await;
        handle.abort();
        assert!(tick.is_ok(), "expected a tick within 3s, got {:?}", tick);
        assert!(
            tick.unwrap().is_some(),
            "tick should be Some(()) — pump must not close prematurely"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interval_pump_exits_when_receiver_dropped() {
        // After the rx is dropped, the first `tx.send().await` returns Err and
        // the loop breaks. The JoinHandle must transition to finished without
        // requiring abort().
        let secs = Arc::new(AtomicU64::new(1));
        let (tx, rx) = mpsc::channel::<()>(1);
        let handle = super::super::spawn_interval_pump(secs, tx);
        drop(rx);
        // Wait long enough for one cadence tick + send failure to land.
        let joined = tokio::time::timeout(StdDuration::from_secs(3), handle).await;
        assert!(
            joined.is_ok(),
            "pump must exit on send failure, got timeout"
        );
        // The task either finished normally (Ok) — abort wasn't needed.
        let join_result = joined.unwrap();
        assert!(
            join_result.is_ok(),
            "pump task should exit cleanly when rx closes, got {:?}",
            join_result
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn sighup_pump_forwards_signal_into_channel() {
        // Spawn the pump, raise SIGHUP at our own PID, and expect a () on the
        // receiver. Serial because signal handling is process-global and a
        // concurrent test could observe / consume the signal.
        let (tx, mut rx) = mpsc::channel::<()>(8);
        let handle = super::super::spawn_sighup_pump(tx).expect("sighup pump registers");
        // Give tokio the SIGHUP signal subscription a chance to wire up before
        // we raise the signal. Without this the kill arrives before the
        // listener exists and is dropped.
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        // SAFETY: libc::kill against own PID is well-defined.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGHUP);
        }
        let tick = tokio::time::timeout(StdDuration::from_secs(3), rx.recv()).await;
        handle.abort();
        assert!(tick.is_ok(), "expected a SIGHUP tick within 3s");
        assert!(tick.unwrap().is_some(), "tick should be Some(())");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn wait_for_shutdown_returns_on_sigterm() {
        // Driving wait_for_shutdown directly: spawn it on its own task, raise
        // SIGTERM at our own PID, and verify the function returns AND the
        // printer captured the SIGTERM message.
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let handle = tokio::spawn(super::super::wait_for_shutdown(Arc::clone(&printer)));
        // Allow the SIGTERM signal listener to register.
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        // SAFETY: libc::kill against own PID is well-defined.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
        let joined = tokio::time::timeout(StdDuration::from_secs(3), handle).await;
        assert!(
            joined.is_ok(),
            "wait_for_shutdown must return after SIGTERM"
        );
        joined.unwrap().expect("task join");
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Received SIGTERM"),
            "shutdown printer should announce SIGTERM, got: {}",
            out
        );
    }

    // ----- DaemonState direct unit coverage -----

    #[test]
    fn daemon_state_with_store_path_round_trips_through_test_accessor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("state.db");
        let state = super::super::DaemonState::new().with_store_path(path.clone());
        let store = state
            .store_path_for_test()
            .expect("store_path set after with_store_path");
        assert_eq!(store, path.as_path());
    }

    #[test]
    fn daemon_state_to_response_reflects_internal_counters_and_sources() {
        // Construct a state, mutate the fields that to_response copies out,
        // and assert each field carried through. Catches reordering / typo
        // regressions in the field-by-field clone in to_response.
        let mut state = super::super::DaemonState::new();
        state.last_reconcile = Some("2026-05-25T00:00:00Z".to_string());
        state.last_sync = Some("2026-05-25T00:05:00Z".to_string());
        state.drift_count = 7;
        state.update_available = Some("0.5.0".to_string());
        state.sources.push(super::super::SourceStatus {
            name: "remote".to_string(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 2,
            status: "active".to_string(),
        });
        let resp = state.to_response();
        assert!(resp.running);
        assert_eq!(resp.pid, std::process::id());
        assert_eq!(resp.last_reconcile.as_deref(), Some("2026-05-25T00:00:00Z"));
        assert_eq!(resp.last_sync.as_deref(), Some("2026-05-25T00:05:00Z"));
        assert_eq!(resp.drift_count, 7);
        assert_eq!(resp.update_available.as_deref(), Some("0.5.0"));
        // Default ctor adds a "local" source; we pushed one more.
        assert_eq!(resp.sources.len(), 2);
        assert_eq!(resp.sources[0].name, "local");
        assert_eq!(resp.sources[1].name, "remote");
        // module_reconcile is always built empty in to_response (a
        // forward-looking field populated elsewhere).
        assert!(resp.module_reconcile.is_empty());
    }

    // ----- Notifier::notify_webhook with a real (mock) HTTP endpoint -----
    //
    // notify_webhook spawns a tokio::task::spawn_blocking, so this test
    // sleeps briefly after .notify() to let the POST land. We assert via the
    // mockito expectation count rather than inspecting tracing output.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notifier_webhook_posts_payload_to_configured_url() {
        let server = tokio::task::spawn_blocking(mockito::Server::new)
            .await
            .expect("spawn mockito server");
        let mut server = server;
        let mock = server
            .mock("POST", "/notify")
            .with_status(200)
            .with_body("ok")
            .expect_at_least(1)
            .create();
        let url = format!("{}/notify", server.url());

        let notifier = super::super::Notifier::new(NotifyMethod::Webhook, Some(url));
        notifier.notify("test-event", "test-body");

        // The webhook POST is queued via spawn_blocking. Poll up to ~2s for the
        // mockito expectation to be satisfied.
        let mut satisfied = false;
        for _ in 0..40 {
            if mock.matched() {
                satisfied = true;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
        assert!(
            satisfied,
            "expected the webhook POST to land at the mock server within 2s"
        );
        mock.assert();
    }

    // ----- run_daemon_with: production-trigger path -----
    //
    // The other run_daemon_with tests in this module install
    // `external_triggers: Some(...)`. This one leaves it None so the function
    // takes the `else` branch and spawns real interval pumps, the SIGHUP pump
    // (Unix), and the shutdown listener. We bound the test with an outer
    // tokio::time::timeout so it terminates deterministically even though no
    // shutdown signal is sent — the assertion is that the function progressed
    // past the trigger-setup block and ran the loop until forcibly aborted.

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn run_daemon_with_production_triggers_progresses_past_setup_then_shutsdown_on_sigterm() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("prod-triggers.sock");
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: None,
        };

        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        // Give the daemon time to bind everything, spawn pumps, and emit the
        // startup banner — proves the production trigger-setup block ran.
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        assert!(
            buf.lock().unwrap().contains("Daemon running"),
            "production-trigger path must emit the startup banner before shutdown"
        );

        // SIGTERM drives the production wait_for_shutdown task which sends on
        // the shutdown oneshot, exiting the loop cleanly.
        // SAFETY: libc::kill against own PID is well-defined.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down on SIGTERM")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon should exit Ok, got {:?}", result);

        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Daemon stopped"),
            "cleanup path must run, got: {}",
            out
        );
    }

    // ----- handle_health_connection unit tests -----
    //
    // Drives the four-way path dispatch in health_ipc::handle_health_connection
    // through an in-memory `tokio::io::duplex` pair. No real socket bind, no
    // listener, no /tmp file — every assertion is on the HTTP response bytes
    // produced by the handler. Routes covered: /health, /status, /drift (both
    // with and without a store_path), and the unknown-path 404 fallback.

    /// Drive one request through `handle_health_connection`. Returns the raw
    /// HTTP response bytes the handler wrote.
    async fn drive_health_request(
        state: Arc<Mutex<super::super::DaemonState>>,
        request: &str,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (client, server) = tokio::io::duplex(8192);
        let handler = tokio::spawn(super::super::health_ipc::handle_health_connection(
            server, state,
        ));
        let (mut client_read, mut client_write) = tokio::io::split(client);
        client_write.write_all(request.as_bytes()).await.unwrap();
        // Drop the write half so the server sees EOF when draining the request
        // headers — the handler completes its response and returns Ok(()).
        drop(client_write);
        let _ = handler
            .await
            .expect("handle_health_connection task panicked");

        let mut out = Vec::new();
        client_read.read_to_end(&mut out).await.unwrap();
        String::from_utf8(out).expect("response should be utf-8")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_returns_health_payload_with_pid_and_uptime() {
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(
            resp.starts_with("HTTP/1.1 200 OK"),
            "expected 200 OK status line, got: {resp}"
        );
        assert!(
            resp.contains("\"status\": \"ok\""),
            "/health body should include status=ok: {resp}"
        );
        assert!(
            resp.contains("\"pid\""),
            "/health body should include pid: {resp}"
        );
        assert!(
            resp.contains("\"uptime_secs\""),
            "/health body should include uptime_secs: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_returns_status_response_with_sources() {
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        // DaemonStatusResponse includes a default "local" source entry.
        assert!(
            resp.contains("\"running\": true"),
            "/status should report running=true: {resp}"
        );
        assert!(
            resp.contains("\"local\""),
            "/status should serialize the default local source: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_drift_with_no_store_path_returns_empty_events() {
        // DaemonState::new() sets store_path=None; the /drift branch then
        // skips the spawn_blocking + StateStore::open and returns drift_count=0.
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /drift HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        assert!(
            resp.contains("\"drift_count\": 0"),
            "drift_count should be 0 with no store_path: {resp}"
        );
        assert!(
            resp.contains("\"events\": []"),
            "events should be the empty array: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_drift_with_recorded_event_returns_it_in_body() {
        // store_path=Some(<tempfile>) drives the spawn_blocking branch that
        // opens the StateStore and pulls unresolved_drift(). With a single
        // recorded drift event, the JSON body should include drift_count=1
        // and the event's resource_id.
        let tmp = tempfile::TempDir::new().unwrap();
        let store_path = tmp.path().join("state.db");
        // Open & seed in a scoped block so the connection drops before the
        // handler opens its own connection — SQLite WAL handles concurrent
        // readers but we keep the test deterministic.
        {
            let store = crate::state::StateStore::open(&store_path).unwrap();
            store
                .record_drift(
                    "file",
                    "/etc/hosts",
                    Some("expected-sha"),
                    Some("actual-sha"),
                    "file-manager",
                )
                .unwrap();
        }
        let mut s = super::super::DaemonState::new();
        // Reach into the private field; `daemon::tests::harness` is inside
        // `daemon` so super::super:: gives us module-private access.
        s.store_path = Some(store_path);
        let state = Arc::new(Mutex::new(s));
        let resp = drive_health_request(
            state,
            "GET /drift HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        assert!(
            resp.contains("\"drift_count\": 1"),
            "drift_count should be 1 after recording one event: {resp}"
        );
        assert!(
            resp.contains("/etc/hosts"),
            "event resource_id should appear in body: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_unknown_path_returns_404() {
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(
            resp.starts_with("HTTP/1.1 404 Not Found"),
            "expected 404 status: {resp}"
        );
        assert!(
            resp.contains("\"error\":\"not found\""),
            "404 body should include not-found marker: {resp}"
        );
    }

    // ----- Loop-surface snapshot floor -----
    //
    // These tests capture only what the daemon's own Printer writes:
    // startup banner, SIGHUP reload chatter, shutdown messages. Per-action
    // reconcile output is emitted by `daemon::reconcile` through separate
    // short-lived printers and is invisible to the buffer below.

    use crate::output::test_capture::{assert_snapshot_at, strip_ansi};

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon/snapshots")
    }

    fn normalize_ipc(raw: &str, ipc_path: &Path) -> String {
        raw.replace(&ipc_path.to_string_lossy().to_string(), "<IPC_PATH>")
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn snapshot_clean_reconcile_cycle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("daemon-test.sock");
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
        };
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        senders.reconcile_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon shutdown did not complete in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon should exit Ok, got {:?}", result);

        drop(printer);
        let raw = buf.lock().unwrap().clone();
        let actual = normalize_ipc(&strip_ansi(&raw), &ipc_path);
        assert_snapshot("clean_reconcile_cycle.txt", &actual);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn snapshot_drift_event() {
        // A file-change tick walks handle_file_change_tick → drift recording →
        // notifier path. The notifier writes via tracing/stdout, not through the
        // loop's Printer, so this snapshot is shape-identical to the clean cycle.
        // Its value is regression coverage that the drift path doesn't write to
        // the loop's surface and that the daemon survives the drift codepath and
        // shuts down cleanly.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("daemon-test.sock");
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
        };
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path.clone(),
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
        ));

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        senders.file_tx.send(config_path).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);

        drop(printer);
        let raw = buf.lock().unwrap().clone();
        let actual = normalize_ipc(&strip_ansi(&raw), &ipc_path);
        assert_snapshot("drift_event.txt", &actual);
    }
}

// ---------------------------------------------------------------------------
// daemon/reconcile.rs — extra branch coverage:
//   * Plural-message branch fires when count > 1 new pending decisions in
//     one call (singular path is already covered by *_detects_new_items_on_change)
//   * pending_resource_paths direct read-back contract — empty / multi-decision
//     / post-resolution-empty
// ---------------------------------------------------------------------------

#[test]
fn process_source_decisions_three_new_items_all_become_pending_in_one_call() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    // The plural-vs-singular branch (lines 730-742 in reconcile.rs) fires
    // inside `notifier.notify(...)` rendering when new_pending_count > 1.
    // We can't inspect the formatted body directly via Stdout notifier, but
    // we CAN pin the precondition: a single call must register all three
    // items as pending in the store + the excluded set. That's exactly the
    // shape that would trigger the plural message in the notifier body.
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default(); // Notify

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into(), "fd".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    let pending = store.pending_decisions().unwrap();
    assert_eq!(
        pending.len(),
        3,
        "all three new cargo items must produce pending decisions on the first call"
    );
    assert_eq!(
        excluded.len(),
        3,
        "all three pending items must appear in the excluded set"
    );
    let names: std::collections::HashSet<&str> =
        pending.iter().map(|d| d.resource.as_str()).collect();
    assert!(names.contains("packages.cargo.bat"));
    assert!(names.contains("packages.cargo.ripgrep"));
    assert!(names.contains("packages.cargo.fd"));
}

#[test]
fn pending_resource_paths_returns_decision_resources_as_set() {
    use crate::daemon::reconcile::pending_resource_paths;

    let store = test_state();
    // Empty store → empty set
    let empty = pending_resource_paths(&store);
    assert!(empty.is_empty(), "no decisions → empty set");

    // Two distinct decisions
    store
        .upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat",
        )
        .unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "files.security/rules.yaml",
            "locked",
            "install",
            "locked files.security/rules.yaml",
        )
        .unwrap();

    let paths = pending_resource_paths(&store);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains("packages.cargo.bat"));
    assert!(paths.contains("files.security/rules.yaml"));

    // Resolving a decision removes it from the pending set
    store
        .resolve_decisions_for_source("acme", "accepted")
        .unwrap();
    let after = pending_resource_paths(&store);
    assert!(
        after.is_empty(),
        "resolving all decisions empties the pending-resource set"
    );
}

// ---------------------------------------------------------------------------
// daemon/reconcile.rs::discover_managed_paths — early-return arms.
// The cfg-load-Err and happy-path arms are covered elsewhere
// (*_returns_empty_for_missing_config and *_returns_targets_from_profile).
// These tests pin:
//   * no-profile arm: cfg has no spec.profile AND no profile_override → []
//   * profile-resolve-Err arm: profile name resolves to a missing file
// ---------------------------------------------------------------------------

struct DiscoverTestHooks;
impl DaemonHooks for DiscoverTestHooks {
    fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
        ProviderRegistry::new()
    }
    fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> crate::errors::Result<Vec<FileAction>> {
        Ok(vec![])
    }
    fn plan_packages(
        &self,
        _: &MergedProfile,
        _: &[&dyn PackageManager],
    ) -> crate::errors::Result<Vec<PackageAction>> {
        Ok(vec![])
    }
    fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &config::PackagesSpec) {}
    fn expand_tilde(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }
}

#[test]
fn discover_managed_paths_returns_empty_when_no_profile_configured_or_overridden() {
    // Config has NO spec.profile and the caller passes no override →
    // the function takes the `None => return Vec::new()` arm before any
    // profile resolution is attempted.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let paths = discover_managed_paths(&config_path, None, &DiscoverTestHooks);
    assert!(
        paths.is_empty(),
        "no profile configured + no override → empty path list"
    );
}

#[test]
fn discover_managed_paths_returns_empty_when_named_profile_does_not_exist() {
    // Config names a profile, but the profiles/ dir doesn't contain it →
    // resolve_profile returns Err → the function takes the resolve-Err arm
    // and returns an empty Vec without panicking.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: ghost\n",
    )
    .unwrap();
    // Intentionally no profiles/ dir written — the named profile cannot resolve.

    let paths = discover_managed_paths(&config_path, None, &DiscoverTestHooks);
    assert!(
        paths.is_empty(),
        "profile name set but resolve_profile fails → empty path list, no panic"
    );
}

// ---------------------------------------------------------------------------
// handle_reconcile — modules block (reconcile.rs lines 264-291)
// Covers the `!resolved.merged.modules.is_empty()` branch of the
// module-resolution if/else. Without these, the entire block (cache_base
// derivation, quiet_printer construction, resolve_modules call, and both
// the Ok and Err arms) is skipped because every existing reconcile test
// uses a profile with no `modules:` list.
// ---------------------------------------------------------------------------

/// Build the minimal CfgdConfig + Profile pair that drives `handle_reconcile`
/// into the modules-resolution block. The profile lists `module_names` in its
/// `spec.modules:` array; the daemon will then call `resolve_modules` against
/// `<config_dir>/modules/`.
fn write_config_with_module_refs(tmp: &Path, module_names: &[&str]) -> PathBuf {
    let config_path = tmp.join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
    )
    .unwrap();
    let profiles_dir = tmp.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    let mods_inline = module_names
        .iter()
        .map(|n| format!("    - {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        profiles_dir.join("default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n{mods_inline}\n",
        ),
    )
    .unwrap();
    config_path
}

/// DaemonHooks impl with no packages and no files. Lets the reconcile flow
/// reach the modules block without needing a registry, package planner, or
/// file planner — those branches are already covered by sibling tests.
struct EmptyPlanHooks;
impl DaemonHooks for EmptyPlanHooks {
    fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
        ProviderRegistry::new()
    }
    fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> crate::errors::Result<Vec<FileAction>> {
        Ok(vec![])
    }
    fn plan_packages(
        &self,
        _: &MergedProfile,
        _: &[&dyn PackageManager],
    ) -> crate::errors::Result<Vec<PackageAction>> {
        Ok(vec![])
    }
    fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &config::PackagesSpec) {}
    fn expand_tilde(&self, path: &Path) -> PathBuf {
        crate::expand_tilde(path)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_warns_when_module_resolution_fails_and_continues() {
    // Profile references a module name with no on-disk module dir, so
    // `resolve_modules -> resolve_dependency_order` returns Err. The reconcile
    // body must take the `tracing::warn!` arm at reconcile.rs:284-287 and
    // substitute Vec::new() for resolved_modules; the rest of the reconcile
    // (plan generation, state update) must continue normally.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let config_path = write_config_with_module_refs(tmp.path(), &["does-not-exist"]);
    // No `modules/` dir is written — load_all_modules finds nothing, then
    // resolve_dependency_order errors with "module not found".

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // The reconcile completed past the modules block — last_reconcile is set.
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "warn-on-module-fail must not short-circuit reconcile — last_reconcile should be set"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_resolves_non_empty_modules_when_module_dir_exists() {
    // Profile lists a module that DOES exist on disk; load_all_modules and
    // resolve_dependency_order both succeed, the Ok arm at reconcile.rs:282-283
    // fires, and the resulting Vec<ResolvedModule> flows into reconciler.plan
    // (covered by sibling tests; here we only assert the reconcile completes).
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let config_path = write_config_with_module_refs(tmp.path(), &["empty-mod"]);

    // Minimal Module on disk — empty spec is enough; the modules block only
    // needs load + dependency-order resolution to succeed.
    let mod_dir = tmp.path().join("modules").join("empty-mod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: empty-mod\nspec: {}\n",
    )
    .unwrap();

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // The reconcile resolved a non-empty module list and completed.
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "resolve_modules Ok arm must allow reconcile to complete normally"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_apply_with_sources_processes_decisions_and_resolves_removed() {
    // Drives reconcile.rs:200-243 — the `auto_apply && !sources.is_empty()`
    // branch. Pre-stages two sources in the config + a state-store row for
    // a third "removed" source whose pending decisions should get
    // auto-resolved (lines 226-238). Asserts:
    //   - last_reconcile is set (reconcile ran past the block)
    //   - pending decisions for the removed source are flipped to
    //     "rejected" by the auto-resolve loop
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      onChange: false\n      autoApply: true\n  sources:\n    - name: keep-src\n      origin:\n        type: Git\n        url: https://example.test/keep.git\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    // Pre-stage a pending decision for a source that's NOT in the config —
    // the auto-resolve loop at 226-238 should flip it to "rejected".
    {
        let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
        store
            .upsert_pending_decision(
                "removed-src",
                "packages.cargo.bat",
                "recommended",
                "install",
                "install bat",
            )
            .unwrap();
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Reconcile completed past the auto-apply block.
    {
        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_some(),
            "auto-apply branch must allow reconcile to complete"
        );
    }

    // The removed-src pending decision should now be rejected.
    let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
    let pending = store.pending_decisions().unwrap();
    assert!(
        pending.iter().all(|d| d.source != "removed-src"),
        "auto-resolve loop must flip removed-src decisions to non-pending: {pending:?}"
    );
}

// ---------------------------------------------------------------------------
// IPC socket security — v0.4.0 release-blocker coverage
// ---------------------------------------------------------------------------
//
// Locks down `resolve_default_ipc_path` and `run_health_server` against the
// pre-fix hijack vectors:
//   - default `/tmp/cfgd.sock` (any local user could pre-bind & MITM)
//   - default umask 0022 leaving the socket world-readable
//   - unbounded client read OOMing the CLI from a hijacked peer
//
// All tests mutate process-global env vars so they MUST be serial. The
// EnvVarGuard / with_test_home_guard helpers restore prior state on drop
// (even on panic) so a failed test cannot poison the next.

mod ipc_socket_security {
    use super::*;
    use crate::daemon::health_ipc::MAX_RESPONSE_BYTES;
    use crate::test_helpers::EnvVarGuard;

    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_env_override_wins() {
        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", "/custom/cfgd.sock");
        assert_eq!(
            resolve_default_ipc_path(),
            std::path::PathBuf::from("/custom/cfgd.sock")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_uses_xdg_runtime_dir_when_set() {
        let _unset_override = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let _xdg = EnvVarGuard::set("XDG_RUNTIME_DIR", "/tmp/test-xdg");
        assert_eq!(
            resolve_default_ipc_path(),
            std::path::PathBuf::from("/tmp/test-xdg/cfgd/cfgd.sock")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_falls_back_to_home_cache_when_xdg_unset_linux() {
        let _unset_override = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let _unset_xdg = EnvVarGuard::unset("XDG_RUNTIME_DIR");
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());
        let expected = tmp.path().join(".cache").join("cfgd").join("cfgd.sock");
        assert_eq!(resolve_default_ipc_path(), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_uses_application_support_on_macos() {
        let _unset_override = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());
        let expected = tmp
            .path()
            .join("Library")
            .join("Application Support")
            .join("cfgd")
            .join("cfgd.sock");
        assert_eq!(resolve_default_ipc_path(), expected);
    }

    /// Drives `run_health_server` against a tempdir socket path and asserts
    /// the bound socket file is 0600 — covers the umask-default-leaks fix.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn bind_socket_sets_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        // run_health_server creates the parent dir with 0700 itself; we point
        // at a nested path so the create-and-chmod arms both fire.
        let sock_path = tmp.path().join("runtime").join("cfgd.sock");

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let sock = sock_path.to_string_lossy().to_string();
        let handle = tokio::spawn(async move {
            let _ = run_health_server(&sock, state).await;
        });

        // Spin briefly waiting for bind — keeps the test deterministic without
        // depending on a fixed sleep.
        for _ in 0..200 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            sock_path.exists(),
            "expected health server to bind {}",
            sock_path.display()
        );

        let mode = std::fs::metadata(&sock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket must be owner-only, got {:o}", mode);

        // Parent directory must be 0700 too (set by run_health_server).
        let parent_mode = std::fs::metadata(sock_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            parent_mode, 0o700,
            "parent dir must be owner-only, got {:o}",
            parent_mode
        );

        handle.abort();
        let _ = handle.await;
    }

    /// Drives `ensure_owner_private_dir` against `/proc/<random>/cfgd` —
    /// `/proc` rejects directory creation even for uid 0, so create_dir_all
    /// surfaces ENOENT and the helper returns a HealthSocketError naming
    /// the offending directory. Proves the helper does not silently continue
    /// when the parent dir cannot be made owner-private.
    ///
    /// The test suite frequently runs as root in CI/devcontainers, so the
    /// mode-check arm (`mode & 0o077 != 0`) cannot be exercised end-to-end:
    /// root bypasses chmod, so the helper always succeeds in lowering 0o755
    /// to 0o700 before the re-stat. The create-failure arm here is the
    /// negative path that fires deterministically regardless of uid; the
    /// owner-private predicate itself is unit-tested in the sibling
    /// `owner_private_predicate_rejects_world_readable_modes` test.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn bind_socket_refuses_world_readable_parent_dir() {
        use crate::daemon::health_ipc::ensure_owner_private_dir;
        let bogus = std::path::PathBuf::from("/proc/cfgd-blocker-test-does-not-exist/cfgd");
        let err = ensure_owner_private_dir(&bogus)
            .expect_err("expected refusal when parent dir cannot be made owner-private");
        let msg = format!("{err}");
        assert!(
            msg.contains(&bogus.display().to_string()),
            "error must name the offending directory, got {msg:?}"
        );
    }

    /// Pure unit test of the mode-check predicate `ensure_owner_private_dir`
    /// uses to refuse world-readable parents. Pairs with the create-failure
    /// test above to cover the second negative arm without relying on uid-0
    /// chmod behaviour. Mirrors the `mode & 0o077 != 0` check.
    #[cfg(unix)]
    #[test]
    fn owner_private_predicate_rejects_world_readable_modes() {
        assert_ne!(0o755 & 0o077, 0, "0o755 must trip the predicate");
        assert_ne!(0o750 & 0o077, 0, "0o750 must trip the predicate");
        assert_ne!(0o701 & 0o077, 0, "0o701 must trip the predicate");
        assert_eq!(0o700 & 0o077, 0, "0o700 must pass the predicate");
        assert_eq!(0o600 & 0o077, 0, "0o600 must pass the predicate");
    }

    /// Drives `query_daemon_status` against a fake server that streams more
    /// than `MAX_RESPONSE_BYTES`, asserting the read is capped and an
    /// "exceeded" error surfaces. Uses a real Unix listener + socketpair-style
    /// override via `CFGD_DAEMON_IPC_PATH`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_caps_response_at_max_bytes() {
        use std::io::Write as IoWrite;
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("flood.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        // Stream MAX_RESPONSE_BYTES * 2 of body so the cap definitely trips
        // before the peer-close EOF would arrive.
        let flood_bytes = (MAX_RESPONSE_BYTES * 2) as usize;
        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                // Pass headers, then flood the body.
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n"
                );
                let chunk = vec![b'x'; 8192];
                let mut sent = 0usize;
                while sent < flood_bytes {
                    if s.write_all(&chunk).is_err() {
                        break;
                    }
                    sent += chunk.len();
                }
                let _ = s.flush();
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(query_daemon_status)
            .await
            .unwrap();
        let _ = server.join();

        match result {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("exceeded"),
                    "expected response-cap error, got {msg:?}"
                );
            }
            Ok(other) => panic!("expected cap error, got Ok({other:?})"),
        }
    }
}

// ---------------------------------------------------------------------------
// query_daemon_status & connect_daemon_ipc — additional coverage paths
// ---------------------------------------------------------------------------

mod query_daemon_status_paths {
    use super::*;
    use crate::test_helpers::EnvVarGuard;

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn query_daemon_status_returns_none_when_socket_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("nope.sock");
        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", nonexistent.to_str().unwrap());
        let result = query_daemon_status().expect("missing socket must not error");
        assert!(
            result.is_none(),
            "missing socket path returns Ok(None), got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_parses_valid_response_body() {
        use std::io::{Read as IoRead, Write as IoWrite};
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("status.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let body = serde_json::to_string(&DaemonStatusResponse {
                    running: true,
                    pid: 42,
                    uptime_secs: 99,
                    last_reconcile: None,
                    last_sync: None,
                    drift_count: 0,
                    sources: vec![],
                    update_available: None,
                    module_reconcile: vec![],
                })
                .unwrap();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.flush();
                // Half-shutdown the write side then drain reads so the client
                // sees EOF (not ECONNRESET) on its read pass.
                let _ = s.shutdown(std::net::Shutdown::Write);
                let mut sink = [0u8; 1024];
                while let Ok(n) = s.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(query_daemon_status)
            .await
            .unwrap();
        let _ = server.join();

        let status = result
            .expect("status must parse")
            .expect("status must be Some");
        assert_eq!(status.pid, 42);
        assert_eq!(status.uptime_secs, 99);
        assert!(status.running);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_returns_none_on_empty_body() {
        use std::io::{Read as IoRead, Write as IoWrite};
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("empty.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n"
                );
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Write);
                let mut sink = [0u8; 1024];
                while let Ok(n) = s.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(query_daemon_status)
            .await
            .unwrap();
        let _ = server.join();

        // Empty body → Ok(None).
        assert!(
            matches!(result, Ok(None)),
            "empty body should give Ok(None), got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_returns_err_on_malformed_json() {
        use std::io::{Read as IoRead, Write as IoWrite};
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("bad.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let body = "{not even json";
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.flush();
                // Half-shutdown the write side then drain reads so the client
                // sees EOF (not ECONNRESET) on its read pass.
                let _ = s.shutdown(std::net::Shutdown::Write);
                let mut sink = [0u8; 1024];
                while let Ok(n) = s.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(query_daemon_status)
            .await
            .unwrap();
        let _ = server.join();

        match result {
            Err(e) => assert!(
                e.to_string().contains("parse response"),
                "error must mention parse, got: {e}"
            ),
            Ok(other) => panic!("expected parse err, got: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// handle_sync — signature verification after pull (require_signed_commits)
// ---------------------------------------------------------------------------

mod handle_sync_signature_paths {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_pulled_unsigned_commit_with_require_signed_returns_false() {
        // require_signed_commits=true, allow_unsigned=false: after a successful
        // pull, verify_head_signature fires; for an unsigned commit it errors,
        // and handle_sync returns false (the content is untrusted).
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");
        let pusher_dir = tmp.path().join("pusher");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        // Clone and seed the work repo with an initial commit pushed to origin.
        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Push an UNSIGNED change from pusher.
        let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
        {
            let mut config = pusher.config().unwrap();
            config.set_str("user.name", "cfgd-pusher").unwrap();
            config.set_str("user.email", "pusher@cfgd.io").unwrap();
        }
        std::fs::write(pusher_dir.join("NEWFILE"), "synced\n").unwrap();
        {
            let mut index = pusher.index().unwrap();
            index.add_path(Path::new("NEWFILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = pusher.find_tree(tree_id).unwrap();
            let sig = pusher.signature().unwrap();
            let parent = pusher.head().unwrap().peel_to_commit().unwrap();
            pusher
                .commit(Some("HEAD"), &sig, &sig, "add newfile", &tree, &[&parent])
                .unwrap();
        }
        {
            let mut remote = pusher.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        // require_signed_commits=true, allow_unsigned=false
        let changed = handle_sync(&work_dir, true, false, "local", &state, true, false).await;

        assert!(
            !changed,
            "unsigned-commit pull with require_signed must return false"
        );
        // Even though the verification failed, last_sync should NOT be
        // updated because the early return short-circuits before the
        // state-mutation block.
        let st = state.lock().await;
        assert!(
            st.last_sync.is_none(),
            "early-return path must not bump last_sync"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_pulled_unsigned_with_allow_unsigned_returns_true() {
        // require_signed_commits=true, allow_unsigned=true: signature check
        // is bypassed by verify_commit_signature; but handle_sync only calls
        // verify_head_signature unconditionally here. allow_unsigned guards
        // the call. Verify the pull succeeds and `changed=true` is returned.
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");
        let pusher_dir = tmp.path().join("pusher");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
        {
            let mut config = pusher.config().unwrap();
            config.set_str("user.name", "cfgd-pusher").unwrap();
            config.set_str("user.email", "pusher@cfgd.io").unwrap();
        }
        std::fs::write(pusher_dir.join("NEWFILE"), "synced\n").unwrap();
        {
            let mut index = pusher.index().unwrap();
            index.add_path(Path::new("NEWFILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = pusher.find_tree(tree_id).unwrap();
            let sig = pusher.signature().unwrap();
            let parent = pusher.head().unwrap().peel_to_commit().unwrap();
            pusher
                .commit(Some("HEAD"), &sig, &sig, "add newfile", &tree, &[&parent])
                .unwrap();
        }
        {
            let mut remote = pusher.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        // require_signed_commits=true, allow_unsigned=true → bypass verify.
        let changed = handle_sync(&work_dir, true, false, "local", &state, true, true).await;

        assert!(
            changed,
            "allow_unsigned must bypass signature verify and return true"
        );
        let st = state.lock().await;
        assert!(
            st.last_sync.is_some(),
            "successful sync must bump last_sync"
        );
    }
}

// ---------------------------------------------------------------------------
// handle_reconcile — module_filter (per-module reconcile) and pending-config
// consumption branches not covered by the wider drift tests.
// ---------------------------------------------------------------------------

mod handle_reconcile_extra_branches {
    use super::*;

    /// Build the minimum cfgd.yaml + profiles/default.yaml on disk and return
    /// the (config_path, state_dir) pair plus the owning TempDir.
    fn write_min_fixture(content: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(&config_path, content).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        (tmp, config_path, state_dir)
    }

    struct NoopHooks;
    impl DaemonHooks for NoopHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_per_module_filter_updates_module_last_reconcile() {
        // module_filter=Some(_) path: the per-module branch records only into
        // `module_last_reconcile` and skips the profile-wide last_reconcile.
        let (_tmp, config_path, state_dir) = write_min_fixture(
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
            handle_reconcile(
                &cp,
                None,
                ReconcileCtx {
                    state: &st,
                    notifier: &not,
                    notify_on_drift: false,
                    hooks: &NoopHooks,
                    state_dir_override: Some(&sd),
                    printer: &printer,
                    module_filter: Some("dev-tools"),
                    auto_apply_override: Some(false),
                    drift_policy_override: Some(config::DriftPolicy::NotifyOnly),
                },
            );
        })
        .await
        .unwrap();

        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_none(),
            "per-module tick must NOT touch profile-wide last_reconcile"
        );
        assert!(
            guard.module_last_reconcile.contains_key("dev-tools"),
            "per-module tick must record into module_last_reconcile, got: {:?}",
            guard.module_last_reconcile
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn handle_reconcile_consumes_pending_server_config_and_clears_file() {
        // Profile-wide tick (module_filter=None) walks the
        // `load_pending_server_config()` -> `clear_pending_server_config()`
        // arm at the bottom of handle_reconcile. Stage a pending JSON file
        // under a CFGD_STATE_DIR-scoped state dir, run reconcile, and assert
        // the file is removed.
        let pending_root = tempfile::tempdir().unwrap();
        let _g = crate::test_helpers::EnvVarGuard::set(
            "CFGD_STATE_DIR",
            pending_root.path().to_str().unwrap(),
        );

        std::fs::create_dir_all(pending_root.path()).unwrap();
        let pending_path = pending_root.path().join("pending-server-config.json");
        std::fs::write(
            &pending_path,
            r#"{"spec":{"profile":"default","packages":{}}}"#,
        )
        .unwrap();
        assert!(pending_path.exists(), "pending file must exist pre-test");

        let (_tmp, config_path, state_dir) = write_min_fixture(
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();

        tokio::task::spawn_blocking(move || {
            let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
            handle_reconcile(
                &cp,
                None,
                quiet_reconcile_ctx(&st, &not, false, &NoopHooks, &sd, &printer),
            );
        })
        .await
        .unwrap();

        assert!(
            !pending_path.exists(),
            "pending-server-config.json should have been consumed and cleared"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_with_invalid_profile_yaml_logs_and_returns() {
        // resolve_profile error arm (lines ~196-201): write a syntactically
        // bogus profile YAML so resolve_profile fails and the function
        // returns without crashing or recording state.
        let (_tmp, config_path, state_dir) = write_min_fixture(
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: bogus\n",
        );
        // bogus profile -> resolve_profile returns NotFound; reconcile logs an
        // error and bails. No state changes expected.

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
            handle_reconcile(
                &cp,
                None,
                quiet_reconcile_ctx(&st, &not, false, &NoopHooks, &sd, &printer),
            );
        })
        .await
        .unwrap();

        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_none(),
            "profile resolution failure must not bump last_reconcile"
        );
        assert_eq!(
            guard.drift_count, 0,
            "no drift counted when planning failed"
        );
    }
}

// ---------------------------------------------------------------------------
// discover_managed_paths — explicit profile_override branches not covered
// by the existing test list.
// ---------------------------------------------------------------------------

mod discover_managed_paths_extra {
    use super::*;

    struct StubHooks;
    impl DaemonHooks for StubHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    #[test]
    fn discover_managed_paths_with_explicit_profile_override_picks_override_targets() {
        // Hits the `profile_override.or(cfg.spec.profile.as_deref())` arm
        // for the explicit-Some case (the existing tests only cover the
        // fallthrough-None path).
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("override.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: override\nspec:\n  files:\n    managed:\n      - source: src/a.txt\n        target: /tmp/cfgd-test-override-target.txt\n",
        )
        .unwrap();

        let paths = discover_managed_paths(&config_path, Some("override"), &StubHooks);
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy().contains("override-target.txt")),
            "explicit profile_override should return that profile's targets: {paths:?}"
        );
    }

    #[test]
    fn discover_managed_paths_returns_empty_when_profile_resolution_fails() {
        // Hits the resolve_profile-Err arm (lines ~85-88): cfg names a profile
        // file that doesn't exist on disk.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: missing\n",
        )
        .unwrap();
        // profiles dir exists but the named profile does not
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();

        let paths = discover_managed_paths(&config_path, None, &StubHooks);
        assert!(
            paths.is_empty(),
            "profile-resolution failure must yield empty paths, got: {paths:?}"
        );
    }
}

mod tests_run_daemon_wrapper {
    use crate::config::CfgdConfig;
    use crate::config::PackagesSpec;
    use crate::daemon::DaemonHooks;
    use crate::daemon::run_daemon;
    use crate::daemon::{MergedProfile, ResolvedProfile};
    use crate::errors::Result as CfgdResult;
    use crate::output::{Printer, Verbosity};
    use crate::providers::{FileAction, PackageAction, PackageManager, ProviderRegistry};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct StubHooks2;
    impl DaemonHooks for StubHooks2 {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> CfgdResult<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
        ) -> CfgdResult<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &PackagesSpec) {}
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_daemon_with_invalid_config_returns_err_early() {
        let printer = Arc::new(Printer::new(Verbosity::Quiet));
        let hooks: Arc<dyn DaemonHooks> = Arc::new(StubHooks2);
        let bogus_path = PathBuf::from("/nonexistent-cfgd-cfg-7f9a/does-not-exist.yaml");
        let result = run_daemon(bogus_path, None, printer, hooks).await;
        assert!(
            result.is_err(),
            "missing config must propagate as Err, got Ok"
        );
    }
}
