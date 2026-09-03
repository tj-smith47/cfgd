use super::*;

use std::collections::{BTreeMap, HashMap};

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
                patch: None,
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
        "test",
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
                patch: None,
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
        "test",
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
                patch: None,
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
        "test",
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
                patch: None,
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
        "test",
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
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("shell").with_drift(
        vec![SystemDrift {
            key: "defaultShell".into(),
            expected: "/bin/zsh".into(),
            actual: "/bin/bash".into(),
        }],
    )));

    let mut system = BTreeMap::new();
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

/// Pins `system_checks_from_diffs`' `detail` to its stored byte shape.
/// `ComplianceCheck` is serialized into the `-o json` payload and into
/// `compliance_snapshots.snapshot_json`, whose content hash covers `detail`
/// (see `snapshot_content_hash`), so this string is a persisted/hashed
/// value, not a display string: a display-side spelling like
/// `crate::output::drift_detail`'s `want: …, have: …` must never land here,
/// because every stored machine's system-violation snapshot would re-hash on
/// upgrade with nothing having actually changed.
#[test]
fn system_checks_from_diffs_pins_the_persisted_detail_shape() {
    let diffs = vec![SystemDiff {
        configurator: "shell".into(),
        outcome: SystemDiffOutcome::Drifts(vec![SystemDrift {
            key: "defaultShell".into(),
            expected: "/bin/zsh".into(),
            actual: "/bin/bash".into(),
        }]),
    }];

    let checks = system_checks_from_diffs(&diffs);
    assert_eq!(checks.len(), 1);
    assert_eq!(
        checks[0].detail.as_deref(),
        Some("expected /bin/zsh, actual /bin/bash")
    );
}

#[test]
fn collect_system_checks_compliant_when_no_drift() {
    use crate::providers::ProviderRegistry;
    use crate::test_helpers::MockSystemConfigurator;

    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("shell")));

    let mut system = BTreeMap::new();
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
    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_watched_package_manager_checks("nonexistent-pm", &registry, &cx).unwrap();
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
    registry.add_package_manager(Box::new(
        StubPackageManager::new("mock").with_installed(&["ripgrep", "fd"]),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_watched_package_manager_checks("mock", &registry, &cx).unwrap();
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

// A snapshot that both declares packages under a manager and watches that same
// manager used to enumerate it once per section. One context makes the two
// sections read one listing.
#[test]
#[serial_test::serial(enumeration_memo)]
fn a_declared_and_watched_manager_is_enumerated_once_per_snapshot() {
    use crate::config::MergedProfile;

    // The count is a memo-hit claim, so the memo's age ceiling is pinned out of
    // reach and the pin is serialized — it is process-global, and a sibling test
    // pins it to zero.
    let _ttl = crate::test_helpers::EnumerationMemoTtlGuard::never_expires();

    let enumerations = crate::test_helpers::measured_in_a_stable_generation(|| {
        let mgr =
            crate::test_helpers::MockPackageManager::new("pipx").with_installed(&["ripgrep", "fd"]);
        let enumerations = mgr.enumeration_counter();
        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(mgr));

        let mut profile = MergedProfile::default();
        profile.packages.pipx = vec!["ripgrep".into(), "fd".into()];

        let printer = crate::test_helpers::test_printer();
        let state = crate::test_helpers::test_state();
        let cx = crate::providers::PackageContext::new(&printer, &state);

        collect_package_checks(&profile, &[], &registry, &cx).unwrap();
        collect_watched_package_manager_checks("pipx", &registry, &cx).unwrap();

        enumerations.load(std::sync::atomic::Ordering::SeqCst)
    });

    assert_eq!(
        enumerations, 1,
        "the declared and watched sections must share one enumeration"
    );
}

#[test]
fn collect_package_checks_installed_package_compliant() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    let mut profile = MergedProfile::default();
    // Use pipx (Vec<String>) which is simpler to construct
    profile.packages.pipx = vec!["ripgrep".into()];

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        StubPackageManager::new("pipx").with_installed(&["ripgrep"]),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[], &registry, &cx).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert_eq!(checks[0].name.as_deref(), Some("ripgrep"));
    assert_eq!(checks[0].manager.as_deref(), Some("pipx"));
}

#[test]
fn collect_package_checks_routes_through_package_identity_for_case_insensitive_manager() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    // The profile desires `Wget` (as authored); chocolatey's installed set is
    // folded to `wget` (parse_choco_list lowercases). Compliance must compare
    // through package_identity, else a compliant package reads as a violation on
    // every snapshot. Reverting the identity wire at collect_package_checks turns
    // this red.
    let mut profile = MergedProfile::default();
    profile.packages.chocolatey = vec!["Wget".into()];

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        StubPackageManager::new("chocolatey")
            .case_folding()
            .with_installed(&["wget"]),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[], &registry, &cx).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(
        checks[0].status,
        ComplianceStatus::Compliant,
        "desired `Wget` must match folded installed `wget`: {checks:?}"
    );
}

