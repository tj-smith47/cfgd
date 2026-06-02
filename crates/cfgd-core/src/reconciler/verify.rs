use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::config::{EnvScope, ResolvedProfile};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::providers::ProviderRegistry;
use crate::state::StateStore;
use crate::to_posix_string;

use super::env_engine::{EnvHostProbe, EnvPlatform, EnvTarget, env_targets};

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

            // Emit a pass OR fail row per package, mirroring the profile-package
            // loop below. The blanket "module healthy" row is gone: module file
            // rows are folded in content-aware by the binary crate, so a blanket
            // healthy line could contradict a folded-in file-drift row.
            results.push(VerifyResult {
                resource_type: "module".to_string(),
                resource_id: format!("{}/{}", module.name, pkg.resolved_name),
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
                    "module",
                    &format!("{}/{}", module.name, pkg.resolved_name),
                    Some("installed"),
                    Some("missing"),
                    "local",
                );
            }
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

    // Managed-file verification is content-aware and lives in the binary crate
    // (`cli::live_drift`), which can reach `CfgdFileManager` to compare rendered
    // source bytes against the on-disk target. This reconciler cannot — the file
    // manager is across the crate boundary — so file results are folded in by the
    // caller rather than computed here as a presence-only check.

    // Verify env: re-derive the same targets the planner wrote and check each.
    verify_env(
        &resolved.merged.env,
        &resolved.merged.aliases,
        resolved.merged.env_scope,
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
    scope: EnvScope,
    modules: &[ResolvedModule],
    state: &StateStore,
    results: &mut Vec<VerifyResult>,
) {
    let (merged, merged_aliases) = merge_module_env_aliases(profile_env, profile_aliases, modules);

    if merged.is_empty() && merged_aliases.is_empty() {
        return;
    }

    // Re-derive the exact target set the planner wrote, so verify never reports
    // a file the current scope intentionally left unwritten as drift.
    let home = expand_tilde(std::path::Path::new("~"));
    let probe = EnvHostProbe::detect(&home);
    let platform = EnvPlatform::current();
    for target in env_targets(&merged, &merged_aliases, scope, &home, &probe, platform) {
        match target {
            EnvTarget::ManagedFile { path, content } => {
                verify_env_file(&path, &content, state, results);
            }
            EnvTarget::SourceLine { rc_path, line } => {
                let has_line = std::fs::read_to_string(&rc_path)
                    .map(|content| content.contains(&line))
                    .unwrap_or(false);
                results.push(VerifyResult {
                    resource_type: "env-rc".to_string(),
                    resource_id: to_posix_string(&rc_path),
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
                        &to_posix_string(&rc_path),
                        Some("source line present"),
                        Some("source line missing"),
                        "local",
                    );
                }
            }
            // The live-session refresh is best-effort and ephemeral (a re-login
            // clears it); it is not a verified-drift surface — the durable file
            // targets above are authoritative.
            EnvTarget::LiveSession { .. } => {}
        }
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
