use super::*;

use std::collections::HashMap;

#[test]
fn snapshot_serializes_to_json() {
    let snapshot = ComplianceSnapshot {
        timestamp: "2026-03-25T00:00:00Z".into(),
        machine: MachineInfo {
            hostname: "test-host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![
            ComplianceCheck {
                category: "file".into(),
                target: Some("/home/user/.zshrc".into()),
                status: ComplianceStatus::Compliant,
                detail: Some("present".into()),
                ..Default::default()
            },
            ComplianceCheck {
                category: "package".into(),
                name: Some("ripgrep".into()),
                manager: Some("apt".into()),
                status: ComplianceStatus::Violation,
                detail: Some("not installed".into()),
                ..Default::default()
            },
        ],
        summary: ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 1,
        },
    };

    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    assert!(json.contains("\"timestamp\""));
    assert!(json.contains("\"machine\""));
    assert!(json.contains("\"test-host\""));
    assert!(json.contains("\"Compliant\""));
    assert!(json.contains("\"Violation\""));

    // Roundtrip
    let parsed: ComplianceSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.profile, "default");
    assert_eq!(parsed.checks.len(), 2);
    assert_eq!(parsed.summary.compliant, 1);
    assert_eq!(parsed.summary.violation, 1);
}

#[test]
fn summary_counts_match_check_statuses() {
    let checks = vec![
        ComplianceCheck {
            category: "file".into(),
            status: ComplianceStatus::Compliant,
            ..Default::default()
        },
        ComplianceCheck {
            category: "file".into(),
            status: ComplianceStatus::Compliant,
            ..Default::default()
        },
        ComplianceCheck {
            category: "package".into(),
            status: ComplianceStatus::Violation,
            ..Default::default()
        },
        ComplianceCheck {
            category: "system".into(),
            status: ComplianceStatus::Warning,
            ..Default::default()
        },
        ComplianceCheck {
            category: "system".into(),
            status: ComplianceStatus::Warning,
            ..Default::default()
        },
        ComplianceCheck {
            category: "file".into(),
            status: ComplianceStatus::Violation,
            ..Default::default()
        },
    ];

    let summary = compute_summary(&checks);
    assert_eq!(summary.compliant, 2);
    assert_eq!(summary.warning, 2);
    assert_eq!(summary.violation, 2);
}

#[test]
fn collect_file_checks_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.conf");
    std::fs::write(&file_path, "content").unwrap();

    let profile = MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![crate::config::ManagedFileSpec {
                source: "test.conf".into(),
                target: file_path.clone(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
        ..Default::default()
    };

    let checks = collect_file_checks(&profile);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert_eq!(checks[0].detail.as_deref(), Some("present"));
}

#[test]
fn collect_file_checks_missing_file() {
    let profile = MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![crate::config::ManagedFileSpec {
                source: "test.conf".into(),
                target: "/tmp/cfgd-nonexistent-file-12345".into(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
        ..Default::default()
    };

    let checks = collect_file_checks(&profile);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert_eq!(checks[0].detail.as_deref(), Some("managed file missing"));
}

#[cfg(unix)]
#[test]
fn collect_file_checks_permissions_match() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("secret.key");
    std::fs::write(&file_path, "key-data").unwrap();
    crate::set_file_permissions(&file_path, 0o600).unwrap();

    let profile = MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![crate::config::ManagedFileSpec {
                source: "secret.key".into(),
                target: file_path.clone(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: Some("600".into()),
            }],
            permissions: HashMap::new(),
        },
        ..Default::default()
    };

    let checks = collect_file_checks(&profile);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert!(checks[0].detail.as_deref().unwrap().contains("0o600"));
}

#[cfg(unix)]
#[test]
fn collect_file_checks_permissions_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("secret.key");
    std::fs::write(&file_path, "key-data").unwrap();
    crate::set_file_permissions(&file_path, 0o644).unwrap();

    let profile = MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![crate::config::ManagedFileSpec {
                source: "secret.key".into(),
                target: file_path.clone(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: Some("600".into()),
            }],
            permissions: HashMap::new(),
        },
        ..Default::default()
    };

    let checks = collect_file_checks(&profile);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    assert!(checks[0].detail.as_deref().unwrap().contains("expected"));
}

