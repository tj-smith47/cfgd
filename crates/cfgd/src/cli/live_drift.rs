//! Shared live-drift detection for the `verify` and `status --scan` paths.
//!
//! Both commands must answer "does the real machine state diverge from the
//! resolved profile *right now*?" using the same engine the `diff` command
//! uses. This module is the single home for that logic so the two `-e` gates
//! cannot drift apart. Detection is strictly read-only — it never records drift
//! events to the state DB (only the daemon and `verify`/`diff` do that).

use cfgd_core::config::ResolvedProfile;
use cfgd_core::modules::ResolvedModule;
use cfgd_core::providers::{PackageAction, ProviderRegistry};
use cfgd_core::reconciler::{Action, ManagerAction, VerifyResult};

use crate::files::{CfgdFileManager, module_patch_binding};
use crate::packages;

/// The ONE shaping of a live [`VerifyResult`] into a recorded-shape
/// [`cfgd_core::state::DriftEvent`], for a caller (`cmd_status`,
/// `cmd_status_module`'s two drift loops) that must fold a live-scan finding
/// into the same `-o json` `drift` array a recorded event lives in. `id: 0`
/// and `resolved_by: None` mark it as never persisted — a live finding has no
/// row of its own — and `source` is always [`cfgd_core::config::LOCAL_LAYER`],
/// since a live scan has no config-layer provenance to attribute. `matches`
/// is dropped: every caller has already filtered to `!matches` before
/// reaching here, and `DriftEvent` carries no field for it.
///
/// `env`/`aliases`/`modules` recompute an env-var/alias row's opaque
/// `current`/`missing or changed` markers into the real declared line and the
/// real line the managed file holds, via
/// [`cfgd_core::reconciler::env_item_display_values`] — safe here precisely
/// because `id: 0` means this `DriftEvent` is never persisted or shipped to
/// the gateway, only rendered into this command's own human/`-o json`
/// output. A caller with no env/alias rows in its input (both
/// `cmd_status_module` loops: file and package kinds only) may pass empty
/// slices; the recompute is a no-op for any other `resource_type`. `modules`
/// is what makes a module-declared entry renderable at all — its entries live
/// in the module rather than in the profile's own `env`/`aliases`, and its
/// line carries the provenance comment the file on disk holds.
pub(super) fn drift_event_from(
    r: &VerifyResult,
    env: &[cfgd_core::config::EnvVar],
    aliases: &[cfgd_core::config::ShellAlias],
    modules: &[cfgd_core::modules::ResolvedModule],
) -> cfgd_core::state::DriftEvent {
    let (expected, actual) = cfgd_core::reconciler::env_item_display_values(
        &r.resource_type,
        &r.resource_id,
        env,
        aliases,
        modules,
    )
    .unwrap_or_else(|| (r.expected.clone(), r.actual.clone()));
    cfgd_core::state::DriftEvent {
        id: 0,
        timestamp: cfgd_core::utc_now_iso8601(),
        resource_type: r.resource_type.clone(),
        resource_id: r.resource_id.clone(),
        expected: Some(expected),
        actual: Some(actual),
        resolved_by: None,
        source: cfgd_core::config::LOCAL_LAYER.to_string(),
    }
}

