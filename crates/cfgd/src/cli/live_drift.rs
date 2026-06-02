//! Shared live-drift detection for the `verify` and `status --exit-code` paths.
//!
//! Both commands must answer "does the real machine state diverge from the
//! resolved profile *right now*?" using the same engine the `diff` command
//! uses. This module is the single home for that logic so the two `-e` gates
//! cannot drift apart. Detection is strictly read-only — it never records drift
//! events to the state DB (only the daemon and `verify`/`diff` do that).

use cfgd_core::config::ResolvedProfile;
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

/// True if any managed file, package, or system setting diverges from the
/// resolved profile on this host *right now*. Read-only: this performs a live
/// scan (the same checks `diff` runs) but never writes to the `drift_events`
/// table, so a `status --exit-code` call stays a non-recording dashboard query.
pub(super) fn live_drift_detected(
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
) -> anyhow::Result<bool> {
    // Files: content-aware comparison via the file manager.
    let file_drift = file_verify_results(config_dir, resolved)?
        .iter()
        .any(|r| !r.matches);
    if file_drift {
        return Ok(true);
    }

    // Packages: any non-Skip action means the installed set diverges from desired.
    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers
        .iter()
        .map(|m| m.as_ref())
        .collect();
    let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;
    let pkg_drift = pkg_actions
        .iter()
        .any(|a| !matches!(a, PackageAction::Skip { .. }));
    if pkg_drift {
        return Ok(true);
    }

    // System: any configurator reporting a non-empty diff is drift.
    for configurator in &registry.available_system_configurators() {
        if let Some(desired) = resolved.merged.system.get(configurator.name()) {
            // A configurator that errors while probing is treated as
            // indeterminate, not drift — surfacing it as drift here would make a
            // transient probe failure flip the exit code. The display path
            // (`diff`/`verify`) reports such errors to the user.
            if let Ok(drifts) = configurator.diff(desired)
                && !drifts.is_empty()
            {
                return Ok(true);
            }
        }
    }

    Ok(false)
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
    fn live_drift_detected_true_on_file_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "desired\n").unwrap();
        let target = dir.path().join("deployed.txt");
        std::fs::write(&target, "tampered\n").unwrap();

        let resolved = resolved_with_file(target);
        let registry = crate::cli::build_registry_with_profile(&resolved.merged.packages);
        assert!(
            live_drift_detected(dir.path(), &resolved, &registry).unwrap(),
            "content drift on a managed file must register as live drift"
        );
    }

    #[test]
    fn live_drift_detected_false_when_everything_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("managed.txt"), "same\n").unwrap();
        let target = dir.path().join("deployed.txt");
        std::fs::write(&target, "same\n").unwrap();

        let resolved = resolved_with_file(target);
        let registry = crate::cli::build_registry_with_profile(&resolved.merged.packages);
        assert!(
            !live_drift_detected(dir.path(), &resolved, &registry).unwrap(),
            "matching file + empty packages/system must be no-drift"
        );
    }
}