/// A snapshot is what an auditor reads when nobody is at the terminal, so a
/// package installed BELOW its declared floor cannot read `installed` there
/// while `cfgd verify` calls the same machine drifted. The floor verdict comes
/// from the one engine (`package_version_floor`); only the vocabulary is
/// compliance's own — a missed floor is a Violation naming both operands, an
/// unjudgeable one a Warning, because a check that could not run is not a
/// finding against the host.
#[test]
fn a_package_below_its_declared_floor_is_a_compliance_violation() {
    use crate::config::MergedProfile;
    use crate::modules::ResolvedPackage;
    use crate::providers::StubPackageManager;

    let pinned = |pkg: &str, min: &str| {
        let mut m = crate::test_helpers::make_resolved_module("dev");
        m.packages = vec![ResolvedPackage {
            canonical_name: pkg.to_string(),
            resolved_name: pkg.to_string(),
            manager: "pipx".to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: Some(min.to_string()),
        }];
        m
    };

    let registry = |mgr: StubPackageManager| {
        let mut r = ProviderRegistry::new();
        r.add_package_manager(Box::new(mgr));
        r
    };
    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();

    let below = registry(StubPackageManager::new("pipx").with_installed_at("ripgrep", "1.0.0"));
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(
        &MergedProfile::default(),
        &[pinned("ripgrep", "2")],
        &below,
        &cx,
    )
    .unwrap();
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].status, ComplianceStatus::Violation, "{checks:?}");
    let detail = checks[0].detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("1.0.0") && detail.contains('2'),
        "the detail states both operands: {detail}"
    );

    let unjudgeable =
        registry(StubPackageManager::new("pipx").with_installed_at("ripgrep", "git-20240101"));
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(
        &MergedProfile::default(),
        &[pinned("ripgrep", "2")],
        &unjudgeable,
        &cx,
    )
    .unwrap();
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(
        checks[0].status,
        ComplianceStatus::Warning,
        "an unjudgeable version is a check that could not run: {checks:?}"
    );
}

