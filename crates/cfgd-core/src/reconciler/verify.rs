use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::config::ResolvedProfile;
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::providers::ProviderRegistry;
use crate::state::StateStore;
use crate::to_posix_string;

use super::env_files::{
    fish_in_use, generate_env_file_content, generate_fish_env_content,
    generate_powershell_env_content,
};

/// Record a drift event or log a warning if the write fails. Previous sites
/// used `.ok()` which silently dropped SQLite errors (locked DB, full disk),
/// leaving `unresolved_drift()` out of sync with observed reality.
pub(super) fn record_drift_or_warn(
    state: &StateStore,
    resource_type: &str,
    resource_id: &str,
    expected: Option<&str>,
    actual: Option<&str>,
    source: &str,
) {
    if let Err(e) = state.record_drift(resource_type, resource_id, expected, actual, source) {
        tracing::warn!(
            error = %e,
            resource_type = %resource_type,
            resource_id = %resource_id,
            "failed to record drift"
        );
    }
}

/// Verify all managed resources match their desired state.
pub fn verify(
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    state: &StateStore,
    _printer: &Printer,
    modules: &[ResolvedModule],
) -> Result<Vec<VerifyResult>> {
    let mut results = Vec::new();

    // Verify modules — check that module packages are installed
    // Cache installed-packages per manager to avoid N+1 queries
    let available_managers = registry.available_package_managers();
    let mut installed_cache: HashMap<String, HashSet<String>> = HashMap::new();
    for module in modules {
        let mut module_ok = true;

        for pkg in &module.packages {
            // Script-based packages can't be verified via installed_packages() —
            // trust the apply log (if the script succeeded, it's installed).
            if pkg.manager == "script" {
                continue;
            }

            if !installed_cache.contains_key(&pkg.manager) {
                let mgr = available_managers.iter().find(|m| m.name() == pkg.manager);
                let set = mgr
                    .map(|m| m.installed_packages())
                    .transpose()?
                    .unwrap_or_default();
                installed_cache.insert(pkg.manager.clone(), set);
            }
            let installed = &installed_cache[&pkg.manager];
            let ok = installed.contains(&pkg.resolved_name);

            if !ok {
                module_ok = false;
                results.push(VerifyResult {
                    resource_type: "module".to_string(),
                    resource_id: format!("{}/{}", module.name, pkg.resolved_name),
                    matches: false,
                    expected: "installed".to_string(),
                    actual: "missing".to_string(),
                });
                record_drift_or_warn(
                    state,
                    "module",
                    &format!("{}/{}", module.name, pkg.resolved_name),
                    Some("installed"),
                    Some("missing"),
                    "local",
                );
            }
        }

        // Check module file targets exist
        for file in &module.files {
            let target = expand_tilde(&file.target);
            if !target.exists() {
                module_ok = false;
                results.push(VerifyResult {
                    resource_type: "module".to_string(),
                    resource_id: format!("{}/{}", module.name, to_posix_string(&target)),
                    matches: false,
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
                record_drift_or_warn(
                    state,
                    "module",
                    &format!("{}/{}", module.name, to_posix_string(&target)),
                    Some("present"),
                    Some("missing"),
                    "local",
                );
            }
        }

        if module_ok {
            results.push(VerifyResult {
                resource_type: "module".to_string(),
                resource_id: module.name.clone(),
                matches: true,
                expected: "healthy".to_string(),
                actual: "healthy".to_string(),
            });
        }
    }

    // Verify packages
    let available_managers = registry.available_package_managers();
    for pm in &available_managers {
        let desired = crate::config::desired_packages_for(pm.name(), &resolved.merged);
        if desired.is_empty() {
            continue;
        }
        let installed = pm.installed_packages()?;
        for pkg in &desired {
            let ok = installed.contains(pkg);
            results.push(VerifyResult {
                resource_type: "package".to_string(),
                resource_id: format!("{}:{}", pm.name(), pkg),
                matches: ok,
                expected: "installed".to_string(),
                actual: if ok {
                    "installed".to_string()
                } else {
                    "missing".to_string()
                },
            });

            if !ok {
                record_drift_or_warn(
                    state,
                    "package",
                    &format!("{}:{}", pm.name(), pkg),
                    Some("installed"),
                    Some("missing"),
                    "local",
                );
            }
        }
    }

    // Verify system configurators
    for sc in registry.available_system_configurators() {
        if let Some(desired) = resolved.merged.system.get(sc.name()) {
            let drifts = sc.diff(desired)?;
            if drifts.is_empty() {
                results.push(VerifyResult {
                    resource_type: "system".to_string(),
                    resource_id: sc.name().to_string(),
                    matches: true,
                    expected: "configured".to_string(),
                    actual: "configured".to_string(),
                });
            } else {
                for drift in &drifts {
                    results.push(VerifyResult {
                        resource_type: "system".to_string(),
                        resource_id: format!("{}.{}", sc.name(), drift.key),
                        matches: false,
                        expected: drift.expected.clone(),
                        actual: drift.actual.clone(),
                    });

                    record_drift_or_warn(
                        state,
                        "system",
                        &format!("{}.{}", sc.name(), drift.key),
                        Some(&drift.expected),
                        Some(&drift.actual),
                        "local",
                    );
                }
            }
        }
    }

    // Verify files by checking managed file targets exist with expected content
    for managed in &resolved.merged.files.managed {
        let target = expand_tilde(&managed.target);
        if target.exists() {
            results.push(VerifyResult {
                resource_type: "file".to_string(),
                resource_id: to_posix_string(&target),
                matches: true,
                expected: "present".to_string(),
                actual: "present".to_string(),
            });
        } else {
            results.push(VerifyResult {
                resource_type: "file".to_string(),
                resource_id: to_posix_string(&target),
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
            });
        }
    }

    // Verify env: check ~/.cfgd.env matches expected content
    verify_env(
        &resolved.merged.env,
        &resolved.merged.aliases,
        modules,
        state,
        &mut results,
    );

    Ok(results)
}

/// Result of verifying a single resource.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    pub resource_type: String,
    pub resource_id: String,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
}

pub(super) fn merge_module_env_aliases(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
) -> (Vec<crate::config::EnvVar>, Vec<crate::config::ShellAlias>) {
    let mut merged = profile_env.to_vec();
    let mut merged_aliases = profile_aliases.to_vec();
    for module in modules {
        crate::merge_env(&mut merged, &module.env);
        crate::merge_aliases(&mut merged_aliases, &module.aliases);
    }
    (merged, merged_aliases)
}

/// Verify env file and shell rc source line match expected state.
// NOTE: Secret-backed env vars (from SecretSpec.envs) are not included in
// verification because they require provider resolution. This means cfgd status
// may report env file drift after secret envs are written. This will be addressed
// when compliance snapshots track secret env metadata.
pub(super) fn verify_env(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
    state: &StateStore,
    results: &mut Vec<VerifyResult>,
) {
    let (merged, merged_aliases) = merge_module_env_aliases(profile_env, profile_aliases, modules);

    if merged.is_empty() && merged_aliases.is_empty() {
        return;
    }

    if cfg!(windows) {
        // Verify PowerShell env file
        let ps_path = expand_tilde(std::path::Path::new("~/.cfgd-env.ps1"));
        let expected_ps = generate_powershell_env_content(&merged, &merged_aliases);
        verify_env_file(&ps_path, &expected_ps, state, results);

        // Verify PowerShell profile injection
        let ps_profile_dirs = [
            expand_tilde(std::path::Path::new("~/Documents/PowerShell")),
            expand_tilde(std::path::Path::new("~/Documents/WindowsPowerShell")),
        ];
        for profile_dir in &ps_profile_dirs {
            let profile_path = profile_dir.join("Microsoft.PowerShell_profile.ps1");
            let has_line = std::fs::read_to_string(&profile_path)
                .map(|content| content.contains("cfgd-env.ps1"))
                .unwrap_or(false);
            results.push(VerifyResult {
                resource_type: "env-rc".to_string(),
                resource_id: to_posix_string(&profile_path),
                matches: has_line,
                expected: "source line present".to_string(),
                actual: if has_line {
                    "source line present".to_string()
                } else {
                    "source line missing".to_string()
                },
            });
            if !has_line {
                record_drift_or_warn(
                    state,
                    "env-rc",
                    &to_posix_string(&profile_path),
                    Some("source line present"),
                    Some("source line missing"),
                    "local",
                );
            }
        }

        // If Git Bash available, also verify bash env file
        if crate::command_available("sh") {
            let bash_path = expand_tilde(std::path::Path::new("~/.cfgd.env"));
            let expected_bash = generate_env_file_content(&merged, &merged_aliases);
            verify_env_file(&bash_path, &expected_bash, state, results);
        }
    } else {
        // Unix: verify bash/zsh env file
        let env_path = expand_tilde(std::path::Path::new("~/.cfgd.env"));
        let expected_content = generate_env_file_content(&merged, &merged_aliases);
        verify_env_file(&env_path, &expected_content, state, results);

        // Check shell rc source line
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let rc_path = if shell.contains("zsh") {
            expand_tilde(std::path::Path::new("~/.zshrc"))
        } else {
            expand_tilde(std::path::Path::new("~/.bashrc"))
        };

        let has_source_line = std::fs::read_to_string(&rc_path)
            .map(|content| content.contains("cfgd.env"))
            .unwrap_or(false);
        results.push(VerifyResult {
            resource_type: "env-rc".to_string(),
            resource_id: to_posix_string(&rc_path),
            matches: has_source_line,
            expected: "source line present".to_string(),
            actual: if has_source_line {
                "source line present".to_string()
            } else {
                "source line missing".to_string()
            },
        });
        if !has_source_line {
            record_drift_or_warn(
                state,
                "env-rc",
                &to_posix_string(&rc_path),
                Some("source line present"),
                Some("source line missing"),
                "local",
            );
        }
    }

    // Check fish env file only if fish is the user's shell.
    // Windows fish lives outside $SHELL conventions — see fish_in_use().
    let fish_conf_d = expand_tilde(std::path::Path::new("~/.config/fish/conf.d"));
    if fish_in_use() && fish_conf_d.exists() {
        let fish_path = fish_conf_d.join("cfgd-env.fish");
        let expected_fish = generate_fish_env_content(&merged, &merged_aliases);
        verify_env_file(&fish_path, &expected_fish, state, results);
    }
}

/// Verify a single env file's content matches expected.
pub(super) fn verify_env_file(
    path: &std::path::Path,
    expected: &str,
    state: &StateStore,
    results: &mut Vec<VerifyResult>,
) {
    match std::fs::read_to_string(path) {
        Ok(actual) if actual == expected => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: true,
                expected: "current".to_string(),
                actual: "current".to_string(),
            });
        }
        Ok(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: false,
                expected: "current".to_string(),
                actual: "stale".to_string(),
            });
            record_drift_or_warn(
                state,
                "env",
                &to_posix_string(path),
                Some("current"),
                Some("stale"),
                "local",
            );
        }
        Err(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: to_posix_string(path),
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
            });
            record_drift_or_warn(
                state,
                "env",
                &to_posix_string(path),
                Some("present"),
                Some("missing"),
                "local",
            );
        }
    }
}
