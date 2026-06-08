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

    let checks = collect_file_checks(
        &profile,
        &[],
        std::path::Path::new("."),
        &ProviderRegistry::new(),
    );
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

    let checks = collect_file_checks(
        &profile,
        &[],
        std::path::Path::new("."),
        &ProviderRegistry::new(),
    );
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

    let checks = collect_file_checks(
        &profile,
        &[],
        std::path::Path::new("."),
        &ProviderRegistry::new(),
    );
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

    let checks = collect_file_checks(
        &profile,
        &[],
        std::path::Path::new("."),
        &ProviderRegistry::new(),
    );
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

    let checks = collect_system_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_system_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_package_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_package_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_package_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_package_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_package_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_system_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_system_checks(&profile, &[], &registry).unwrap();
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
    let checks = collect_system_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_system_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_system_checks(&profile, &[], &registry).unwrap();
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

    let checks = collect_file_checks(
        &profile,
        &[],
        std::path::Path::new("."),
        &ProviderRegistry::new(),
    );
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

    let checks = collect_file_checks(
        &profile,
        &[],
        std::path::Path::new("."),
        &ProviderRegistry::new(),
    );
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

// -----------------------------------------------------------------------
// Module-aware + content-aware collection
// -----------------------------------------------------------------------

use crate::modules::{ResolvedFile, ResolvedModule, ResolvedPackage};

/// An empty resolved module to fill in one resource kind per test.
fn empty_module(name: &str) -> ResolvedModule {
    ResolvedModule {
        name: name.to_string(),
        packages: Vec::new(),
        files: Vec::new(),
        env: Vec::new(),
        aliases: Vec::new(),
        system: HashMap::new(),
        pre_apply_scripts: Vec::new(),
        post_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        depends: Vec::new(),
        dir: std::path::PathBuf::from("/tmp/module"),
        platform_skip_reason: None,
        origin: None,
    }
}

#[test]
fn collect_file_checks_includes_module_file_and_attributes_origin() {
    // A module-deployed file present on disk must appear in the snapshot and be
    // attributed to its module in the detail.
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("mod-src.txt");
    std::fs::write(&source, "same").unwrap();
    let target = dir.path().join("mod-deployed.txt");
    std::fs::write(&target, "same").unwrap();

    let profile = MergedProfile::default();
    let mut m = empty_module("dev");
    m.files = vec![ResolvedFile {
        source,
        target: target.clone(),
        is_git_source: false,
        strategy: None,
        encryption: None,
        permissions: None,
    }];

    // No file_manager + no declared perms → exactly ONE check: the "present"
    // existence signal, attributed to its module.
    let checks = collect_file_checks(&profile, &[m], dir.path(), &ProviderRegistry::new());
    assert_eq!(
        checks.len(),
        1,
        "present + no-perms + no-file-manager must be exactly one check: {checks:?}"
    );
    assert_eq!(checks[0].category, "file");
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert_eq!(checks[0].detail.as_deref(), Some("present (module: dev)"));
}

#[test]
fn collect_file_checks_content_drift_is_violation() {
    // A managed file present on disk whose bytes drifted from the source is a
    // content violation when a file manager is wired.
    use crate::test_helpers::MockFileManager;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src.txt");
    std::fs::write(&source, "desired").unwrap();
    let target = dir.path().join("deployed.txt");
    std::fs::write(&target, "tampered").unwrap();

    let mut profile = MergedProfile::default();
    profile.files.managed = vec![crate::config::ManagedFileSpec {
        source: source.to_string_lossy().into_owned(),
        target: target.clone(),
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    }];

    let mut registry = ProviderRegistry::new();
    registry.file_manager = Some(Box::new(MockFileManager::new()));

    // file_manager wired + no declared perms → exactly ONE check (file-content);
    // the legacy "present" check is suppressed so existence isn't double-counted.
    let checks = collect_file_checks(&profile, &[], dir.path(), &registry);
    assert_eq!(
        checks.len(),
        1,
        "present + no-perms + file-manager must be exactly one check (no double-count): {checks:?}"
    );
    assert_eq!(checks[0].category, "file-content");
    assert_eq!(
        checks[0].status,
        ComplianceStatus::Violation,
        "tampered content must be a violation: {checks:?}"
    );
    assert!(checks[0].detail.as_deref().unwrap().contains("differs"));
}