#[test]
fn collect_system_checks_maps_drifts() {
    use crate::providers::{ProviderRegistry, SystemDrift};
    use crate::test_helpers::MockSystemConfigurator;

    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("shell").with_drift(
            vec![SystemDrift {
                key: "defaultShell".into(),
                expected: "/bin/zsh".into(),
                actual: "/bin/bash".into(),
            }],
        )));

    let mut system = HashMap::new();
    system.insert(
        "shell".to_owned(),
        serde_yaml::Value::String("/bin/zsh".into()),
    );

    let profile = MergedProfile {
        system,
        ..Default::default()
    };

    let checks = collect_system_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert_eq!(checks[0].key.as_deref(), Some("shell.defaultShell"));
    assert!(checks[0].detail.as_deref().unwrap().contains("/bin/bash"));
}

#[test]
fn collect_system_checks_compliant_when_no_drift() {
    use crate::providers::ProviderRegistry;
    use crate::test_helpers::MockSystemConfigurator;

    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("shell")));

    let mut system = HashMap::new();
    system.insert(
        "shell".to_owned(),
        serde_yaml::Value::String("/bin/zsh".into()),
    );

    let profile = MergedProfile {
        system,
        ..Default::default()
    };

    let checks = collect_system_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
}

#[test]
fn collect_secret_checks_target_exists() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("token.txt");
    std::fs::write(&target, "redacted").unwrap();

    let profile = MergedProfile {
        secrets: vec![crate::config::SecretSpec {
            source: "vault://secret/token".into(),
            target: Some(target.clone()),
            template: None,
            backend: None,
            envs: None,
        }],
        ..Default::default()
    };

    let checks = collect_secret_checks(&profile);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
}

#[test]
fn collect_secret_checks_target_missing() {
    let profile = MergedProfile {
        secrets: vec![crate::config::SecretSpec {
            source: "vault://secret/token".into(),
            target: Some("/tmp/cfgd-nonexistent-secret-12345".into()),
            template: None,
            backend: None,
            envs: None,
        }],
        ..Default::default()
    };

    let checks = collect_secret_checks(&profile);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
}

#[test]
fn collect_secret_checks_env_only_skipped() {
    let profile = MergedProfile {
        secrets: vec![crate::config::SecretSpec {
            source: "vault://secret/api-key".into(),
            target: None,
            template: None,
            backend: None,
            envs: Some(vec!["API_KEY=vault://secret/api-key".into()]),
        }],
        ..Default::default()
    };

    let checks = collect_secret_checks(&profile);
    assert!(checks.is_empty());
}

#[test]
fn watch_path_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("watched.conf");
    std::fs::write(&file_path, "data").unwrap();

    let checks = collect_watch_path_checks(&file_path.to_string_lossy());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert!(checks[0].detail.as_deref().unwrap().contains("file"));
}

#[test]
fn watch_path_nonexistent() {
    let checks = collect_watch_path_checks("/tmp/cfgd-nonexistent-watch-12345");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
}

#[test]
fn watch_package_manager_not_available() {
    let registry = ProviderRegistry::new();
    let checks = collect_watched_package_manager_checks("nonexistent-pm", &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].category, "watchPackage");
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    assert!(
        checks[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("not available")
    );
}

#[test]
fn watch_package_manager_returns_installed() {
    use crate::providers::StubPackageManager;

    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("mock").with_installed(&["ripgrep", "fd"]),
    ));

    let checks = collect_watched_package_manager_checks("mock", &registry).unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|c| c.category == "watchPackage"));
    assert!(
        checks
            .iter()
            .all(|c| c.status == ComplianceStatus::Compliant)
    );
    assert!(checks.iter().all(|c| c.manager.as_deref() == Some("mock")));
    // Sorted by name
    assert_eq!(checks[0].name.as_deref(), Some("fd"));
    assert_eq!(checks[1].name.as_deref(), Some("ripgrep"));
}

