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

    // Verify packages — profile and module packages share one effective desired
    // set so a `(manager, name)` declared in both is checked once, and the
    // module-vs-profile attribution drives the result shape. The per-manager
    // installed set is cached to avoid N+1 queries.
    let available_managers = registry.available_package_managers();
    let mut installed_cache: HashMap<String, HashSet<String>> = HashMap::new();
    for ep in crate::effective::effective_desired_packages(&resolved.merged, modules) {
        // A `prefer: [script]` package has no queryable installed-state: a custom
        // install script can put anything anywhere, so there is no
        // installed_packages() set to diff it against. It is therefore invisible
        // to drift detection by design. Idempotency for these installs is the
        // script's responsibility, expressed via the package entry's
        // creates/onlyIf/unless guards (honored on the apply path in
        // reconciler::modules) — not something verify can re-derive here.
        if ep.manager == "script" {
            continue;
        }

        // A manager that isn't available on this host cannot install or report
        // its packages, so a "missing" verdict here would be a false alarm. Skip
        // such packages for BOTH origins (profile packages were already skipped
        // by iterating only available managers; module packages used to be
        // reported missing — this makes the two consistent).
        let Some(mgr) = available_managers.iter().find(|m| m.name() == ep.manager) else {
            continue;
        };

        if !installed_cache.contains_key(&ep.manager) {
            installed_cache.insert(ep.manager.clone(), mgr.installed_packages()?);
        }
        let installed = &installed_cache[&ep.manager];
        // Compare through package_identity so case-insensitive managers (choco/scoop/
        // winget: `wget` vs installed `Wget`) and name-remapping managers (go: module
        // path vs binary) match like with like.
        let ok = installed.contains(&mgr.package_identity(&ep.name));

        // Preserve each origin's resource conventions: module packages report as
        // `module` / `<module>/<name>`; profile packages as `package` /
        // `<manager>:<name>`.
        let (resource_type, resource_id) = match &ep.origin {
            crate::effective::Origin::Module(name) => ("module", format!("{}/{}", name, ep.name)),
            crate::effective::Origin::Profile => ("package", format!("{}:{}", ep.manager, ep.name)),
        };

        results.push(VerifyResult {
            resource_type: resource_type.to_string(),
            resource_id: resource_id.clone(),
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
                resource_type,
                &resource_id,
                Some("installed"),
                Some("missing"),
                "local",
            );
        }
    }

    // Verify system configurators against the effective (profile ⊕ modules)
    // system map so module system config is verified too.
    let system = crate::effective::effective_system_map(&resolved.merged, modules);
    for sc in registry.available_system_configurators() {
        if let Some(desired) = system.get(sc.name()) {
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