#[test]
fn collect_file_checks_content_match_is_compliant() {
    use crate::test_helpers::MockFileManager;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src.txt");
    std::fs::write(&source, "same").unwrap();
    let target = dir.path().join("deployed.txt");
    std::fs::write(&target, "same").unwrap();

    let mut profile = MergedProfile::default();
    profile.files.managed = vec![crate::config::ManagedFileSpec {
        source: source.to_string_lossy().into_owned(),
        target: target.clone(),
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    }];

    let mut registry = ProviderRegistry::new();
    registry.file_manager = Some(Box::new(MockFileManager::new()));

    // file_manager wired + no declared perms → exactly ONE Compliant file-content
    // check; no duplicate "present" row.
    let checks = collect_file_checks(&profile, &[], dir.path(), &registry);
    assert_eq!(
        checks.len(),
        1,
        "present + no-perms + file-manager must be exactly one check: {checks:?}"
    );
    assert_eq!(checks[0].category, "file-content");
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
}

#[cfg(unix)]
#[test]
fn collect_file_checks_content_plus_perms_is_two_checks() {
    // file_manager wired + declared perms → exactly TWO checks: the content check
    // (existence + bytes) and the permissions check (a distinct concern). They are
    // not mutually exclusive — only the redundant "present" signal is suppressed.
    use crate::test_helpers::MockFileManager;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src.txt");
    std::fs::write(&source, "same").unwrap();
    let target = dir.path().join("deployed.txt");
    std::fs::write(&target, "same").unwrap();
    crate::set_file_permissions(&target, 0o600).unwrap();

    let mut profile = MergedProfile::default();
    profile.files.managed = vec![crate::config::ManagedFileSpec {
        source: source.to_string_lossy().into_owned(),
        target: target.clone(),
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: Some("600".into()),
    }];

    let mut registry = ProviderRegistry::new();
    registry.file_manager = Some(Box::new(MockFileManager::new()));

    let checks = collect_file_checks(&profile, &[], dir.path(), &registry);
    assert_eq!(
        checks.len(),
        2,
        "present + perms + file-manager must be exactly two checks: {checks:?}"
    );
    assert!(
        checks.iter().any(|c| c.category == "file-content"),
        "expected a content check: {checks:?}"
    );
    assert!(
        checks
            .iter()
            .any(|c| c.category == "file" && c.detail.as_deref() == Some("permissions 0o600")),
        "expected a permissions check: {checks:?}"
    );
}

#[test]
fn collect_package_checks_includes_module_only_package() {
    // A module-only package the host's available manager lacks appears as a
    // violation, attributed to its module.
    use crate::providers::StubPackageManager;

    let profile = MergedProfile::default();
    let mut m = empty_module("dev");
    m.packages = vec![ResolvedPackage {
        canonical_name: "ripgrep".into(),
        resolved_name: "ripgrep".into(),
        manager: "pipx".into(),
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
    }];

    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        StubPackageManager::new("pipx").with_installed(&[]),
    ));

    let checks = collect_package_checks(&profile, &[m], &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name.as_deref(), Some("ripgrep"));
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert_eq!(
        checks[0].detail.as_deref(),
        Some("not installed (module: dev)")
    );
}

#[test]
fn collect_package_checks_skips_unavailable_manager() {
    // A module package whose manager is not in the registry is skipped (host-
    // agnostic desired set intersected with available managers).
    let profile = MergedProfile::default();
    let mut m = empty_module("dev");
    m.packages = vec![ResolvedPackage {
        canonical_name: "fd".into(),
        resolved_name: "fd".into(),
        manager: "brew".into(),
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
    }];

    let registry = ProviderRegistry::new();
    let checks = collect_package_checks(&profile, &[m], &registry).unwrap();
    assert!(
        checks.is_empty(),
        "package for an unavailable manager must be skipped: {checks:?}"
    );
}