#[test]
fn collect_package_checks_missing_package_violation() {
    use crate::config::MergedProfile;
    use crate::providers::StubPackageManager;

    let mut profile = MergedProfile::default();
    profile.packages.pipx = vec!["missing-pkg".into()];

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        StubPackageManager::new("pipx").with_installed(&[]),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[], &registry, &cx).unwrap();
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
    registry.add_package_manager(Box::new(
        StubPackageManager::new("pipx").with_installed(&["curl"]),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[], &registry, &cx).unwrap();
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
    registry.add_package_manager(Box::new(
        StubPackageManager::new("pipx").with_installed_error("permission denied: /var/lib/pipx"),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[], &registry, &cx).unwrap();
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
    registry.add_package_manager(Box::new(
        StubPackageManager::new("snap")
            .with_installed_error("snapd not responding (no such file or directory)"),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_watched_package_manager_checks("snap", &registry, &cx).unwrap();
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
    registry.add_package_manager(Box::new(
        StubPackageManager::new("pipx").with_installed(&["ripgrep"]),
    ));
    registry.add_package_manager(Box::new(StubPackageManager::new("dnf").with_installed(&[])));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[], &registry, &cx).unwrap();
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
        _cx: &crate::providers::SystemContext<'_>,
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
    registry.add_system_configurator(Box::new(InlineSystemMock {
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
    registry.add_system_configurator(Box::new(InlineSystemMock {
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
    registry.add_system_configurator(Box::new(InlineSystemMock {
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
    registry.add_system_configurator(Box::new(InlineSystemMock {
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

/// A configurator that answers like [`InlineSystemMock`] and counts how many
/// times it was asked. The count IS the claim of the two tests below: a checkin
/// diffs the machine for its compliance snapshot and again for its drift
/// report, and every ask is whatever the real configurator spawns.
struct CountingConfigurator {
    diffs: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::providers::SystemConfigurator for CountingConfigurator {
    fn name(&self) -> &str {
        "mock"
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
        self.diffs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(vec![crate::providers::SystemDrift {
            key: "net.ipv4.ip_forward".into(),
            expected: "1".into(),
            actual: "0".into(),
        }])
    }
    fn apply(
        &self,
        _desired: &serde_yaml::Value,
        _cx: &crate::providers::SystemContext<'_>,
    ) -> crate::errors::Result<()> {
        Ok(())
    }
}

fn counting_registry() -> (
    ProviderRegistry,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let diffs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(CountingConfigurator {
        diffs: std::sync::Arc::clone(&diffs),
    }));
    (registry, diffs)
}

fn counting_profile() -> crate::config::MergedProfile {
    let mut profile = crate::config::MergedProfile::default();
    profile.system.insert(
        "mock".to_string(),
        serde_yaml::Value::String("desired".into()),
    );
    profile
}

#[test]
fn a_snapshot_handed_collected_diffs_asks_no_configurator_again() {
    let (registry, diffs) = counting_registry();
    let profile = counting_profile();
    let dir = tempfile::tempdir().unwrap();
    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();

    let collected = collect_system_diffs(&profile, &[], &registry);
    assert_eq!(diffs.load(std::sync::atomic::Ordering::SeqCst), 1);

    let snapshot = collect_snapshot(
        "default",
        &profile,
        &[],
        dir.path(),
        &registry,
        &ComplianceScope::default(),
        &[],
        &printer,
        &state,
        Some(&collected),
    )
    .unwrap();

    assert_eq!(
        diffs.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the snapshot re-diffed a machine it was handed the answers for"
    );
    // And the drift report derived from the same answers names the same drift.
    let drifts = system_drifts(&collected);
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].0, "mock");
    assert_eq!(drifts[0].1.key, "net.ipv4.ip_forward");
    assert_eq!(diffs.load(std::sync::atomic::Ordering::SeqCst), 1);

    // The check it renders is the one the un-handed path renders.
    let system_checks: Vec<_> = snapshot
        .checks
        .iter()
        .filter(|c| c.category == "system")
        .collect();
    assert_eq!(system_checks.len(), 1);
    assert_eq!(system_checks[0].status, ComplianceStatus::Violation);
    assert_eq!(
        system_checks[0].key.as_deref(),
        Some("mock.net.ipv4.ip_forward")
    );
}

#[test]
fn collected_diffs_render_exactly_what_collecting_inside_the_snapshot_renders() {
    let (registry, diffs) = counting_registry();
    let profile = counting_profile();

    let from_collected = system_checks_from_diffs(&collect_system_diffs(&profile, &[], &registry));
    let inline = collect_system_checks(&profile, &[], &registry).unwrap();

    assert_eq!(diffs.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        serde_json::to_value(&from_collected).unwrap(),
        serde_json::to_value(&inline).unwrap(),
        "reusing collected diffs must not change a single stored check"
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
                patch: None,
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
        "test",
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
                patch: None,
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
        "test",
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
        dep_pulled: false,
        name: name.to_string(),
        packages: Vec::new(),
        files: Vec::new(),
        env: Vec::new(),
        aliases: Vec::new(),
        system: BTreeMap::new(),
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
        patch: None,
    }];

    // No file_manager + no declared perms → exactly ONE check: the "present"
    // existence signal, attributed to its module.
    let checks = collect_file_checks("test", &profile, &[m], dir.path(), &ProviderRegistry::new());
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
fn collect_file_checks_patch_reports_content_convergence() {
    // A `Patch` entry has no source to compare against — its content check
    // comes from re-evaluating the merge over the target, with no file manager
    // wired.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("settings.json");
    std::fs::write(&target, "{\n  \"telemetry\": false\n}\n").unwrap();

    let profile = patch_profile(&target, "telemetry: false");
    let checks = collect_file_checks("test", &profile, &[], dir.path(), &ProviderRegistry::new());

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].category, "file-content");
    assert_eq!(checks[0].status, ComplianceStatus::Compliant);
    assert_eq!(
        checks[0].detail.as_deref(),
        Some("content satisfies patch spec")
    );
}

#[test]
fn collect_file_checks_patch_drift_is_violation() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("settings.json");
    std::fs::write(&target, "{\n  \"telemetry\": true\n}\n").unwrap();

    let profile = patch_profile(&target, "telemetry: false");
    let checks = collect_file_checks("test", &profile, &[], dir.path(), &ProviderRegistry::new());

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert_eq!(
        checks[0].detail.as_deref(),
        Some("content differs from patch spec")
    );
}

#[test]
fn collect_file_checks_patch_unparseable_target_warns() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("settings.json");
    std::fs::write(&target, "not json at all").unwrap();

    let profile = patch_profile(&target, "telemetry: false");
    let checks = collect_file_checks("test", &profile, &[], dir.path(), &ProviderRegistry::new());

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Warning);
    assert!(
        checks[0]
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("cannot evaluate patch spec:")),
        "expected an evaluation warning, got: {:?}",
        checks[0].detail
    );
}

/// Profile with a single `Patch` managed file over `target`.
fn patch_profile(target: &std::path::Path, ensure: &str) -> MergedProfile {
    MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![crate::config::ManagedFileSpec {
                patch: Some(crate::config::PatchSpec {
                    format: None,
                    ensure: Some(serde_yaml::from_str(ensure).unwrap()),
                    script: None,
                    blocked_by: None,
                }),
                source: String::new(),
                target: target.to_path_buf(),
                strategy: Some(crate::config::FileStrategy::Patch),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        },
        ..Default::default()
    }
}

#[test]
fn collect_file_checks_module_patch_attributes_origin() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("settings.json");
    std::fs::write(&target, "{\n  \"telemetry\": true\n}\n").unwrap();

    let profile = MergedProfile::default();
    let mut m = empty_module("dev");
    m.files = vec![ResolvedFile {
        source: PathBuf::new(),
        target: target.clone(),
        is_git_source: false,
        strategy: Some(crate::config::FileStrategy::Patch),
        encryption: None,
        permissions: None,
        patch: Some(crate::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str("telemetry: false").unwrap()),
            script: None,
            blocked_by: None,
        }),
    }];

    let checks = collect_file_checks("test", &profile, &[m], dir.path(), &ProviderRegistry::new());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, ComplianceStatus::Violation);
    assert_eq!(
        checks[0].detail.as_deref(),
        Some("content differs from patch spec (module: dev)")
    );
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
        patch: None,
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
    let checks = collect_file_checks("test", &profile, &[], dir.path(), &registry);
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
        patch: None,
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
    let checks = collect_file_checks("test", &profile, &[], dir.path(), &registry);
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
        patch: None,
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

    let checks = collect_file_checks("test", &profile, &[], dir.path(), &registry);
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
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    }];

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        StubPackageManager::new("pipx").with_installed(&[]),
    ));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[m], &registry, &cx).unwrap();
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
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    }];

    let registry = ProviderRegistry::new();
    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let checks = collect_package_checks(&profile, &[m], &registry, &cx).unwrap();
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
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
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
        patch: None,
    }];
    m.packages = vec![ResolvedPackage {
        canonical_name: "ripgrep".into(),
        resolved_name: "ripgrep".into(),
        manager: "pipx".into(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    }];
    m.system.insert(
        "sysctl".to_string(),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    );

    let mut registry = ProviderRegistry::new();
    registry.file_manager = Some(Box::new(MockFileManager::new()));
    registry.add_package_manager(Box::new(
        StubPackageManager::new("pipx").with_installed(&[]),
    ));
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
        vec![SystemDrift {
            key: "vm.swappiness".into(),
            expected: "10".into(),
            actual: "60".into(),
        }],
    )));

    let printer = crate::test_helpers::test_printer();
    let state = crate::test_helpers::test_state();
    let snapshot = collect_snapshot(
        "default",
        &profile,
        &[m],
        dir.path(),
        &registry,
        &ComplianceScope::default(),
        &["local".to_string()],
        &printer,
        &state,
        None,
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

#[test]
fn a_fixed_snapshot_hashes_to_a_pinned_digest() {
    // The canonical form `snapshot_json_content_hash` normalizes to depends on
    // `serde_json::Map` being a BTreeMap, which holds only while nothing in the
    // dependency graph enables `serde_json/preserve_order`. Feature unification
    // is global, so a dep bump three crates away could flip it and silently
    // change what every stored `content_hash` means. Pinning one digest turns
    // that into a failing test instead of one spurious "changed" snapshot.
    let snapshot = ComplianceSnapshot {
        timestamp: "2026-03-25T00:00:00Z".into(),
        machine: MachineInfo {
            hostname: "test-host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![ComplianceCheck {
            category: "system".into(),
            key: Some("sysctl.vm.swappiness".into()),
            status: ComplianceStatus::Violation,
            detail: Some("want 10, have 60".into()),
            ..Default::default()
        }],
        summary: ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 1,
        },
    };

    let (_, hash) = snapshot_content_hash(&snapshot).unwrap();
    assert_eq!(
        hash, "2a1c0cef36205ca80c5ea9b03601d9f79a8a4aec020e3d554d5f741a9ea90094",
        "the canonical form moved — check whether a dependency enabled \
         serde_json/preserve_order, or whether a serialized field was added to \
         ComplianceSnapshot/ComplianceCheck (stored hashes change meaning either way)"
    );

    // The timestamp is excluded, so restamping the same content cannot move it.
    let mut later = snapshot.clone();
    later.timestamp = "2099-12-31T23:59:59Z".into();
    assert_eq!(snapshot_content_hash(&later).unwrap().1, hash);
}