#[test]
fn export_snapshot_to_file_json() {
    let dir = tempfile::tempdir().unwrap();
    let export = ComplianceExport {
        format: ComplianceFormat::Json,
        path: dir.path().display().to_string(),
    };
    let snapshot = ComplianceSnapshot {
        timestamp: "2026-03-25T12:00:00Z".into(),
        machine: MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![],
        summary: ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 0,
        },
    };

    let path = export_snapshot_to_file(&snapshot, &export).unwrap();
    assert!(path.exists());
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".json")
    );

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: ComplianceSnapshot = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.profile, "default");
}

#[test]
fn export_snapshot_to_file_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let export = ComplianceExport {
        format: ComplianceFormat::Yaml,
        path: dir.path().display().to_string(),
    };
    let snapshot = ComplianceSnapshot {
        timestamp: "2026-03-25T12:00:00Z".into(),
        machine: MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![],
        summary: ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 0,
        },
    };

    let path = export_snapshot_to_file(&snapshot, &export).unwrap();
    assert!(path.exists());
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".yaml")
    );
}

// -----------------------------------------------------------------------
// collect_package_checks
// -----------------------------------------------------------------------

#[test]
fn collect_package_checks_installed_package_compliant() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    let mut profile = MergedProfile::default();
    // Use pipx (Vec<String>) which is simpler to construct
    profile.packages.pipx = vec!["ripgrep".into()];

    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("pipx").with_installed(&["ripgrep"]),
    ));

    let checks = collect_package_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert_eq!(checks[0].name.as_deref(), Some("ripgrep"));
    assert_eq!(checks[0].manager.as_deref(), Some("pipx"));
}

#[test]
fn collect_package_checks_missing_package_violation() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    let mut profile = MergedProfile::default();
    profile.packages.pipx = vec!["missing-pkg".into()];

    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("pipx").with_installed(&[]),
    ));

    let checks = collect_package_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert!(
        checks[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("not installed")
    );
}

#[test]
fn collect_package_checks_empty_desired_skips_manager() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    let profile = MergedProfile::default();
    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("pipx").with_installed(&["curl"]),
    ));

    let checks = collect_package_checks(&profile, &registry).unwrap();
    assert!(checks.is_empty(), "no desired packages = no checks");
}

#[test]
fn collect_package_checks_manager_query_error_emits_warning_and_skips_packages() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    let mut profile = MergedProfile::default();
    // Two desired packages — should be skipped entirely when the manager
    // fails to enumerate; only a single Warning emerges for the manager.
    profile.packages.pipx = vec!["ripgrep".into(), "fd".into()];

    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("pipx").with_installed_error("permission denied: /var/lib/pipx"),
    ));

    let checks = collect_package_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1, "single Warning per unqueryable manager");
    assert_eq!(checks[0].category, "package");
    assert_eq!(checks[0].manager.as_deref(), Some("pipx"));
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    let detail = checks[0].detail.as_deref().unwrap();
    assert!(
        detail.contains("cannot query pipx"),
        "expected 'cannot query <name>', got: {detail}"
    );
    assert!(
        detail.contains("permission denied"),
        "expected underlying error in detail, got: {detail}"
    );
    // Ensure the per-package iteration was skipped (no name-bearing checks).
    assert!(
        checks.iter().all(|c| c.name.is_none()),
        "no per-package checks should be emitted on query failure"
    );
}

#[test]
fn watch_package_manager_query_error_emits_warning() {
    use crate::providers::StubPackageManager;

    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("snap")
            .with_installed_error("snapd not responding (no such file or directory)"),
    ));

    let checks = collect_watched_package_manager_checks("snap", &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].category, "watchPackage");
    assert_eq!(checks[0].manager.as_deref(), Some("snap"));
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    let detail = checks[0].detail.as_deref().unwrap();
    assert!(
        detail.contains("cannot query snap"),
        "expected 'cannot query <name>', got: {detail}"
    );
    assert!(
        detail.contains("snapd not responding"),
        "expected underlying error in detail, got: {detail}"
    );
}