/// Content-aware verify results for every managed file in the profile.
///
/// Wraps [`CfgdFileManager::file_drift_results`] into the reconciler's
/// `VerifyResult` shape so `cmd_verify` can fold file content drift in beside
/// the package/system/module/env results it already collects. A drifted or
/// missing file yields a non-matching result, driving `verify --exit-code` to 5.
///
/// Takes the file manager rather than building one: this and
/// [`module_file_verify_results`] run back to back on every `verify` and every
/// `status --scan`, over the same profile, and each construction rebuilds
/// the template context and the whole secret-provider set.
pub(super) fn file_verify_results(
    fm: &CfgdFileManager,
    resolved: &ResolvedProfile,
) -> anyhow::Result<Vec<VerifyResult>> {
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
/// The ONE composition of a module file's verify/drift identity.
///
/// `target` is posix-folded and, for a real deployed file, absolute — joining
/// it under the module name with a bare `/` doubles up into `nvim//home/tj/…`,
/// so the redundant leading separator is trimmed and the id reads as one path
/// rather than two glued halves. `cfgd status <module>` composes the same
/// string from a file the state store recorded to ask whether the scan found
/// that file drifted; the producer and that lookup must never disagree about
/// the spelling, or a drifted file reads clean under Deployed Files.
pub(super) fn module_file_resource_id(module: &str, target: &str) -> String {
    format!("{}/{}", module, target.trim_start_matches('/'))
}

/// The inverse of [`module_file_resource_id`]: the module a finding belongs to
/// and the deployed path it names.
///
/// A drift row names its owner and its item separately, and the id is the only
/// place a live file finding carries either. The leading separator the id
/// trimmed is restored unless what remains is already rooted, so a Windows
/// `C:/Users/…` target keeps its drive instead of gaining a separator that
/// names nothing. Judged on the string rather than through `Path::is_absolute`
/// so one host's answer is every host's — an id is written and read on the
/// same machine, but the round-trip is proven on whichever one runs the tests.
pub(super) fn split_module_file_resource_id(id: &str) -> Option<(&str, String)> {
    let (module, rest) = id.split_once('/')?;
    if module.is_empty() || rest.is_empty() {
        return None;
    }
    let drive_rooted = matches!(rest.as_bytes(), [d, b':', ..] if d.is_ascii_alphabetic());
    let target = if drive_rooted {
        rest.to_string()
    } else {
        format!("/{rest}")
    };
    Some((module, target))
}

pub(super) fn module_file_verify_results(
    fm: &CfgdFileManager,
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
    modules: &[ResolvedModule],
) -> anyhow::Result<Vec<VerifyResult>> {
    let mut results = Vec::new();
    for module in modules {
        for file in &module.files {
            let drift = match &file.patch {
                // A `Patch` file has no source to compare against: it has
                // converged when re-running its merge over the target's current
                // content would change nothing.
                Some(spec) => {
                    let binding = module_patch_binding(config_dir, resolved, module);
                    let evaluated = cfgd_core::reconciler::evaluate_patch(
                        spec,
                        &file.target,
                        &binding.context(),
                    );
                    crate::files::patch_drift_result(&file.target, evaluated)
                }
                None => fm.file_drift_one(&file.source, &file.target, None, file.strategy)?,
            };
            results.push(VerifyResult {
                resource_type: "module".to_string(),
                resource_id: module_file_resource_id(&module.name, &drift.target),
                matches: drift.matches,
                expected: drift.expected,
                actual: drift.actual,
            });
        }
    }
    Ok(results)
}

/// Non-matching live verify results across every category the live scan covers
/// (profile files, module files, packages, system, declared env vars and
/// aliases). Read-only: this performs a live scan (the same checks `diff`
/// runs) but never writes to the `drift_events` table, so a `status --scan`
/// call stays a non-recording dashboard query.
///
/// Only divergent results are returned — the caller treats a non-empty vector as
/// "drift detected" and renders each entry. This is the single source of truth
/// for both `status --scan`'s rendered Drift section and the `-e` exit gate, so
/// the human verdict can never contradict the exit code.
pub(super) fn live_drift_results(
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    modules: &[ResolvedModule],
    cfgd_installed: &std::collections::HashSet<String>,
    state: &cfgd_core::state::StateStore,
    cx: &cfgd_core::providers::PackageContext<'_>,
) -> anyhow::Result<Vec<VerifyResult>> {
    // One spinner across the whole scan, narrated per pass via `set_message`.
    cx.printer.narrate("Scanning: profile files", |sp| {
        live_drift_results_inner(
            config_dir,
            resolved,
            registry,
            modules,
            cfgd_installed,
            state,
            cx,
            sp,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn live_drift_results_inner(
    config_dir: &std::path::Path,
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    modules: &[ResolvedModule],
    cfgd_installed: &std::collections::HashSet<String>,
    state: &cfgd_core::state::StateStore,
    cx: &cfgd_core::providers::PackageContext<'_>,
    sp: &mut cfgd_core::output::Spinner<'_>,
) -> anyhow::Result<Vec<VerifyResult>> {
    let mut drift = Vec::new();

    // One file manager for both file halves of the scan.
    let fm = CfgdFileManager::new(config_dir, resolved)?;

    // Files: content-aware comparison via the file manager.
    drift.extend(
        file_verify_results(&fm, resolved)?
            .into_iter()
            .filter(|r| !r.matches),
    );

    // Module files: content-aware comparison for each resolved module.
    sp.set_message("Scanning: module files");
    drift.extend(
        module_file_verify_results(&fm, config_dir, resolved, modules)?
            .into_iter()
            .filter(|r| !r.matches),
    );

    // Packages: any non-Skip action means the installed set diverges from desired.
    sp.set_message("Scanning: packages");
    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers()
        .iter()
        .map(|m| m.as_ref())
        .collect();
    let pkg_actions =
        packages::plan_packages(&resolved.merged, modules, &all_managers, cfgd_installed, cx)?;
    for action in &pkg_actions {
        if let Some(result) = package_action_drift(action) {
            drift.push(result);
        }
    }

    // Managers: a manager the plan would provision or refuse is itself drift —
    // the same signal `diff`'s `cfgd:managers` group renders, from the same
    // planner, so `verify`/`status --scan` cannot disagree with `diff` about
    // whether a missing manager is live drift.
    sp.set_message("Scanning: managers");
    for ma in manager_drift_actions(cfgd_core::reconciler::plan_managers(
        registry,
        &pkg_actions,
        &[],
    )) {
        drift.extend(manager_action_drift(&ma));
    }

    // System: any configurator reporting a non-empty diff is drift. The desired
    // map combines profile and module system config so module system tweaks
    // surface here exactly as they do on the write path.
    sp.set_message("Scanning: system");
    let system = cfgd_core::effective::effective_system_map(&resolved.merged, modules);
    for configurator in &registry.available_system_configurators() {
        if let Some(desired) = system.get(configurator.name()) {
            // A configurator that errors while probing is treated as
            // indeterminate, not drift — surfacing it as drift here would make a
            // transient probe failure flip the exit code. The display path
            // (`diff`/`verify`) reports such errors to the user.
            if let Ok(drifts) = configurator.diff(desired) {
                for d in &drifts {
                    drift.push(VerifyResult {
                        resource_type: "system".to_string(),
                        resource_id: cfgd_core::reconciler::system_resource_key(
                            configurator.name(),
                            &d.key,
                        ),
                        matches: false,
                        expected: d.expected.clone(),
                        actual: d.actual.clone(),
                    });
                }
            }
        }
    }

    // Env & aliases: whether the primary managed env file still holds the line
    // each declared alias and env var renders as, using the same
    // generator-and-compare check `verify` persists as drift. Read-only like
    // every other pass here — only the recording half in `reconciler::verify`
    // writes to `drift_events`. `path_dirs` must be the same recorded
    // bootstrap directories `cfgd verify` passes: the whole-file check bundled
    // into `env_verify_results` compares against a freshly generated file, and
    // the file cfgd actually wrote carries the bootstrapped `PATH` export line
    // as its first line — an empty `path_dirs` here would generate content
    // that never matches a converged machine's file, permanently.
    sp.set_message("Scanning: env & aliases");
    let path_dirs =
        cfgd_core::reconciler::recorded_manager_path_dirs(state, &resolved.merged, modules);
    drift.extend(
        cfgd_core::reconciler::env_verify_results(
            &resolved.merged.env,
            &resolved.merged.aliases,
            resolved.merged.env_scope,
            modules,
            &path_dirs,
        )
        .into_iter()
        .filter(|r| !r.matches),
    );

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
            resource_id: super::diff::package_resource_id(manager, packages),
            matches: false,
            expected: "installed".to_string(),
            actual: cfgd_core::Absence::NotInstalled.to_string(),
        }),
        PackageAction::Uninstall {
            manager, packages, ..
        } => Some(VerifyResult {
            resource_type: "package".to_string(),
            resource_id: super::diff::package_resource_id(manager, packages),
            matches: false,
            expected: "absent".to_string(),
            actual: "to remove".to_string(),
        }),
    }
}

/// Filter a planner's [`Action`]s down to the [`ManagerAction`]s that are
/// themselves drift: a manager the plan would provision or refuse. The single
/// predicate `diff`'s `cfgd:managers` group and every live-check surface
/// share, so "is this ManagerAction drift" has one answer instead of two
/// filters that could disagree. `RefreshIndex`/`Prerequisite` are excluded:
/// neither is something the user declared and can be *missing* — they run
/// every apply regardless of drift, so surfacing them here would flag a fresh
/// clone as drifted even when nothing diverges.
pub(in crate::cli) fn manager_drift_actions(actions: Vec<Action>) -> Vec<ManagerAction> {
    actions
        .into_iter()
        .filter_map(|a| match a {
            Action::Manager(
                ma @ (ManagerAction::Provision { .. } | ManagerAction::Refuse { .. }),
            ) => Some(ma),
            _ => None,
        })
        .collect()
}

/// How a drifted manager stands, in the ONE phrasing every surface renders it
/// in: the state it is in, and what can be done about it.
///
/// Two surfaces say this fact and they must say it the same way — `diff` as a
/// status line (`<manager>: not installed` with the detail after it) and
/// `verify` / `status --scan` folded into a [`VerifyResult::actual`]. Derived here
/// rather than at each of them, because a reader matching a `verify` row
/// against the `diff` that explains it is matching the same words.
pub(in crate::cli) struct ManagerDriftPhrase {
    /// What the manager's state IS, with no subject — the `diff` line prepends
    /// `<manager>: ` and the `actual` string stands alone.
    pub(in crate::cli) state: &'static str,
    /// What can be done about it: `can bootstrap via <method>`, or
    /// `cannot bootstrap: <reason>`.
    pub(in crate::cli) detail: String,
}

/// The phrase for one drift [`ManagerAction`], or `None` for a node that is not
/// drift at all — a refresh and a prerequisite run every apply regardless, so
/// neither has a state to report. [`manager_drift_actions`] already filters
/// both out; answering `None` rather than panicking keeps that filter an
/// optimisation instead of a precondition a second caller has to know about.
pub(in crate::cli) fn manager_drift_phrase(action: &ManagerAction) -> Option<ManagerDriftPhrase> {
    match action {
        ManagerAction::RefreshIndex { .. } | ManagerAction::Prerequisite { .. } => None,
        ManagerAction::Provision { via, .. } => Some(ManagerDriftPhrase {
            state: cfgd_core::Absence::NotInstalled.as_str(),
            detail: format!("can bootstrap via {via}"),
        }),
        ManagerAction::Refuse { reason, .. } => Some(ManagerDriftPhrase {
            state: cfgd_core::Absence::NotInstalled.as_str(),
            detail: format!("cannot bootstrap: {reason}"),
        }),
    }
}

/// Map a drift [`ManagerAction`] to its `VerifyResult` rows. The `resource_id`
/// uses the journal's own tail grammar for the same fact
/// (`provision:<manager>` / `refuse:<manager>`) rather than the bare manager
/// name, so a `package`-row consumer meets the persisted identity instead of a
/// third grammar.
///
/// A provision that batches several managers onto one install yields one row
/// PER manager: each of them is missing from the host, and a reader asking
/// `verify` whether pipx is installed must not be answered only about the
/// manager whose name the batch happens to carry.
fn manager_action_drift(action: &ManagerAction) -> Vec<VerifyResult> {
    let Some(phrase) = manager_drift_phrase(action) else {
        return Vec::new();
    };
    let row = |resource_id: String| VerifyResult {
        resource_type: "package".to_string(),
        resource_id,
        matches: false,
        expected: "installed".to_string(),
        actual: format!("{} ({})", phrase.state, phrase.detail),
    };
    let provisioned = action.provisioned_managers();
    if provisioned.is_empty() {
        return vec![row(action.resource_id())];
    }
    provisioned
        .iter()
        .map(|m| row(ManagerAction::provision_resource_id(m)))
        .collect()
}

/// Manager-drift half of [`live_drift_results`], usable standalone by
/// `cmd_verify` — which needs manager provisioning/refusal drift but drives
/// its file/system/module checks through `reconciler::verify`, not this
/// module's file functions. Computes its own package-action plan rather than
/// accepting one, so it's a single self-contained call for the caller.
pub(super) fn manager_verify_results(
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    modules: &[ResolvedModule],
    cfgd_installed: &std::collections::HashSet<String>,
    cx: &cfgd_core::providers::PackageContext<'_>,
) -> anyhow::Result<Vec<VerifyResult>> {
    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers()
        .iter()
        .map(|m| m.as_ref())
        .collect();
    let pkg_actions =
        packages::plan_packages(&resolved.merged, modules, &all_managers, cfgd_installed, cx)?;
    Ok(manager_drift_actions(cfgd_core::reconciler::plan_managers(
        registry,
        &pkg_actions,
        &[],
    ))
    .iter()
    .flat_map(manager_action_drift)
    .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cfgd_core::config::{
        FileStrategy, FilesSpec, LayerPolicy, ManagedFileSpec, MergedProfile, ProfileLayer,
        ProfileSpec, ResolvedProfile, ShellAlias,
    };
    use cfgd_core::output::Printer;

    use super::*;

    fn resolved_with_file(target: std::path::PathBuf) -> ResolvedProfile {
        let files = FilesSpec {
            managed: vec![ManagedFileSpec {
                patch: None,
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
        let results = file_verify_results(
            &CfgdFileManager::new(dir.path(), &resolved).unwrap(),
            &resolved,
        )
        .unwrap();
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
        let results = file_verify_results(
            &CfgdFileManager::new(dir.path(), &resolved).unwrap(),
            &resolved,
        )
        .unwrap();
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
        let results = file_verify_results(
            &CfgdFileManager::new(dir.path(), &resolved).unwrap(),
            &resolved,
        )
        .unwrap();
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
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &[],
            &std::collections::HashSet::new(),
            &state,
            &cx,
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
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &[],
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();
        assert!(
            drift.is_empty(),
            "matching file + empty packages/system must be no-drift: {drift:?}"
        );
    }

    /// `status --scan` / `verify` share this engine, and it never checked
    /// `spec.aliases` at all before the Env pass was wired in — an alias
    /// hand-edited on the machine was invisible to a live scan even though
    /// `cfgd verify`'s recording half already caught it. Prove the shared
    /// engine now reports the same mismatch this read-only path is meant to
    /// surface.
    #[test]
    #[serial_test::serial]
    fn live_drift_results_includes_a_hand_edited_alias() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // Dialect is platform-dependent, so the hand-edited line is derived
        // from `env_item_declared_line` (production's per-item renderer for
        // the running platform) instead of a hardcoded POSIX literal.
        let hand_edited = ShellAlias {
            name: "ll".to_string(),
            command: "ls -lah".to_string(),
        };
        let hand_edited_line = cfgd_core::reconciler::env_item_declared_line(
            "alias",
            "ll",
            &[],
            std::slice::from_ref(&hand_edited),
            &[],
        )
        .expect("alias renders a declared line");
        std::fs::write(
            tmp_home
                .path()
                .join(crate::cli::helpers::tests::primary_env_file_name()),
            format!("# managed by cfgd \u{2014} do not edit\n{hand_edited_line}\n"),
        )
        .unwrap();

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".to_string(),
                profile_name: "test".to_string(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                aliases: vec![ShellAlias {
                    name: "ll".to_string(),
                    command: "ls -la".to_string(),
                }],
                ..Default::default()
            },
        };
        let dir = tempfile::tempdir().unwrap();
        let registry = crate::cli::build_registry_with_profile(&resolved.merged.packages);
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &[],
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();
        let alias_row = drift
            .iter()
            .find(|r| r.resource_type == "alias" && r.resource_id == "ll")
            .expect("a hand-edited alias must appear in the live scan");
        assert!(!alias_row.matches);
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
                patch: None,
            }],
            env: Vec::new(),
            aliases: Vec::new(),
            system: std::collections::BTreeMap::new(),
            pre_apply_scripts: Vec::new(),
            post_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            on_drift_scripts: Vec::new(),
            depends: Vec::new(),
            dir: std::path::PathBuf::new(),
            platform_skip_reason: None,
            origin: None,
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
        let results = module_file_verify_results(
            &CfgdFileManager::new(dir.path(), &resolved).unwrap(),
            dir.path(),
            &resolved,
            &modules,
        )
        .unwrap();
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
        let results = module_file_verify_results(
            &CfgdFileManager::new(dir.path(), &resolved).unwrap(),
            dir.path(),
            &resolved,
            &modules,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            !results[0].matches,
            "tampered module file must fail: {results:?}"
        );
        assert!(results[0].actual.contains("differs"));
    }

    #[test]
    fn module_file_verify_results_patch_reports_drift_and_convergence() {
        // A `Patch` module file has no source to compare against, so its
        // verify result comes from re-evaluating the merge over the target.
        // Covers both `cfgd status --scan` and `cfgd verify`, which share
        // this function.
        let dir = tempfile::tempdir().unwrap();
        let drifted = dir.path().join("drifted.json");
        std::fs::write(&drifted, "{\n  \"keep\": 1\n}\n").unwrap();
        let converged = dir.path().join("converged.json");
        std::fs::write(&converged, "{\n  \"telemetry\": false\n}\n").unwrap();

        let resolved = resolved_with_file(dir.path().join("unused.txt"));
        let mut modules = vec![module_with_file(
            "accmod",
            std::path::PathBuf::new(),
            drifted,
        )];
        let spec = cfgd_core::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str("telemetry: false").unwrap()),
            script: None,
            blocked_by: None,
        };
        modules[0].files[0].strategy = Some(FileStrategy::Patch);
        modules[0].files[0].patch = Some(spec.clone());
        modules[0].files.push(cfgd_core::modules::ResolvedFile {
            source: std::path::PathBuf::new(),
            target: converged,
            is_git_source: false,
            strategy: Some(FileStrategy::Patch),
            encryption: None,
            permissions: None,
            patch: Some(spec),
        });

        let results = module_file_verify_results(
            &CfgdFileManager::new(dir.path(), &resolved).unwrap(),
            dir.path(),
            &resolved,
            &modules,
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        assert!(!results[0].matches, "drifted Patch target must fail");
        assert_eq!(results[0].resource_type, "module");
        assert!(results[0].resource_id.starts_with("accmod/"));
        assert!(results[1].matches, "converged Patch target must pass");
    }

    #[test]
    fn module_file_verify_results_reports_an_unevaluable_patch_as_drift() {
        // `cfgd verify` / `status --scan` scan every resource: a target
        // cfgd cannot parse is drift, not a reason to abort and hide the rest.
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "{ this is not json").unwrap();

        let resolved = resolved_with_file(dir.path().join("unused.txt"));
        let mut modules = vec![module_with_file(
            "accmod",
            std::path::PathBuf::new(),
            broken,
        )];
        modules[0].files[0].strategy = Some(FileStrategy::Patch);
        modules[0].files[0].patch = Some(cfgd_core::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str("telemetry: false").unwrap()),
            script: None,
            blocked_by: None,
        });

        let results = module_file_verify_results(
            &CfgdFileManager::new(dir.path(), &resolved).unwrap(),
            dir.path(),
            &resolved,
            &modules,
        )
        .expect("one unevaluable file must not fail the scan");
        assert_eq!(results.len(), 1);
        assert!(!results[0].matches);
        assert!(
            results[0].actual.starts_with("cannot evaluate patch spec:"),
            "the failure is surfaced per-file, got: {}",
            results[0].actual
        );
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
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();
        assert_eq!(drift.len(), 1, "only the module file drifts: {drift:?}");
        assert_eq!(drift[0].resource_type, "module");
    }

    /// A resolved profile with no managed files (so only packages/system can
    /// drive drift) for the module-package / module-system live-drift tests.
    fn resolved_no_files() -> ResolvedProfile {
        ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".to_string(),
                profile_name: "test".to_string(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile::default(),
        }
    }

    /// A `ResolvedModule` carrying a single package, no files.
    fn module_with_package(name: &str, manager: &str, pkg: &str) -> ResolvedModule {
        ResolvedModule {
            name: name.to_string(),
            packages: vec![cfgd_core::modules::ResolvedPackage {
                canonical_name: pkg.to_string(),
                resolved_name: pkg.to_string(),
                manager: manager.to_string(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            }],
            files: Vec::new(),
            env: Vec::new(),
            aliases: Vec::new(),
            system: std::collections::BTreeMap::new(),
            pre_apply_scripts: Vec::new(),
            post_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            on_drift_scripts: Vec::new(),
            depends: Vec::new(),
            dir: std::path::PathBuf::new(),
            platform_skip_reason: None,
            origin: None,
        }
    }

    #[test]
    fn live_drift_results_includes_module_only_package() {
        // A module-only package the host lacks must register as live drift, via
        // the effective desired set the package planner now consumes. Built with
        // a hand-wired registry (available mock manager, package not installed)
        // so the result is host-independent.
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolved_no_files();

        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(
            cfgd_core::test_helpers::MockPackageManager::new("brew").with_installed(&[]),
        ));

        let modules = vec![module_with_package("dev", "brew", "ripgrep")];
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();

        assert!(
            drift
                .iter()
                .any(|r| r.resource_type == "package" && r.resource_id.contains("ripgrep")),
            "module-only package must register as live drift: {drift:?}"
        );
    }

    #[test]
    fn live_drift_results_includes_a_provisionable_manager() {
        // A missing manager the plan CAN self-heal must still surface as live
        // drift — otherwise `verify`/`status --scan` say "converged" on a host
        // `apply` would still change, the same gap `diff` closed for its own
        // `cfgd:managers` group.
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolved_no_files();

        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(
            cfgd_core::test_helpers::MockPackageManager::new("npm")
                .unavailable()
                .bootstrappable_via("pip install npm-bootstrap"),
        ));

        let modules = vec![module_with_package("dev", "npm", "left-pad")];
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();

        let manager_row = drift
            .iter()
            .find(|r| r.resource_type == "package" && r.resource_id == "provision:npm")
            .unwrap_or_else(|| panic!("a provisionable manager must register as drift: {drift:?}"));
        assert_eq!(
            manager_row.actual, "not installed (can bootstrap via pip install npm-bootstrap)",
            "must name the method `diff` would show, got: {manager_row:?}"
        );
    }

    #[test]
    fn live_drift_results_includes_a_refused_manager() {
        // A manager the plan cannot self-heal (no path to its prerequisite
        // tool) must still register as drift, distinguishable from the
        // provisionable case by its reason rather than being silently dropped.
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolved_no_files();

        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(
            cfgd_core::test_helpers::MockPackageManager::new("npm")
                .unavailable()
                .bootstrappable_via("pip install npm-bootstrap")
                .requiring(&["a-tool-nothing-provides"]),
        ));

        let modules = vec![module_with_package("dev", "npm", "left-pad")];
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();

        let manager_row = drift
            .iter()
            .find(|r| r.resource_type == "package" && r.resource_id == "refuse:npm")
            .unwrap_or_else(|| panic!("a refused manager must register as drift too: {drift:?}"));
        assert!(
            manager_row.actual.contains("cannot bootstrap")
                && manager_row.actual.contains("a-tool-nothing-provides"),
            "must name why, distinct from the provisionable wording, got: {manager_row:?}"
        );
    }

    // `cmd_verify` folds `manager_verify_results` straight into its `results`
    // vector and derives BOTH its exit code (`fail_count = results.iter()
    // .filter(|r| !r.matches).count()`, `has_drift = fail_count > 0`) and its
    // `-o json` `results` array from that same vector with no reshaping — so
    // pinning `matches: false` and the row shape here pins what `verify -e`
    // would exit with and what `verify -o json` would print, without needing
    // to trigger the real `ExitCode::exit()` (`-> !`) inside a test process.
    // `cfgd verify` asks the resource half (`reconciler::verify`) and the manager
    // half (`manager_verify_results`, which plans packages) about the same
    // machine. Given one context, one manager answers once for both halves and
    // for every package in them; given a context per half — which is what
    // `cmd_verify` built before — the same manager is enumerated twice.
    #[test]
    fn both_halves_of_verify_share_one_enumeration_per_manager() {
        // The count is a memo-hit claim, so the memo's age ceiling is pinned out
        // of reach — unpinned it rests on the 30s wall clock. No serialization:
        // nothing in this crate's test binary pins the ceiling to zero, and a
        // longer ceiling can only let another test's entries live longer.
        let _ttl = cfgd_core::test_helpers::EnumerationMemoTtlGuard::never_expires();
        let enumerations = cfgd_core::test_helpers::measured_in_a_stable_generation(|| {
            let mgr = cfgd_core::test_helpers::MockPackageManager::new("npm")
                .with_installed(&["left-pad", "chalk"]);
            let enumerations = mgr.enumeration_counter();
            let mut registry = ProviderRegistry::new();
            registry.add_package_manager(Box::new(mgr));

            let resolved = resolved_no_files();
            let modules = vec![
                module_with_package("dev", "npm", "left-pad"),
                module_with_package("web", "npm", "chalk"),
            ];
            let (printer, _cap) = Printer::for_test_doc();
            let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
            let cx = cfgd_core::providers::PackageContext::new(&printer, &state);

            cfgd_core::reconciler::verify(&resolved, &registry, &state, &modules, &cx).unwrap();
            manager_verify_results(
                &resolved,
                &registry,
                &modules,
                &std::collections::HashSet::new(),
                &cx,
            )
            .unwrap();

            enumerations.load(std::sync::atomic::Ordering::SeqCst)
        });

        assert_eq!(
            enumerations, 1,
            "both verify halves must read one enumeration per manager"
        );
    }

    #[test]
    fn manager_verify_results_flags_a_provisionable_manager_as_drift() {
        let resolved = resolved_no_files();
        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(
            cfgd_core::test_helpers::MockPackageManager::new("npm")
                .unavailable()
                .bootstrappable_via("pip install npm-bootstrap"),
        ));
        let modules = vec![module_with_package("dev", "npm", "left-pad")];
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);

        let results = manager_verify_results(
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
            &cx,
        )
        .unwrap();

        let row = results
            .iter()
            .find(|r| r.resource_id == "provision:npm")
            .unwrap_or_else(|| panic!("a provisionable manager must reach verify -e: {results:?}"));
        assert_eq!(row.resource_type, "package");
        assert!(
            !row.matches,
            "must fail verify — this is what flips exit code 5"
        );
        assert_eq!(
            row.actual, "not installed (can bootstrap via pip install npm-bootstrap)",
            "must name the method, same as diff/status, got: {row:?}"
        );
    }

    #[test]
    fn manager_verify_results_flags_a_refused_manager_as_drift() {
        let resolved = resolved_no_files();
        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(
            cfgd_core::test_helpers::MockPackageManager::new("npm")
                .unavailable()
                .bootstrappable_via("pip install npm-bootstrap")
                .requiring(&["a-tool-nothing-provides"]),
        ));
        let modules = vec![module_with_package("dev", "npm", "left-pad")];
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);

        let results = manager_verify_results(
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
            &cx,
        )
        .unwrap();

        let row = results
            .iter()
            .find(|r| r.resource_id == "refuse:npm")
            .unwrap_or_else(|| panic!("a refused manager must reach verify -e too: {results:?}"));
        assert_eq!(row.resource_type, "package");
        assert!(
            !row.matches,
            "must fail verify — this is what flips exit code 5"
        );
        assert!(
            row.actual
                .contains("cannot bootstrap: a-tool-nothing-provides"),
            "must name the refusal reason, got: {row:?}"
        );
    }

    /// One unprovisionable manager, read on both surfaces that report it.
    ///
    /// `diff` renders a status line and `verify`/`status --scan` a `VerifyResult`,
    /// and the two used to word the same fact differently (`cannot bootstrap:
    /// <reason>` against `not installed (cannot bootstrap — <reason>)`), so a
    /// reader matching a verify row against the diff explaining it met two
    /// spellings of one refusal. Captured from the real renders rather than
    /// from the derivation, so re-hardcoding either surface's words fails here.
    #[test]
    fn a_refused_manager_reads_identically_on_diff_and_on_verify() {
        let action = ManagerAction::Refuse {
            manager: "snap".into(),
            reason: "no available system manager".into(),
        };
        let phrase = manager_drift_phrase(&action).expect("a refusal is drift");

        let (printer, cap) = Printer::for_test_doc();
        let mut payload = crate::cli::output_types::DiffOutput::default();
        {
            let section =
                printer.section_phase(&cfgd_core::reconciler::PhaseName::Packages.section_label());
            crate::cli::diff::print_package_drift(
                &[],
                std::slice::from_ref(&action),
                &section,
                &cfgd_core::reconciler::Owner::profile("tiny"),
                &mut payload,
            );
        }
        drop(printer);
        // The manager name and its qualifier now render in separate theme
        // slots (subject / muted qualifier), so a raw substring match would
        // see the SGR reset between them; strip before asserting on content.
        let rendered = cfgd_core::output::strip_ansi(&cap.human());

        let rows = manager_action_drift(&action);
        let row = rows.first().expect("a refusal is drift");
        assert!(
            rendered.contains(&format!("snap: {}", phrase.state))
                && rendered.contains(&phrase.detail),
            "the diff line must be the shared phrase, got: {rendered}"
        );
        assert_eq!(
            row.actual,
            format!("{} ({})", phrase.state, phrase.detail),
            "the verify row must be the same phrase, parenthesised"
        );
    }

    #[test]
    fn live_drift_results_includes_module_only_system_tweak() {
        // A module-only system tweak must register as live drift, via the
        // effective system map the system loop now consumes. The configurator
        // drifts only when its key is in the desired map — declaring it ONLY in
        // a module proves the module system config is read.
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolved_no_files();

        let mut registry = ProviderRegistry::new();
        registry.add_system_configurator(Box::new(
            cfgd_core::test_helpers::MockSystemConfigurator::new("sysctl").with_drift(vec![
                cfgd_core::providers::SystemDrift {
                    key: "vm.swappiness".to_string(),
                    expected: "10".to_string(),
                    actual: "60".to_string(),
                },
            ]),
        ));

        let mut module = module_with_package("dev", "brew", "ignored");
        module.packages = Vec::new();
        module.system.insert(
            "sysctl".to_string(),
            serde_yaml::to_value(serde_yaml::Mapping::new()).unwrap(),
        );

        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &[module],
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();

        assert!(
            drift
                .iter()
                .any(|r| r.resource_type == "system" && r.resource_id == "sysctl.vm.swappiness"),
            "module-only system tweak must register as live drift: {drift:?}"
        );
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
        let (printer, _cap) = Printer::for_test_doc();
        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        let cx = cfgd_core::providers::PackageContext::new(&printer, &state);
        let drift = live_drift_results(
            dir.path(),
            &resolved,
            &registry,
            &modules,
            &std::collections::HashSet::new(),
            &state,
            &cx,
        )
        .unwrap();
        assert!(
            drift.is_empty(),
            "clean module file must not drift: {drift:?}"
        );
    }

    /// The id a finding carries and the pair a row renders from are one
    /// composition read in two directions: a target that does not survive the
    /// round trip is a drift row naming a path that never existed.
    #[test]
    fn a_module_file_id_round_trips_back_to_its_module_and_target() {
        for target in ["/home/user/.zshrc", "C:/Users/user/.zshrc"] {
            let id = module_file_resource_id("nvim", target);
            assert_eq!(
                split_module_file_resource_id(&id),
                Some(("nvim", target.to_string())),
                "round trip failed for {target}"
            );
        }
    }

    #[test]
    fn an_id_with_no_target_half_names_nothing() {
        assert_eq!(split_module_file_resource_id("nvim"), None);
        assert_eq!(split_module_file_resource_id("nvim/"), None);
        assert_eq!(split_module_file_resource_id("/home/user/x"), None);
    }
}
