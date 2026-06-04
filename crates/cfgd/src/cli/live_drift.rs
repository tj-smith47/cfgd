//! Shared live-drift detection for the `verify` and `status --exit-code` paths.
//!
//! Both commands must answer "does the real machine state diverge from the
//! resolved profile *right now*?" using the same engine the `diff` command
//! uses. This module is the single home for that logic so the two `-e` gates
//! cannot drift apart. Detection is strictly read-only — it never records drift
//! events to the state DB (only the daemon and `verify`/`diff` do that).

use cfgd_core::config::ResolvedProfile;
use cfgd_core::modules::ResolvedModule;
use cfgd_core::providers::{PackageAction, ProviderRegistry};
use cfgd_core::reconciler::VerifyResult;

use crate::files::CfgdFileManager;
use crate::packages;

/// Content-aware verify results for every managed file in the profile.
///
/// Wraps [`CfgdFileManager::file_drift_results`] into the reconciler's
/// `VerifyResult` shape so `cmd_verify` can fold file content drift in beside
/// the package/system/module/env results it already collects. A drifted or
/// missing file yields a non-matching result, driving `verify --exit-code` to 5.
pub(super) fn file_verify_results(
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
) -> anyhow::Result<Vec<VerifyResult>> {
    let fm = CfgdFileManager::new(config_dir, resolved)?;
    let drift = fm.file_drift_results(&resolved.merged)?;
    Ok(drift
        .into_iter()
        .map(|d| VerifyResult {
            resource_type: "file".to_string(),
            resource_id: d.target,
            matches: d.matches,
            expected: d.expected,
            actual: d.actual,
        })
        .collect())
}

/// Content-aware verify results for every file a resolved module deploys.
///
/// Mirrors [`file_verify_results`] for module-deployed files: each module file's
/// rendered source bytes are compared to the on-disk target via
/// [`CfgdFileManager::file_drift_one`], yielding a non-matching result when the
/// target is missing OR its bytes drifted out-of-band. Module files carry no tera
/// `origin`, so `None` is passed — consistent with how they deploy. The
/// `resource_id` is `"<module>/<target>"` so module-file drift is attributable.
pub(super) fn module_file_verify_results(
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
    modules: &[ResolvedModule],
) -> anyhow::Result<Vec<VerifyResult>> {
    let fm = CfgdFileManager::new(config_dir, resolved)?;
    let mut results = Vec::new();
    for module in modules {
        for file in &module.files {
            let drift = fm.file_drift_one(&file.source, &file.target, None)?;
            results.push(VerifyResult {
                resource_type: "module".to_string(),
                resource_id: format!("{}/{}", module.name, drift.target),
                matches: drift.matches,
                expected: drift.expected,
                actual: drift.actual,
            });
        }
    }
    Ok(results)
}

/// Non-matching live verify results across every category the live scan covers
/// (profile files, module files, packages, system). Read-only: this performs a
/// live scan (the same checks `diff` runs) but never writes to the `drift_events`
/// table, so a `status --exit-code` call stays a non-recording dashboard query.
///
/// Only divergent results are returned — the caller treats a non-empty vector as
/// "drift detected" and renders each entry. This is the single source of truth
/// for both the `status -e` exit gate and its rendered Drift section, so the
/// human verdict can never contradict the exit code.
pub(super) fn live_drift_results(
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    modules: &[ResolvedModule],
    cfgd_installed: &std::collections::HashSet<String>,
) -> anyhow::Result<Vec<VerifyResult>> {
    let mut drift = Vec::new();

    // Files: content-aware comparison via the file manager.
    drift.extend(
        file_verify_results(config_dir, resolved)?
            .into_iter()
            .filter(|r| !r.matches),
    );

    // Module files: content-aware comparison for each resolved module.
    drift.extend(
        module_file_verify_results(config_dir, resolved, modules)?
            .into_iter()
            .filter(|r| !r.matches),
    );

    // Packages: any non-Skip action means the installed set diverges from desired.
    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers
        .iter()
        .map(|m| m.as_ref())
        .collect();
    for action in packages::plan_packages(&resolved.merged, &all_managers, cfgd_installed)? {
        if let Some(result) = package_action_drift(&action) {
            drift.push(result);
        }
    }

    // System: any configurator reporting a non-empty diff is drift.
    for configurator in &registry.available_system_configurators() {
        if let Some(desired) = resolved.merged.system.get(configurator.name()) {
            // A configurator that errors while probing is treated as
            // indeterminate, not drift — surfacing it as drift here would make a
            // transient probe failure flip the exit code. The display path
            // (`diff`/`verify`) reports such errors to the user.
            if let Ok(drifts) = configurator.diff(desired) {
                for d in &drifts {
                    drift.push(VerifyResult {
                        resource_type: "system".to_string(),
                        resource_id: format!("{}.{}", configurator.name(), d.key),
                        matches: false,
                        expected: d.expected.clone(),
                        actual: d.actual.clone(),
                    });
                }
            }
        }
    }

    Ok(drift)
}

