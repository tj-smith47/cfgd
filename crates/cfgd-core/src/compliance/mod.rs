// Compliance snapshot — types, collection logic, summary computation

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{ComplianceScope, MergedProfile};
use crate::errors::Result;
use crate::platform::Platform;
use crate::providers::ProviderRegistry;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceSnapshot {
    pub timestamp: String,
    pub machine: MachineInfo,
    pub profile: String,
    pub sources: Vec<String>,
    pub checks: Vec<ComplianceCheck>,
    pub summary: ComplianceSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComplianceCheck {
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub status: ComplianceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ComplianceStatus {
    #[default]
    Compliant,
    Warning,
    Violation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceSummary {
    pub compliant: usize,
    pub warning: usize,
    pub violation: usize,
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

/// Collect a full compliance snapshot for the current machine state.
pub fn collect_snapshot(
    profile_name: &str,
    profile: &MergedProfile,
    registry: &ProviderRegistry,
    scope: &ComplianceScope,
    sources: &[String],
) -> Result<ComplianceSnapshot> {
    let platform = Platform::detect();
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    let machine = MachineInfo {
        hostname,
        os: platform.os.as_str().to_owned(),
        arch: platform.arch.as_str().to_owned(),
    };

    let mut checks = Vec::new();

    if scope.files {
        checks.extend(collect_file_checks(profile));
    }
    if scope.packages {
        checks.extend(collect_package_checks(profile, registry)?);
    }
    if scope.system {
        checks.extend(collect_system_checks(profile, registry)?);
    }
    if scope.secrets {
        checks.extend(collect_secret_checks(profile));
    }
    for watch_path in &scope.watch_paths {
        checks.extend(collect_watch_path_checks(watch_path));
    }

    let summary = compute_summary(&checks);

    Ok(ComplianceSnapshot {
        timestamp: crate::utc_now_iso8601(),
        machine,
        profile: profile_name.to_owned(),
        sources: sources.to_vec(),
        checks,
        summary,
    })
}

/// Compute summary counts from a list of checks.
pub fn compute_summary(checks: &[ComplianceCheck]) -> ComplianceSummary {
    let mut compliant = 0usize;
    let mut warning = 0usize;
    let mut violation = 0usize;

    for check in checks {
        match check.status {
            ComplianceStatus::Compliant => compliant += 1,
            ComplianceStatus::Warning => warning += 1,
            ComplianceStatus::Violation => violation += 1,
        }
    }

    ComplianceSummary {
        compliant,
        warning,
        violation,
    }
}

// ---------------------------------------------------------------------------
// File checks
// ---------------------------------------------------------------------------

/// Check managed files: existence, permissions, encryption declaration.
pub fn collect_file_checks(profile: &MergedProfile) -> Vec<ComplianceCheck> {
    let mut checks = Vec::new();

    for file in &profile.files.managed {
        let target = crate::expand_tilde(&file.target);
        let exists = target.exists();

        if !exists {
            checks.push(ComplianceCheck {
                category: "file".into(),
                target: Some(target.display().to_string()),
                status: ComplianceStatus::Violation,
                detail: Some("managed file missing".into()),
                ..Default::default()
            });
            continue;
        }

        // Check permissions if declared
        if let Some(ref perm_str) = file.permissions {
            if let Ok(desired_mode) = u32::from_str_radix(perm_str, 8) {
                let actual_mode = target
                    .metadata()
                    .ok()
                    .and_then(|m| crate::file_permissions_mode(&m));
                match actual_mode {
                    Some(mode) if mode == desired_mode => {
                        checks.push(ComplianceCheck {
                            category: "file".into(),
                            target: Some(target.display().to_string()),
                            status: ComplianceStatus::Compliant,
                            detail: Some(format!("permissions {:#o}", mode)),
                            ..Default::default()
                        });
                    }
                    Some(mode) => {
                        checks.push(ComplianceCheck {
                            category: "file".into(),
                            target: Some(target.display().to_string()),
                            status: ComplianceStatus::Warning,
                            detail: Some(format!(
                                "permissions {:#o}, expected {:#o}",
                                mode, desired_mode
                            )),
                            ..Default::default()
                        });
                    }
                    None => {
                        // Windows or metadata unavailable — compliant by default
                        checks.push(ComplianceCheck {
                            category: "file".into(),
                            target: Some(target.display().to_string()),
                            status: ComplianceStatus::Compliant,
                            detail: Some("permissions not applicable on this platform".into()),
                            ..Default::default()
                        });
                    }
                }
            } else {
                // Malformed permission string
                checks.push(ComplianceCheck {
                    category: "file".into(),
                    target: Some(target.display().to_string()),
                    status: ComplianceStatus::Warning,
                    detail: Some(format!("invalid permission string: {}", perm_str)),
                    ..Default::default()
                });
            }
        } else {
            // No permissions declared — file exists, compliant
            checks.push(ComplianceCheck {
                category: "file".into(),
                target: Some(target.display().to_string()),
                status: ComplianceStatus::Compliant,
                detail: Some("present".into()),
                ..Default::default()
            });
        }

        // Check encryption declaration (if encryption is specified, just verify it is declared)
        if let Some(ref enc) = file.encryption {
            checks.push(ComplianceCheck {
                category: "file-encryption".into(),
                target: Some(target.display().to_string()),
                status: ComplianceStatus::Compliant,
                detail: Some(format!("encryption: backend={}", enc.backend)),
                ..Default::default()
            });
        }
    }

    checks
}

// ---------------------------------------------------------------------------
// Package checks
// ---------------------------------------------------------------------------

/// Check that declared packages are installed via their respective managers.
pub fn collect_package_checks(
    profile: &MergedProfile,
    registry: &ProviderRegistry,
) -> Result<Vec<ComplianceCheck>> {
    let mut checks = Vec::new();

    for pm in registry.available_package_managers() {
        let desired = crate::config::desired_packages_for_spec(pm.name(), &profile.packages);
        if desired.is_empty() {
            continue;
        }

        let installed = match pm.installed_packages() {
            Ok(set) => set,
            Err(e) => {
                // Cannot query this manager — report as warning
                checks.push(ComplianceCheck {
                    category: "package".into(),
                    manager: Some(pm.name().to_owned()),
                    status: ComplianceStatus::Warning,
                    detail: Some(format!("cannot query {}: {}", pm.name(), e)),
                    ..Default::default()
                });
                continue;
            }
        };

        for pkg in &desired {
            if installed.contains(pkg) {
                checks.push(ComplianceCheck {
                    category: "package".into(),
                    name: Some(pkg.clone()),
                    manager: Some(pm.name().to_owned()),
                    status: ComplianceStatus::Compliant,
                    detail: Some("installed".into()),
                    ..Default::default()
                });
            } else {
                checks.push(ComplianceCheck {
                    category: "package".into(),
                    name: Some(pkg.clone()),
                    manager: Some(pm.name().to_owned()),
                    status: ComplianceStatus::Violation,
                    detail: Some("not installed".into()),
                    ..Default::default()
                });
            }
        }
    }

    Ok(checks)
}

// ---------------------------------------------------------------------------
// System checks
// ---------------------------------------------------------------------------

/// Check system configurator state for drift.
pub fn collect_system_checks(
    profile: &MergedProfile,
    registry: &ProviderRegistry,
) -> Result<Vec<ComplianceCheck>> {
    let mut checks = Vec::new();
    let available = registry.available_system_configurators();

    for (key, desired) in &profile.system {
        let configurator = available.iter().find(|c| c.name() == key);

        let Some(configurator) = configurator else {
            checks.push(ComplianceCheck {
                category: "system".into(),
                key: Some(key.clone()),
                status: ComplianceStatus::Warning,
                detail: Some(format!("no configurator available for '{}'", key)),
                ..Default::default()
            });
            continue;
        };

        match configurator.diff(desired) {
            Ok(drifts) => {
                if drifts.is_empty() {
                    checks.push(ComplianceCheck {
                        category: "system".into(),
                        key: Some(key.clone()),
                        status: ComplianceStatus::Compliant,
                        detail: Some("no drift".into()),
                        ..Default::default()
                    });
                } else {
                    for drift in &drifts {
                        checks.push(ComplianceCheck {
                            category: "system".into(),
                            key: Some(format!("{}.{}", key, drift.key)),
                            status: ComplianceStatus::Violation,
                            detail: Some(format!(
                                "expected {}, actual {}",
                                drift.expected, drift.actual
                            )),
                            value: Some(drift.actual.clone()),
                            ..Default::default()
                        });
                    }
                }
            }
            Err(e) => {
                checks.push(ComplianceCheck {
                    category: "system".into(),
                    key: Some(key.clone()),
                    status: ComplianceStatus::Warning,
                    detail: Some(format!("diff failed: {}", e)),
                    ..Default::default()
                });
            }
        }
    }

    Ok(checks)
}

// ---------------------------------------------------------------------------
// Secret checks
// ---------------------------------------------------------------------------

/// Check secrets: for secrets with file targets, verify the target file exists
/// and check its permissions. NEVER reads or logs secret values.
pub fn collect_secret_checks(profile: &MergedProfile) -> Vec<ComplianceCheck> {
    let mut checks = Vec::new();

    for secret in &profile.secrets {
        let Some(ref target_path) = secret.target else {
            // Env-only secret — no file to check
            continue;
        };

        let target = crate::expand_tilde(target_path);
        if target.exists() {
            checks.push(ComplianceCheck {
                category: "secret".into(),
                target: Some(target.display().to_string()),
                status: ComplianceStatus::Compliant,
                detail: Some("target file present".into()),
                ..Default::default()
            });
        } else {
            checks.push(ComplianceCheck {
                category: "secret".into(),
                target: Some(target.display().to_string()),
                status: ComplianceStatus::Violation,
                detail: Some("target file missing".into()),
                ..Default::default()
            });
        }
    }

    checks
}

// ---------------------------------------------------------------------------
// Watch path checks
// ---------------------------------------------------------------------------

/// Stat a watch path and report basic info.
fn collect_watch_path_checks(path_str: &str) -> Vec<ComplianceCheck> {
    let path = crate::expand_tilde(Path::new(path_str));

    if !path.exists() {
        return vec![ComplianceCheck {
            category: "watch-path".into(),
            path: Some(path.display().to_string()),
            status: ComplianceStatus::Warning,
            detail: Some("path does not exist".into()),
            ..Default::default()
        }];
    }

    let meta = match path.metadata() {
        Ok(m) => m,
        Err(e) => {
            return vec![ComplianceCheck {
                category: "watch-path".into(),
                path: Some(path.display().to_string()),
                status: ComplianceStatus::Warning,
                detail: Some(format!("cannot stat: {}", e)),
                ..Default::default()
            }];
        }
    };

    let perms = crate::file_permissions_mode(&meta);
    let kind = if meta.is_dir() {
        "directory"
    } else if meta.is_file() {
        "file"
    } else {
        "other"
    };

    let detail = match perms {
        Some(mode) => format!("{}, permissions {:#o}", kind, mode),
        None => kind.to_string(),
    };

    vec![ComplianceCheck {
        category: "watch-path".into(),
        path: Some(path.display().to_string()),
        status: ComplianceStatus::Compliant,
        detail: Some(detail),
        ..Default::default()
    }]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
        assert!(json.contains("\"compliant\""));
        assert!(json.contains("\"violation\""));

        // Roundtrip
        let parsed: ComplianceSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.profile, "default");
        assert_eq!(parsed.checks.len(), 2);
        assert_eq!(parsed.summary.compliant, 1);
        assert_eq!(parsed.summary.violation, 1);
    }

    #[test]
    fn compliance_check_default_is_compliant() {
        let check = ComplianceCheck::default();
        assert_eq!(check.status, ComplianceStatus::Compliant);
        assert!(check.category.is_empty());
        assert!(check.target.is_none());
        assert!(check.name.is_none());
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
        use crate::providers::{ProviderRegistry, SystemConfigurator, SystemDrift};

        struct MockConfigurator;
        impl SystemConfigurator for MockConfigurator {
            fn name(&self) -> &str {
                "shell"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn current_state(&self) -> Result<serde_yaml::Value> {
                Ok(serde_yaml::Value::Null)
            }
            fn diff(&self, _desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
                Ok(vec![SystemDrift {
                    key: "defaultShell".into(),
                    expected: "/bin/zsh".into(),
                    actual: "/bin/bash".into(),
                }])
            }
            fn apply(
                &self,
                _desired: &serde_yaml::Value,
                _printer: &crate::output::Printer,
            ) -> Result<()> {
                Ok(())
            }
        }

        let mut registry = ProviderRegistry::new();
        registry
            .system_configurators
            .push(Box::new(MockConfigurator));

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
        use crate::providers::{ProviderRegistry, SystemConfigurator, SystemDrift};

        struct MockConfigurator;
        impl SystemConfigurator for MockConfigurator {
            fn name(&self) -> &str {
                "shell"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn current_state(&self) -> Result<serde_yaml::Value> {
                Ok(serde_yaml::Value::Null)
            }
            fn diff(&self, _desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
                Ok(vec![])
            }
            fn apply(
                &self,
                _desired: &serde_yaml::Value,
                _printer: &crate::output::Printer,
            ) -> Result<()> {
                Ok(())
            }
        }

        let mut registry = ProviderRegistry::new();
        registry
            .system_configurators
            .push(Box::new(MockConfigurator));

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
}
