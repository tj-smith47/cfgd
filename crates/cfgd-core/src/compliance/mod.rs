// Compliance snapshot — types, collection logic, summary computation, export

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{ComplianceExport, ComplianceFormat, ComplianceScope, MergedProfile};
use crate::effective::{Origin, effective_files};
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::platform::Platform;
use crate::providers::ProviderRegistry;
use crate::to_posix_string;

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
///
/// The collected state is the *effective* desired state — the profile combined
/// with the modules it pulls in — so module-contributed files, packages, and
/// system settings appear in compliance reporting exactly as they do on the
/// write/verify paths. `config_dir` resolves profile-relative file sources so
/// file checks can compare content; `modules` are the profile's resolved modules
/// (empty slice for a module-free profile).
///
/// File checks are content-aware when `registry.file_manager` is set: a managed
/// file present on disk but whose bytes drifted from its rendered source is a
/// violation, matching the live drift paths. When no file manager is wired, file
/// checks degrade to existence + permissions only.
pub fn collect_snapshot(
    profile_name: &str,
    profile: &MergedProfile,
    modules: &[ResolvedModule],
    config_dir: &Path,
    registry: &ProviderRegistry,
    scope: &ComplianceScope,
    sources: &[String],
) -> Result<ComplianceSnapshot> {
    let platform = Platform::detect();
    let hostname = crate::hostname_string();

    let machine = MachineInfo {
        hostname,
        os: platform.os.as_str().to_owned(),
        arch: platform.arch.as_str().to_owned(),
    };

    let mut checks = Vec::new();

    if scope.files {
        checks.extend(collect_file_checks(profile, modules, config_dir, registry));
    }
    if scope.packages {
        checks.extend(collect_package_checks(profile, modules, registry)?);
    }
    if scope.system {
        checks.extend(collect_system_checks(profile, modules, registry)?);
    }
    if scope.secrets {
        checks.extend(collect_secret_checks(profile));
    }
    for watch_path in &scope.watch_paths {
        checks.extend(collect_watch_path_checks(watch_path));
    }
    for manager_name in &scope.watch_package_managers {
        checks.extend(collect_watched_package_manager_checks(
            manager_name,
            registry,
        )?);
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
// Export
// ---------------------------------------------------------------------------

/// Export a compliance snapshot to a file based on the export configuration.
///
/// Steps: expand tilde in path, create directory, build timestamped filename,
/// serialize to JSON or YAML, and atomically write.
///
/// Returns the path of the written file.
pub fn export_snapshot_to_file(
    snapshot: &ComplianceSnapshot,
    export: &ComplianceExport,
) -> Result<PathBuf> {
    let export_dir = crate::expand_tilde(Path::new(&export.path));
    std::fs::create_dir_all(&export_dir)?;

    let timestamp_safe = crate::iso8601_to_filename_safe(&snapshot.timestamp);
    let ext = match export.format {
        ComplianceFormat::Json => "json",
        ComplianceFormat::Yaml => "yaml",
    };
    let filename = format!("compliance-{}.{}", timestamp_safe, ext);
    let file_path = export_dir.join(&filename);

    let content = match export.format {
        ComplianceFormat::Json => serde_json::to_string_pretty(snapshot)
            .map_err(|e| std::io::Error::other(format!("JSON serialization failed: {}", e)))?,
        ComplianceFormat::Yaml => serde_yaml::to_string(snapshot)
            .map_err(|e| std::io::Error::other(format!("YAML serialization failed: {}", e)))?,
    };

    crate::atomic_write_str(&file_path, &content)?;
    Ok(file_path)
}

// ---------------------------------------------------------------------------
// File checks
// ---------------------------------------------------------------------------

/// Detail suffix attributing a check to the module that contributed it. Empty
/// for profile-declared resources so existing profile-file detail is unchanged.
fn origin_suffix(origin: &Origin) -> String {
    match origin {
        Origin::Profile => String::new(),
        Origin::Module(name) => format!(" (module: {})", name),
    }
}

/// Check managed files across the effective desired state (profile AND modules):
/// content drift, existence, permissions, and encryption declaration.
///
/// When `registry.file_manager` is set, each present file's on-disk bytes are
/// compared to its rendered source via `FileManager::content_drift` — a file that
/// exists but drifted is a violation, matching the live drift paths. Without a
/// file manager, content checking is skipped and only existence + permissions are
/// reported (honest degradation). Module-contributed files are attributed in each
/// check's detail so a reader can tell their origin.
pub fn collect_file_checks(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
    config_dir: &Path,
    registry: &ProviderRegistry,
) -> Vec<ComplianceCheck> {
    let mut checks = Vec::new();

    for file in effective_files(profile, modules, config_dir) {
        let target = crate::expand_tilde(&file.target);
        let exists = target.exists();
        let suffix = origin_suffix(&file.origin);

        if !exists {
            checks.push(ComplianceCheck {
                category: "file".into(),
                target: Some(to_posix_string(&target)),
                status: ComplianceStatus::Violation,
                detail: Some(format!("managed file missing{}", suffix)),
                ..Default::default()
            });
            continue;
        }

        // Content drift: compare on-disk bytes to the rendered source. When a
        // file manager is wired this check is the existence signal (content
        // matching proves the file is present), so the legacy "present" check is
        // suppressed below to avoid two Compliant rows for the same signal.
        // Without a file manager, content checking is skipped and the "present"
        // check stands in as the existence signal.
        let content_checked = if let Some(ref fm) = registry.file_manager {
            match fm.content_drift(
                Path::new(&file.source),
                &file.target,
                file.tera_origin.as_deref(),
            ) {
                Ok(drift) => {
                    if drift.matches {
                        checks.push(ComplianceCheck {
                            category: "file-content".into(),
                            target: Some(to_posix_string(&target)),
                            status: ComplianceStatus::Compliant,
                            detail: Some(format!("content matches source{}", suffix)),
                            ..Default::default()
                        });
                    } else {
                        checks.push(ComplianceCheck {
                            category: "file-content".into(),
                            target: Some(to_posix_string(&target)),
                            status: ComplianceStatus::Violation,
                            detail: Some(format!("{}{}", drift.actual, suffix)),
                            ..Default::default()
                        });
                    }
                }
                Err(e) => {
                    checks.push(ComplianceCheck {
                        category: "file-content".into(),
                        target: Some(to_posix_string(&target)),
                        status: ComplianceStatus::Warning,
                        detail: Some(format!("cannot compare content: {}{}", e, suffix)),
                        ..Default::default()
                    });
                }
            }
            true
        } else {
            false
        };

        // Check permissions if declared
        if let Some(ref perm_str) = file.permissions {
            if let Ok(desired_mode) = crate::parse_octal_mode(perm_str) {
                let actual_mode = target
                    .metadata()
                    .ok()
                    .and_then(|m| crate::file_permissions_mode(&m));
                match actual_mode {
                    Some(mode) if mode == desired_mode => {
                        checks.push(ComplianceCheck {
                            category: "file".into(),
                            target: Some(to_posix_string(&target)),
                            status: ComplianceStatus::Compliant,
                            detail: Some(format!("permissions {:#o}{}", mode, suffix)),
                            ..Default::default()
                        });
                    }
                    Some(mode) => {
                        checks.push(ComplianceCheck {
                            category: "file".into(),
                            target: Some(to_posix_string(&target)),
                            status: ComplianceStatus::Warning,
                            detail: Some(format!(
                                "permissions {:#o}, expected {:#o}{}",
                                mode, desired_mode, suffix
                            )),
                            ..Default::default()
                        });
                    }
                    None => {
                        // Windows or metadata unavailable — compliant by default
                        checks.push(ComplianceCheck {
                            category: "file".into(),
                            target: Some(to_posix_string(&target)),
                            status: ComplianceStatus::Compliant,
                            detail: Some(format!(
                                "permissions not applicable on this platform{}",
                                suffix
                            )),
                            ..Default::default()
                        });
                    }
                }
            } else {
                // Malformed permission string
                checks.push(ComplianceCheck {
                    category: "file".into(),
                    target: Some(to_posix_string(&target)),
                    status: ComplianceStatus::Warning,
                    detail: Some(format!("invalid permission string: {}{}", perm_str, suffix)),
                    ..Default::default()
                });
            }
        } else if !content_checked {
            // No permissions declared and no content check ran (no file manager):
            // the "present" check is the existence signal. When a content check
            // ran it already proved presence, so this would double-count.
            checks.push(ComplianceCheck {
                category: "file".into(),
                target: Some(to_posix_string(&target)),
                status: ComplianceStatus::Compliant,
                detail: Some(format!("present{}", suffix)),
                ..Default::default()
            });
        }

        // Check encryption declaration (if encryption is specified, just verify it is declared)
        if let Some(ref enc) = file.encryption {
            checks.push(ComplianceCheck {
                category: "file-encryption".into(),
                target: Some(to_posix_string(&target)),
                status: ComplianceStatus::Compliant,
                detail: Some(format!("encryption: backend={}{}", enc.backend, suffix)),
                ..Default::default()
            });
        }
    }

    checks
}

// ---------------------------------------------------------------------------
// Package checks
// ---------------------------------------------------------------------------

/// Check that the effective desired packages (profile AND modules) are installed
/// via their respective managers.
///
/// The desired set is derived from
/// [`crate::effective::effective_desired_packages`] and intersected with the
/// registry's available managers: a package whose manager is unavailable on this
/// host is skipped (consistent with the verify path), and a manager that cannot
/// be queried yields a single per-manager warning. Module packages now appear,
/// attributed to their module in the check detail.
pub fn collect_package_checks(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
    registry: &ProviderRegistry,
) -> Result<Vec<ComplianceCheck>> {
    use std::collections::HashMap;

    let mut checks = Vec::new();

    // Group desired packages by manager, preserving origin for attribution.
    let mut by_manager: HashMap<String, Vec<(String, Origin)>> = HashMap::new();
    for ep in crate::effective::effective_desired_packages(profile, modules) {
        by_manager
            .entry(ep.manager)
            .or_default()
            .push((ep.name, ep.origin));
    }

    for pm in registry.available_package_managers() {
        let Some(desired) = by_manager.get(pm.name()) else {
            continue;
        };
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

        for (pkg, origin) in desired {
            let suffix = origin_suffix(origin);
            if installed.contains(pkg) {
                checks.push(ComplianceCheck {
                    category: "package".into(),
                    name: Some(pkg.clone()),
                    manager: Some(pm.name().to_owned()),
                    status: ComplianceStatus::Compliant,
                    detail: Some(format!("installed{}", suffix)),
                    ..Default::default()
                });
            } else {
                checks.push(ComplianceCheck {
                    category: "package".into(),
                    name: Some(pkg.clone()),
                    manager: Some(pm.name().to_owned()),
                    status: ComplianceStatus::Violation,
                    detail: Some(format!("not installed{}", suffix)),
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

/// Check system configurator state for drift across the effective desired state
/// (profile system settings deep-merged with every module's), so module system
/// tweaks surface in compliance exactly as they do on the write path.
pub fn collect_system_checks(
    profile: &MergedProfile,
    modules: &[ResolvedModule],
    registry: &ProviderRegistry,
) -> Result<Vec<ComplianceCheck>> {
    let mut checks = Vec::new();
    let available = registry.available_system_configurators();
    let system = crate::effective::effective_system_map(profile, modules);

    for (key, desired) in &system {
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
                target: Some(to_posix_string(&target)),
                status: ComplianceStatus::Compliant,
                detail: Some("target file present".into()),
                ..Default::default()
            });
        } else {
            checks.push(ComplianceCheck {
                category: "secret".into(),
                target: Some(to_posix_string(&target)),
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
            category: "watchPath".into(),
            path: Some(to_posix_string(path)),
            status: ComplianceStatus::Warning,
            detail: Some("path does not exist".into()),
            ..Default::default()
        }];
    }

    let meta = match path.metadata() {
        Ok(m) => m,
        Err(e) => {
            return vec![ComplianceCheck {
                category: "watchPath".into(),
                path: Some(to_posix_string(path)),
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
        category: "watchPath".into(),
        path: Some(to_posix_string(path)),
        status: ComplianceStatus::Compliant,
        detail: Some(detail),
        ..Default::default()
    }]
}

// ---------------------------------------------------------------------------
// Watch package manager checks
// ---------------------------------------------------------------------------

/// Enumerate all installed packages from a named package manager.
/// Each installed package is reported as a `watchPackage` category check,
/// providing a full inventory of what is installed (not just managed packages).
fn collect_watched_package_manager_checks(
    manager_name: &str,
    registry: &ProviderRegistry,
) -> Result<Vec<ComplianceCheck>> {
    let pm = registry
        .available_package_managers()
        .into_iter()
        .find(|pm| pm.name() == manager_name);

    let Some(pm) = pm else {
        return Ok(vec![ComplianceCheck {
            category: "watchPackage".into(),
            manager: Some(manager_name.to_owned()),
            status: ComplianceStatus::Warning,
            detail: Some(format!("package manager '{}' not available", manager_name)),
            ..Default::default()
        }]);
    };

    let installed = match pm.installed_packages() {
        Ok(set) => set,
        Err(e) => {
            return Ok(vec![ComplianceCheck {
                category: "watchPackage".into(),
                manager: Some(manager_name.to_owned()),
                status: ComplianceStatus::Warning,
                detail: Some(format!("cannot query {}: {}", manager_name, e)),
                ..Default::default()
            }]);
        }
    };

    let mut checks: Vec<ComplianceCheck> = installed
        .into_iter()
        .map(|pkg| ComplianceCheck {
            category: "watchPackage".into(),
            name: Some(pkg),
            manager: Some(manager_name.to_owned()),
            status: ComplianceStatus::Compliant,
            detail: Some("installed".into()),
            ..Default::default()
        })
        .collect();

    // Sort for deterministic output
    checks.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(checks)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