#[test]
fn collect_package_checks_multiple_managers() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    let mut profile = MergedProfile::default();
    profile.packages.pipx = vec!["ripgrep".into()];
    profile.packages.dnf = vec!["fd-find".into()];

    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("pipx").with_installed(&["ripgrep"]),
    ));
    registry
        .package_managers
        .push(Box::new(StubPackageManager::new("dnf").with_installed(&[])));

    let checks = collect_package_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 2);
    let pipx_check = checks
        .iter()
        .find(|c| c.manager.as_deref() == Some("pipx"))
        .unwrap();
    assert_eq!(pipx_check.status, ComplianceStatus::Compliant);
    let dnf_check = checks
        .iter()
        .find(|c| c.manager.as_deref() == Some("dnf"))
        .unwrap();
    assert_eq!(dnf_check.status, ComplianceStatus::Violation);
}

// -----------------------------------------------------------------------
// collect_system_checks
// -----------------------------------------------------------------------

// Inline mock for system configurator tests (test_helpers is feature-gated)
struct InlineSystemMock {
    configurator_name: String,
    // Store as tuples to avoid Clone requirement on SystemDrift
    drift_tuples: Vec<(String, String, String)>,
    should_fail: bool,
}
impl crate::providers::SystemConfigurator for InlineSystemMock {
    fn name(&self) -> &str {
        &self.configurator_name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn current_state(&self) -> crate::errors::Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }
    fn diff(
        &self,
        _desired: &serde_yaml::Value,
    ) -> crate::errors::Result<Vec<crate::providers::SystemDrift>> {
        if self.should_fail {
            Err(crate::errors::CfgdError::Io(std::io::Error::other(
                "mock diff failure",
            )))
        } else {
            Ok(self
                .drift_tuples
                .iter()
                .map(|(k, e, a)| crate::providers::SystemDrift {
                    key: k.clone(),
                    expected: e.clone(),
                    actual: a.clone(),
                })
                .collect())
        }
    }
    fn apply(
        &self,
        _desired: &serde_yaml::Value,
        _printer: &crate::output::Printer,
    ) -> crate::errors::Result<()> {
        Ok(())
    }
}

#[test]
fn collect_system_checks_no_drift_compliant() {
    use crate::config::MergedProfile;

    let mut profile = MergedProfile::default();
    profile.system.insert(
        "mock".to_string(),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    );

    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(InlineSystemMock {
            configurator_name: "mock".to_string(),
            drift_tuples: vec![],
            should_fail: false,
        }));

    let checks = collect_system_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
}

#[test]
fn collect_system_checks_with_drift_violation() {
    use crate::config::MergedProfile;
    let mut profile = MergedProfile::default();
    profile.system.insert(
        "mock".to_string(),
        serde_yaml::Value::String("desired".into()),
    );

    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(InlineSystemMock {
            configurator_name: "mock".to_string(),
            drift_tuples: vec![("net.ipv4.ip_forward".into(), "1".into(), "0".into())],
            should_fail: false,
        }));

    let checks = collect_system_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert!(checks[0].detail.as_deref().unwrap().contains("expected 1"));
    assert!(checks[0].detail.as_deref().unwrap().contains("actual 0"));
}

#[test]
fn collect_system_checks_missing_configurator_warning() {
    use crate::config::MergedProfile;

    let mut profile = MergedProfile::default();
    profile.system.insert(
        "nonexistent".to_string(),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    );

    let registry = ProviderRegistry::new();
    let checks = collect_system_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    assert!(
        checks[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("no configurator")
    );
}

#[test]
fn collect_system_checks_diff_error_warning() {
    use crate::config::MergedProfile;

    let mut profile = MergedProfile::default();
    profile.system.insert(
        "mock".to_string(),
        serde_yaml::Value::String("desired".into()),
    );

    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(InlineSystemMock {
            configurator_name: "mock".to_string(),
            drift_tuples: vec![],
            should_fail: true,
        }));

    let checks = collect_system_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    assert!(checks[0].detail.as_deref().unwrap().contains("diff failed"));
}