/// Map a non-`Skip` [`PackageAction`] to a drift `VerifyResult`. Returns `None`
/// for `Skip` (the desired/installed sets already agree). The `actual` verb is
/// chosen to read naturally in the drift display (e.g. "not installed").
fn package_action_drift(action: &PackageAction) -> Option<VerifyResult> {
    match action {
        PackageAction::Skip { .. } => None,
        PackageAction::Install {
            manager, packages, ..
        } => Some(VerifyResult {
            resource_type: "package".to_string(),
            resource_id: format!("{}:{}", manager, packages.join(", ")),
            matches: false,
            expected: "installed".to_string(),
            actual: "not installed".to_string(),
        }),
        PackageAction::Uninstall {
            manager, packages, ..
        } => Some(VerifyResult {
            resource_type: "package".to_string(),
            resource_id: format!("{}:{}", manager, packages.join(", ")),
            matches: false,
            expected: "absent".to_string(),
            actual: "to remove".to_string(),
        }),
        PackageAction::Bootstrap {
            manager, method, ..
        } => Some(VerifyResult {
            resource_type: "package".to_string(),
            resource_id: manager.clone(),
            matches: false,
            expected: "installed".to_string(),
            actual: format!("not installed (bootstrap via {method})"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cfgd_core::config::{
        FileStrategy, FilesSpec, LayerPolicy, ManagedFileSpec, MergedProfile, ProfileLayer,
        ProfileSpec, ResolvedProfile,
    };

    use super::*;

    fn resolved_with_file(target: std::path::PathBuf) -> ResolvedProfile {
        let files = FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "managed.txt".to_string(),
                target,
                strategy: Some(FileStrategy::Copy),
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            permissions: HashMap::new(),
        };
        ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".to_string(),
                profile_name: "test".to_string(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                files,
                ..Default::default()
            },
        }
    }

    #[test]
    fn file_verify_results_passes_when_target_matches_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "hello\n").unwrap();
        let target = dir.path().join("deployed.txt");
        std::fs::write(&target, "hello\n").unwrap();

        let resolved = resolved_with_file(target);
        let results = file_verify_results(dir.path(), &resolved).unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0].matches,
            "matching content must pass: {results:?}"
        );
        assert_eq!(results[0].resource_type, "file");
    }

    #[test]
    fn file_verify_results_fails_on_out_of_band_content_drift() {
        // A managed file overwritten out-of-band (present, but different bytes)
        // must be reported as non-matching.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "desired\n").unwrap();
        let target = dir.path().join("deployed.txt");
        std::fs::write(&target, "tampered\n").unwrap();

        let resolved = resolved_with_file(target);
        let results = file_verify_results(dir.path(), &resolved).unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            !results[0].matches,
            "out-of-band content drift must fail: {results:?}"
        );
        assert!(results[0].actual.contains("differs"));
    }

    #[test]
    fn file_verify_results_fails_on_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "x\n").unwrap();
        let target = dir.path().join("never-deployed.txt");

        let resolved = resolved_with_file(target);
        let results = file_verify_results(dir.path(), &resolved).unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0].matches);
        assert_eq!(results[0].actual, "missing");
    }

    #[test]
    fn live_drift_results_nonempty_on_file_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "desired\n").unwrap();
        let target = dir.path().join("deployed.txt");
        std::fs::write(&target, "tampered\n").unwrap();

        let resolved = resolved_with_file(target);
        let registry = crate::cli::build_registry_with_profile(&resolved.merged.packages);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &[],
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert!(
            !drift.is_empty(),
            "content drift on a managed file must register as live drift: {drift:?}"
        );
    }

    #[test]
    fn live_drift_results_empty_when_everything_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "same\n").unwrap();
        let target = dir.path().join("deployed.txt");
        std::fs::write(&target, "same\n").unwrap();

        let resolved = resolved_with_file(target);
        let registry = crate::cli::build_registry_with_profile(&resolved.merged.packages);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &[],
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert!(
            drift.is_empty(),
            "matching file + empty packages/system must be no-drift: {drift:?}"
        );
    }

    /// Build a `ResolvedModule` with a single file (source + target) for the
    /// module-file content-drift tests.
    fn module_with_file(
        name: &str,
        source: std::path::PathBuf,
        target: std::path::PathBuf,
    ) -> ResolvedModule {
        ResolvedModule {
            name: name.to_string(),
            packages: Vec::new(),
            files: vec![cfgd_core::modules::ResolvedFile {
                source,
                target,
                is_git_source: false,
                strategy: None,
                encryption: None,
                permissions: None,
            }],
            env: Vec::new(),
            aliases: Vec::new(),
            system: HashMap::new(),
            pre_apply_scripts: Vec::new(),
            post_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            depends: Vec::new(),
            dir: std::path::PathBuf::new(),
            platform_skip_reason: None,
        }
    }

    #[test]
    fn module_file_verify_results_passes_when_target_matches_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("mod-src.txt");
        std::fs::write(&source, "deployed\n").unwrap();
        let target = dir.path().join("mod-deployed.txt");
        std::fs::write(&target, "deployed\n").unwrap();

        let resolved = resolved_with_file(dir.path().join("unused.txt"));
        let modules = vec![module_with_file("accmod", source, target)];
        let results = module_file_verify_results(dir.path(), &resolved, &modules).unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0].matches,
            "matching module file must pass: {results:?}"
        );
        assert_eq!(results[0].resource_type, "module");
        assert!(results[0].resource_id.starts_with("accmod/"));
    }

    #[test]
    fn module_file_verify_results_fails_on_out_of_band_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("mod-src.txt");
        std::fs::write(&source, "desired\n").unwrap();
        let target = dir.path().join("mod-deployed.txt");
        std::fs::write(&target, "tampered\n").unwrap();

        let resolved = resolved_with_file(dir.path().join("unused.txt"));
        let modules = vec![module_with_file("accmod", source, target)];
        let results = module_file_verify_results(dir.path(), &resolved, &modules).unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            !results[0].matches,
            "tampered module file must fail: {results:?}"
        );
        assert!(results[0].actual.contains("differs"));
    }

    #[test]
    fn live_drift_results_includes_module_file_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        // Profile file matches (no profile-file drift) so only the module file
        // can drive the result — proves the module category is wired in.
        std::fs::write(dir.path().join("managed.txt"), "same\n").unwrap();
        let profile_target = dir.path().join("deployed.txt");
        std::fs::write(&profile_target, "same\n").unwrap();

        let mod_source = dir.path().join("mod-src.txt");
        std::fs::write(&mod_source, "desired\n").unwrap();
        let mod_target = dir.path().join("mod-deployed.txt");
        std::fs::write(&mod_target, "tampered\n").unwrap();

        let resolved = resolved_with_file(profile_target);
        let registry = crate::cli::build_registry_with_profile(&resolved.merged.packages);
        let modules = vec![module_with_file("accmod", mod_source, mod_target)];
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert_eq!(drift.len(), 1, "only the module file drifts: {drift:?}");
        assert_eq!(drift[0].resource_type, "module");
    }

    #[test]
    fn live_drift_results_clean_module_file_yields_no_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "same\n").unwrap();
        let profile_target = dir.path().join("deployed.txt");
        std::fs::write(&profile_target, "same\n").unwrap();

        let mod_source = dir.path().join("mod-src.txt");
        std::fs::write(&mod_source, "clean\n").unwrap();
        let mod_target = dir.path().join("mod-deployed.txt");
        std::fs::write(&mod_target, "clean\n").unwrap();

        let resolved = resolved_with_file(profile_target);
        let registry = crate::cli::build_registry_with_profile(&resolved.merged.packages);
        let modules = vec![module_with_file("accmod", mod_source, mod_target)];
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert!(
            drift.is_empty(),
            "clean module file must not drift: {drift:?}"
        );
    }
}