#[test]
fn collect_system_checks_includes_module_only_tweak() {
    // A system tweak declared ONLY in a module must surface, proving the system
    // map combines module config.
    use crate::providers::SystemDrift;
    use crate::test_helpers::MockSystemConfigurator;

    let profile = MergedProfile::default();
    let mut m = empty_module("dev");
    m.system.insert(
        "sysctl".to_string(),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    );

    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
            vec![SystemDrift {
                key: "vm.swappiness".into(),
                expected: "10".into(),
                actual: "60".into(),
            }],
        )));

    let checks = collect_system_checks(&profile, &[m], &registry).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert_eq!(checks[0].key.as_deref(), Some("sysctl.vm.swappiness"));
}

#[test]
fn collect_snapshot_includes_module_resources_and_content_check() {
    // Ground-truth end-to-end test of the full collector: a profile that declares
    // NOTHING, plus a module contributing one (content-matching) file, one
    // not-installed package, and one drifting system tweak. Asserts the real
    // snapshot output — module attribution, content-awareness, and summary counts.
    use crate::config::ComplianceScope;
    use crate::providers::{StubPackageManager, SystemDrift};
    use crate::test_helpers::{MockFileManager, MockSystemConfigurator};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("mod-src.txt");
    std::fs::write(&source, "same").unwrap();
    let target = dir.path().join("mod-deployed.txt");
    std::fs::write(&target, "same").unwrap();

    let profile = MergedProfile::default();

    let mut m = empty_module("dev");
    m.files = vec![ResolvedFile {
        source,
        target: target.clone(),
        is_git_source: false,
        strategy: None,
        encryption: None,
        permissions: None,
    }];
    m.packages = vec![ResolvedPackage {
        canonical_name: "ripgrep".into(),
        resolved_name: "ripgrep".into(),
        manager: "pipx".into(),
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
    }];
    m.system.insert(
        "sysctl".to_string(),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    );

    let mut registry = ProviderRegistry::new();
    registry.file_manager = Some(Box::new(MockFileManager::new()));
    registry.package_managers.push(Box::new(
        StubPackageManager::new("pipx").with_installed(&[]),
    ));
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
            vec![SystemDrift {
                key: "vm.swappiness".into(),
                expected: "10".into(),
                actual: "60".into(),
            }],
        )));

    let snapshot = collect_snapshot(
        "default",
        &profile,
        &[m],
        dir.path(),
        &registry,
        &ComplianceScope::default(),
        &["local".to_string()],
    )
    .unwrap();

    // Module-only file: content-matching → one Compliant file-content check,
    // attributed to its module.
    let file_check = snapshot
        .checks
        .iter()
        .find(|c| c.category == "file-content")
        .expect("module file content check must appear");
    assert_eq!(file_check.status, ComplianceStatus::Compliant);
    assert_eq!(
        file_check.detail.as_deref(),
        Some("content matches source (module: dev)")
    );

    // Module-only package: not installed → Violation, attributed to its module.
    let pkg_check = snapshot
        .checks
        .iter()
        .find(|c| c.category == "package" && c.name.as_deref() == Some("ripgrep"))
        .expect("module package check must appear");
    assert_eq!(pkg_check.status, ComplianceStatus::Violation);
    assert_eq!(
        pkg_check.detail.as_deref(),
        Some("not installed (module: dev)")
    );

    // Module-only system tweak: drift → Violation.
    let sys_check = snapshot
        .checks
        .iter()
        .find(|c| c.category == "system" && c.key.as_deref() == Some("sysctl.vm.swappiness"))
        .expect("module system check must appear");
    assert_eq!(sys_check.status, ComplianceStatus::Violation);

    // Exactly three checks total (file-content + package + system); no secrets,
    // no watch paths, and the file check is not double-counted.
    assert_eq!(
        snapshot.checks.len(),
        3,
        "expected exactly three checks: {:?}",
        snapshot.checks
    );
    assert_eq!(snapshot.summary.compliant, 1);
    assert_eq!(snapshot.summary.warning, 0);
    assert_eq!(snapshot.summary.violation, 2);
}