#[test]
fn collect_system_checks_multiple_drifts_multiple_violations() {
    use crate::config::MergedProfile;
    let mut profile = MergedProfile::default();
    profile.system.insert(
        "mock".to_string(),
        serde_yaml::Value::String("desired".into()),
    );

    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(InlineSystemMock {
            configurator_name: "mock".to_string(),
            drift_tuples: vec![
                ("a".into(), "1".into(), "0".into()),
                ("b".into(), "true".into(), "false".into()),
            ],
            should_fail: false,
        }));

    let checks = collect_system_checks(&profile, &registry).unwrap();
    assert_eq!(checks.len(), 2);
    assert!(
        checks
            .iter()
            .all(|c| c.status == ComplianceStatus::Violation)
    );
}

#[test]
fn watch_path_directory() {
    let dir = tempfile::tempdir().unwrap();
    let checks = collect_watch_path_checks(&dir.path().to_string_lossy());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert!(checks[0].detail.as_deref().unwrap().contains("directory"));
}

#[test]
fn export_snapshot_creates_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("deep/nested/dir");
    let export = ComplianceExport {
        format: ComplianceFormat::Json,
        path: nested.display().to_string(),
    };
    let snapshot = ComplianceSnapshot {
        timestamp: "2026-03-25T12:00:00Z".into(),
        machine: MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![],
        summary: ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 0,
        },
    };

    let path = export_snapshot_to_file(&snapshot, &export).unwrap();
    assert!(path.exists());
    assert!(nested.exists());
}

#[cfg(unix)]
#[test]
fn collect_file_checks_invalid_permission_string_warns() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("malformed.conf");
    std::fs::write(&file_path, "content").unwrap();

    let profile = MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![crate::config::ManagedFileSpec {
                source: "malformed.conf".into(),
                target: file_path.clone(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: Some("not-octal".into()),
            }],
            permissions: HashMap::new(),
        },
        ..Default::default()
    };

    let checks = collect_file_checks(&profile);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    let detail = checks[0].detail.as_deref().unwrap();
    assert!(
        detail.contains("invalid permission string"),
        "expected invalid-permission detail, got: {detail}"
    );
    assert!(
        detail.contains("not-octal"),
        "detail should echo the bad string, got: {detail}"
    );
}

#[test]
fn collect_file_checks_with_encryption_declared_adds_file_encryption_check() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("secret.enc.yaml");
    std::fs::write(&file_path, "encrypted-blob").unwrap();

    let profile = MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![crate::config::ManagedFileSpec {
                source: "secret.enc.yaml".into(),
                target: file_path.clone(),
                strategy: None,
                private: false,
                origin: None,
                encryption: Some(crate::config::EncryptionSpec {
                    backend: "sops".into(),
                    mode: crate::config::EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
        ..Default::default()
    };

    let checks = collect_file_checks(&profile);
    // First check: file present (no permissions declared → Compliant "present").
    // Second check: encryption declaration → "file-encryption" category, Compliant.
    assert_eq!(checks.len(), 2, "expected file + encryption checks");
    let enc = checks
        .iter()
        .find(|c| c.category == "file-encryption")
        .expect("expected a file-encryption category check");
    assert_eq!(enc.status, ComplianceStatus::Compliant);
    let detail = enc.detail.as_deref().unwrap();
    assert!(
        detail.contains("backend=sops"),
        "expected backend in detail, got: {detail}"
    );
    assert_eq!(
        enc.target.as_deref(),
        Some(crate::to_posix_string(&file_path).as_str())
    );
}

#[test]
fn compute_summary_all_statuses() {
    let checks = vec![
        ComplianceCheck {
            status: ComplianceStatus::Compliant,
            ..Default::default()
        },
        ComplianceCheck {
            status: ComplianceStatus::Compliant,
            ..Default::default()
        },
        ComplianceCheck {
            status: ComplianceStatus::Warning,
            ..Default::default()
        },
        ComplianceCheck {
            status: ComplianceStatus::Violation,
            ..Default::default()
        },
        ComplianceCheck {
            status: ComplianceStatus::Violation,
            ..Default::default()
        },
        ComplianceCheck {
            status: ComplianceStatus::Violation,
            ..Default::default()
        },
    ];
    let summary = compute_summary(&checks);
    assert_eq!(summary.compliant, 2);
    assert_eq!(summary.warning, 1);
    assert_eq!(summary.violation, 3);
}
