use super::*;
use crate::config::ScriptCommand;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use crate::PathDisplayExt;
use crate::config::*;
use crate::providers::{ActionNote, PackageContext, PackageManager};

use crate::providers::StubPackageManager as MockPackageManager;
use crate::test_helpers::{
    MockSecretBackend, MockSecretProvider, MockSystemConfigurator, make_empty_resolved,
    make_resolved_module, test_package_context, test_printer, test_state,
};

/// Plan item strings for a whole phase, in the plan's own order.
fn plan_items(phase: &Phase) -> Vec<String> {
    phase.actions().map(format_plan_item).collect()
}

#[test]
fn empty_plan_has_no_phases() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    // An action-less phase is dropped, so a plan with nothing to do carries no
    // phases at all rather than eight empty ones.
    assert_eq!(plan.phases.len(), 0);
    assert!(plan.is_empty());
}

#[test]
fn plan_includes_package_actions() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(!plan.is_empty());
    assert_eq!(plan.total_actions(), 1);
}

#[test]
fn plan_includes_file_actions() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let file_actions = vec![FileAction::Create {
        source: PathBuf::from("/src/test"),
        target: PathBuf::from("/dst/test"),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::default(),
        source_hash: None,
        patch: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            file_actions,
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(!plan.is_empty());
    assert_eq!(plan.total_actions(), 1);
}

#[test]
fn plan_includes_script_actions() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.pre_reconcile = vec![ScriptEntry::Simple("scripts/pre.sh".to_string())];
    resolved.merged.scripts.post_reconcile =
        vec![ScriptEntry::Simple("scripts/post.sh".to_string())];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Reconcile,
        )
        .unwrap();

    // Pre-scripts phase should have the pre_reconcile script
    let pre_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::PreScripts)
        .unwrap();
    assert_eq!(pre_phase.action_count(), 1);

    // Post-scripts phase should have the post_reconcile script
    let post_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::PostScripts)
        .unwrap();
    assert_eq!(post_phase.action_count(), 1);
}

#[test]
fn apply_empty_plan_records_success() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Empty plan — no actions means success with 0 results
    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 0);
}

/// Build a two-file Create plan plus the source files, returning the dir guard,
/// the two targets, and the planned `Plan`.
fn two_file_create_plan(
    reconciler: &Reconciler<'_>,
    resolved: &ResolvedProfile,
) -> (tempfile::TempDir, PathBuf, PathBuf, Plan) {
    let dir = tempfile::tempdir().unwrap();
    let src_a = dir.path().join("a.src");
    let src_b = dir.path().join("b.src");
    let tgt_a = dir.path().join("a.txt");
    let tgt_b = dir.path().join("b.txt");
    std::fs::write(&src_a, "alpha").unwrap();
    std::fs::write(&src_b, "beta").unwrap();

    let file_actions = vec![
        FileAction::Create {
            source: src_a,
            target: tgt_a.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
            patch: None,
        },
        FileAction::Create {
            source: src_b,
            target: tgt_b.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
            patch: None,
        },
    ];

    let plan = reconciler
        .plan(
            resolved,
            file_actions,
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();
    (dir, tgt_a, tgt_b, plan)
}

#[test]
fn apply_aborts_before_first_action_when_flag_preset() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let (dir, tgt_a, tgt_b, plan) = two_file_create_plan(&reconciler, &resolved);

    // Abort requested BEFORE apply begins → zero actions run.
    let abort = crate::AbortFlag::new();
    abort.set(130);

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            true,
            None,
            &abort,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Aborted);
    assert_eq!(result.aborted, Some(130));
    assert_eq!(result.succeeded(), 0, "no action should have run");
    // Unfiltered: planned_total equals the whole plan.
    assert_eq!(result.planned_total, 2);
    // Neither target written, and no temp/torn file left behind.
    assert!(
        !tgt_a.exists(),
        "first target must be untouched on pre-abort"
    );
    assert!(
        !tgt_b.exists(),
        "second target must be untouched on pre-abort"
    );
    let leftover: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp") || n.ends_with('~'))
        .collect();
    assert!(leftover.is_empty(), "no torn temp files, got: {leftover:?}");

    // The applies row reflects the abort.
    let record = state.last_apply().unwrap().unwrap();
    assert_eq!(record.status, ApplyStatus::Aborted);
}

#[test]
fn apply_not_aborted_applies_everything_and_records_success() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let (dir, tgt_a, tgt_b, plan) = two_file_create_plan(&reconciler, &resolved);

    // Flag never set → regression: full apply, Success.
    let abort = crate::AbortFlag::new();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            true,
            None,
            &abort,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.aborted, None);
    assert_eq!(result.succeeded(), 2);
    assert!(tgt_a.exists(), "first target must be written");
    assert!(tgt_b.exists(), "second target must be written");

    let record = state.last_apply().unwrap().unwrap();
    assert_eq!(record.status, ApplyStatus::Success);
}

#[test]
fn aborted_planned_total_counts_only_filtered_actions() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let dir = tempfile::tempdir().unwrap();
    let src_a = dir.path().join("a.src");
    let src_b = dir.path().join("b.src");
    std::fs::write(&src_a, "alpha").unwrap();
    std::fs::write(&src_b, "beta").unwrap();

    // A 3-action plan across two phases: 2 file actions + 1 package action. A
    // `--phase files` filter keeps only the 2 file actions in scope.
    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Files,
                &Owner::profile("test"),
                vec![
                    Action::File(FileAction::Create {
                        source: src_a,
                        target: dir.path().join("a.txt"),
                        origin: "local".to_string(),
                        strategy: crate::config::FileStrategy::Copy,
                        source_hash: None,
                        patch: None,
                    }),
                    Action::File(FileAction::Create {
                        source: src_b,
                        target: dir.path().join("b.txt"),
                        origin: "local".to_string(),
                        strategy: crate::config::FileStrategy::Copy,
                        source_hash: None,
                        patch: None,
                    }),
                ],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Package(PackageAction::Install {
                    manager: "brew".to_string(),
                    packages: vec!["ripgrep".to_string()],
                    origin: "local".to_string(),
                })],
            ),
        ],
        warnings: vec![],
    };

    // Abort before any action runs; filter scopes to the Files phase only.
    let abort = crate::AbortFlag::new();
    abort.set(130);

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &[],
            ReconcileContext::Apply,
            true,
            None,
            &abort,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Aborted);
    assert_eq!(result.succeeded(), 0);
    // Honest total: the 2 in-scope file actions, NOT the global plan size of 3.
    assert_eq!(
        result.planned_total, 2,
        "planned_total must count only filter-surviving actions"
    );
}

#[test]
fn phase_name_roundtrip() {
    for name in &[
        PhaseName::PreScripts,
        PhaseName::Prerequisites,
        PhaseName::Modules,
        PhaseName::Packages,
        PhaseName::System,
        PhaseName::Files,
        PhaseName::Secrets,
        PhaseName::PostScripts,
    ] {
        let s = name.as_str();
        let parsed = PhaseName::from_str(s).unwrap();
        assert_eq!(&parsed, name);
    }
}

#[test]
fn format_plan_items_for_display() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("test"),
        vec![
            Action::Package(PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["ripgrep".to_string(), "fd".to_string()],
                origin: "local".to_string(),
            }),
            Action::Package(PackageAction::Skip {
                manager: "apt".to_string(),
                reason: "not available".to_string(),
                origin: "local".to_string(),
            }),
        ],
    );

    let items = plan_items(&phase);
    assert_eq!(items.len(), 2); // Skip items are now shown
    assert!(items[0].contains("ripgrep"));
    assert!(items[1].contains("skip apt: not available"));
}

#[test]
fn verify_returns_results() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    registry.add_package_manager(Box::new(
        MockPackageManager::new("cargo").with_installed(&["ripgrep"]),
    ));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.cargo = Some(crate::config::CargoSpec {
        file: None,
        packages: vec!["ripgrep".to_string(), "bat".to_string()],
    });

    let printer = test_printer();
    let results = verify(
        &resolved,
        &registry,
        &state,
        &[],
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    // ripgrep should be present, bat should be missing
    let rg = results
        .iter()
        .find(|r| r.resource_id == "cargo:ripgrep")
        .unwrap();
    assert!(rg.matches);

    let bat = results
        .iter()
        .find(|r| r.resource_id == "cargo:bat")
        .unwrap();
    assert!(!bat.matches);
}

#[test]
#[serial_test::serial(enumeration_memo)]
fn verify_asks_each_manager_once_however_many_packages_are_declared() {
    // Measured in a stable generation: anything in this binary that installs
    // something retires the memo, and the second package's read would then
    // legitimately re-enumerate.
    //
    // The count is a memo-hit claim, so the memo's age ceiling is pinned out of
    // reach and the pin is serialized — it is process-global, and a sibling test
    // pins it to zero.
    let _ttl = crate::test_helpers::EnumerationMemoTtlGuard::never_expires();
    let (package_results, enumerations) =
        crate::test_helpers::measured_in_a_stable_generation(|| {
            let state = test_state();
            let mut registry = ProviderRegistry::new();
            let mgr =
                crate::test_helpers::MockPackageManager::new("cargo").with_installed(&["ripgrep"]);
            let enumerations = mgr.enumeration_counter();
            registry.add_package_manager(Box::new(mgr));

            let mut resolved = make_empty_resolved();
            resolved.merged.packages.cargo = Some(crate::config::CargoSpec {
                file: None,
                packages: vec![
                    "ripgrep".to_string(),
                    "bat".to_string(),
                    "fd".to_string(),
                    "jq".to_string(),
                ],
            });

            let printer = test_printer();
            let cx = crate::providers::PackageContext::new(&printer, &state);
            let results = verify(&resolved, &registry, &state, &[], &cx, true)
                .unwrap()
                .results;

            (
                results
                    .iter()
                    .filter(|r| r.resource_type == "package")
                    .count(),
                enumerations.load(std::sync::atomic::Ordering::SeqCst),
            )
        });

    assert_eq!(package_results, 4);
    assert_eq!(
        enumerations, 1,
        "four declared packages under one manager is one question"
    );
}

#[test]
#[serial_test::serial(enumeration_memo)]
fn one_context_spans_the_verify_walk_and_the_tracking_gc_with_one_enumeration() {
    // The count is a memo-hit claim, so the memo's age ceiling is pinned out of
    // reach and the pin is serialized — it is process-global, and a sibling test
    // pins it to zero.
    let _ttl = crate::test_helpers::EnumerationMemoTtlGuard::never_expires();
    let (stale, enumerations) = crate::test_helpers::measured_in_a_stable_generation(|| {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        let mgr =
            crate::test_helpers::MockPackageManager::new("cargo").with_installed(&["ripgrep"]);
        let enumerations = mgr.enumeration_counter();
        registry.add_package_manager(Box::new(mgr));

        let mut resolved = make_empty_resolved();
        resolved.merged.packages.cargo = Some(crate::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".to_string()],
        });

        let printer = test_printer();
        let cx = crate::providers::PackageContext::new(&printer, &state);
        verify(&resolved, &registry, &state, &[], &cx, true).unwrap();

        // What `cfgd verify` and `cfgd apply` both do next: diff the tracking
        // table against the same installed state, with nothing in between that
        // could have changed it.
        let managers = registry.available_package_managers();
        let tracked: std::collections::HashSet<String> =
            ["cargo/ripgrep".to_string(), "cargo/gone".to_string()].into();
        let stale = crate::reconciler::stale_tracked_packages(&managers, &tracked, &cx).unwrap();

        (
            stale,
            enumerations.load(std::sync::atomic::Ordering::SeqCst),
        )
    });

    assert_eq!(stale, vec![("cargo".to_string(), "gone".to_string())]);
    assert_eq!(
        enumerations, 1,
        "two phases of one run sharing a context is one question per manager"
    );
}

#[test]
fn the_tracking_gc_re_enumerates_once_the_run_has_installed_something() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    let mgr = crate::test_helpers::MockPackageManager::new("cargo").with_installed(&["ripgrep"]);
    let enumerations = mgr.enumeration_counter();
    registry.add_package_manager(Box::new(mgr));

    let printer = test_printer();
    let cx = crate::providers::PackageContext::new(&printer, &state);
    let managers = registry.available_package_managers();
    let tracked: std::collections::HashSet<String> = ["cargo/ripgrep".to_string()].into();

    crate::reconciler::stale_tracked_packages(&managers, &tracked, &cx).unwrap();
    // The exec path's own signal that the machine moved under the run.
    crate::invalidate_command_resolution();
    crate::reconciler::stale_tracked_packages(&managers, &tracked, &cx).unwrap();

    assert_eq!(
        enumerations.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a GC that ran after an install must read the machine as the install left it"
    );
}

#[test]
fn plan_hash_string() {
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Package(PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["ripgrep".to_string()],
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };
    let hash = plan.to_hash_string();
    assert!(!hash.is_empty());
    assert_eq!(
        hash,
        plan.to_hash_string(),
        "plan hash must be deterministic"
    );
}

#[test]
fn apply_result_counts() {
    let result = ApplyResult {
        action_results: vec![
            ActionResult {
                phase: "files".to_string(),
                description: "test".to_string(),
                success: true,
                error: None,
                changed: true,
                skipped: false,
                not_attempted: None,
                installed: None,
                versions: Default::default(),
            },
            ActionResult {
                phase: "files".to_string(),
                description: "test2".to_string(),
                success: false,
                error: Some("failed".to_string()),
                changed: false,
                skipped: false,
                not_attempted: None,
                installed: None,
                versions: Default::default(),
            },
        ],
        status: ApplyStatus::Partial,
        apply_id: 0,
        aborted: None,
        planned_total: 2,
        caveats: Vec::new(),
    };

    assert_eq!(result.succeeded(), 1);
    assert_eq!(result.failed(), 1);
}

// --- Module integration tests ---

use crate::modules::{ResolvedFile, ResolvedModule, ResolvedPackage};

#[test]
fn plan_routes_module_packages_into_the_packages_phase() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![make_resolved_module("nvim")];
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let module_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Packages)
        .unwrap();

    assert!(!module_phase.is_empty());

    // Check that actions are ModuleAction
    for action in module_phase.actions() {
        match action {
            Action::Module(ma) => {
                assert_eq!(ma.module_name, "nvim");
            }
            _ => panic!("expected Module action in the Packages phase"),
        }
    }
}

#[test]
fn plan_module_with_files() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("nvim-config");
    std::fs::write(&source, "config").unwrap();

    let modules = vec![ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source,
            target: PathBuf::from("/home/user/.config/nvim"),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let module_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Files)
        .unwrap();
    assert_eq!(module_phase.action_count(), 1);

    match module_phase
        .actions()
        .next()
        .expect("phase holds an action")
    {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::DeployFiles { files, .. } => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].target, PathBuf::from("/home/user/.config/nvim"));
            }
            _ => panic!("expected DeployFiles action"),
        },
        _ => panic!("expected Module action"),
    }
}

#[test]
fn plan_module_with_scripts() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![
            ScriptEntry::Simple("nvim --headless +qa".to_string()),
            ScriptEntry::Simple("echo done".to_string()),
        ],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let module_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::PostScripts)
        .unwrap();
    assert_eq!(module_phase.action_count(), 2);

    for action in module_phase.actions() {
        match action {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::RunScript { script, .. } => {
                    assert!(!script.run_str().is_empty());
                }
                _ => panic!("expected RunScript action"),
            },
            _ => panic!("expected Module action"),
        }
    }
}

#[test]
fn plan_multiple_modules_in_dependency_order() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![
        ResolvedModule {
            name: "node".to_string(),
            packages: vec![ResolvedPackage {
                canonical_name: "nodejs".to_string(),
                resolved_name: "nodejs".to_string(),
                manager: "apt".to_string(),
                manager_declared: false,
                version: Some("18.19.0".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            on_drift_scripts: Vec::new(),
            system: BTreeMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
            origin: None,
            platform_skip_reason: None,
        },
        ResolvedModule {
            name: "nvim".to_string(),
            packages: vec![ResolvedPackage {
                canonical_name: "neovim".to_string(),
                resolved_name: "neovim".to_string(),
                manager: "brew".to_string(),
                manager_declared: false,
                version: Some("0.10.2".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            on_drift_scripts: Vec::new(),
            system: BTreeMap::new(),
            depends: vec!["node".to_string()],
            dir: PathBuf::from("."),
            origin: None,
            platform_skip_reason: None,
        },
    ];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    // Each module gets its own group inside the one Packages phase — two
    // modules never merge into one group even when both declare only packages.
    let packages = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Packages)
        .expect("packages phase");
    assert_eq!(packages.groups().len(), 2);
    assert_eq!(packages.action_count(), 2);

    let owners: Vec<&Owner> = packages.groups().iter().map(|g| &g.owner).collect();
    assert_eq!(owners, vec![&Owner::module("node"), &Owner::module("nvim")]);
    for group in packages.groups() {
        match group.actions.first().expect("group holds an action") {
            Action::Module(ma) => assert_eq!(ma.module_name, group.owner.name),
            other => panic!("expected Module action, got {other:?}"),
        }
    }
}

/// F2: a module declaring packages across two managers of the same
/// availability class (both unresolved against an empty registry, so both
/// fall into the "unknown" tier) used to route through
/// `by_manager.keys().collect()` — a `HashMap`, whose key order is
/// `RandomState` and reshuffled per process. `sort_by_key` is stable, so
/// same-class managers kept whatever order the `HashMap` handed them: the
/// plan tree's bullet order, the `-o json` payload order, the journal
/// `action_index`, and the phase's execution offer order all drew from it.
/// Runs `plan()` many times over one process on a fixed module and asserts
/// the two `InstallPackages` actions land in the same (alphabetical by
/// manager name) order every time.
#[test]
fn plan_package_actions_order_ties_by_manager_name_every_run() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module = ResolvedModule {
        name: "toolchain".to_string(),
        packages: vec![
            ResolvedPackage {
                canonical_name: "typescript".to_string(),
                resolved_name: "typescript".to_string(),
                manager: "npm".to_string(),
                manager_declared: false,
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            },
            ResolvedPackage {
                canonical_name: "ripgrep".to_string(),
                resolved_name: "ripgrep".to_string(),
                manager: "cargo".to_string(),
                manager_declared: false,
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            },
        ],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    };

    for _ in 0..50 {
        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                vec![module.clone()],
                ReconcileContext::Apply,
            )
            .unwrap();

        let packages = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Packages)
            .expect("packages phase");
        let managers: Vec<&str> = packages
            .actions()
            .map(|action| match action {
                Action::Module(ma) => match &ma.kind {
                    ModuleActionKind::InstallPackages { resolved } => resolved[0].manager.as_str(),
                    other => panic!("expected InstallPackages, got {other:?}"),
                },
                other => panic!("expected Module action, got {other:?}"),
            })
            .collect();
        assert_eq!(
            managers,
            vec!["cargo", "npm"],
            "same-class managers must tie-break on name, identically every run"
        );
    }
}

#[test]
fn plan_routes_module_work_to_the_phase_of_its_kind() {
    // packages + files + postApply script → three consecutive, correctly
    // ordered Modules phases, each scoped to its own section.
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("nvim-config");
    std::fs::write(&source, "config").unwrap();

    let modules = vec![ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "neovim".to_string(),
            resolved_name: "neovim".to_string(),
            manager: "brew".to_string(),
            manager_declared: false,
            version: Some("0.10.2".to_string()),
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![ResolvedFile {
            source,
            target: PathBuf::from("/home/user/.config/nvim/init.lua"),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![ScriptEntry::Simple("nvim --headless +qa".to_string())],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let owned = |name: PhaseName| -> Vec<(&Owner, &Action)> {
        plan.phases
            .iter()
            .filter(|p| p.name == name)
            .flat_map(|p| p.owned_actions())
            .collect()
    };

    assert!(
        !plan.phases.iter().any(|p| p.name == PhaseName::Modules),
        "a module with real work leaves the meta phase empty: {:?}",
        plan.phases
    );

    for (phase, expect) in [
        (PhaseName::Packages, "InstallPackages"),
        (PhaseName::Files, "DeployFiles"),
        (PhaseName::PostScripts, "RunScript"),
    ] {
        let actions = owned(phase.clone());
        assert_eq!(actions.len(), 1, "one nvim action in {phase:?}");
        let (owner, action) = actions[0];
        assert_eq!(owner, &Owner::module("nvim"), "owned by the module");
        let matches = matches!(
            (action, expect),
            (
                Action::Module(ModuleAction {
                    kind: ModuleActionKind::InstallPackages { .. },
                    ..
                }),
                "InstallPackages"
            ) | (
                Action::Module(ModuleAction {
                    kind: ModuleActionKind::DeployFiles { .. },
                    ..
                }),
                "DeployFiles"
            ) | (
                Action::Module(ModuleAction {
                    kind: ModuleActionKind::RunScript { .. },
                    ..
                }),
                "RunScript"
            )
        );
        assert!(matches, "expected {expect} in {phase:?}, got {action:?}");
    }
}

/// A `ResolvedModule` carrying exactly one package, distinct from any other
/// call's package name/manager pair. `dedup_module_packages` claims packages
/// by `(manager, resolved_name)` across the WHOLE plan, so two modules built
/// from the fixture-sharing `make_resolved_module` (both "neovim"/"ripgrep"
/// on "brew") collapse into one module's action — the second module's
/// packages get claimed away as duplicates and its phase never appears. This
/// helper keeps package identities disjoint so both modules' phases survive.
fn resolved_module_with_package(name: &str, pkg: &str, manager: &str) -> ResolvedModule {
    ResolvedModule {
        name: name.to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: pkg.to_string(),
            resolved_name: pkg.to_string(),
            manager: manager.to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }
}

#[test]
fn plan_two_modules_with_packages_get_one_group_each() {
    // Two independent modules, each with only a packages action: consecutive-
    // run splitting must not merge them into one "packages" run just because
    // the section repeats back to back — every phase's scope must name the
    // right module in module order.
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![
        resolved_module_with_package("alpha", "alpha-tool", "apt"),
        resolved_module_with_package("beta", "beta-tool", "brew"),
    ];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let packages = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Packages)
        .expect("packages phase");
    let owners: Vec<&Owner> = packages.groups().iter().map(|g| &g.owner).collect();
    assert_eq!(
        owners,
        vec![&Owner::module("alpha"), &Owner::module("beta")],
        "one group per module, never merged: {:?}",
        packages.groups()
    );
    for group in packages.groups() {
        assert_eq!(group.actions.len(), 1, "one install per module");
    }
}

#[test]
fn phase_modules_filter_selects_module_work_from_every_kind_phase() {
    // `--phase modules` is an OWNER filter after the kind routing: a module's
    // packages, files and scripts sit in three different phases, so a filter
    // that compared phase names would select at most one of them.
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![
        resolved_module_with_package("alpha", "alpha-tool", "apt"),
        resolved_module_with_package("beta", "beta-tool", "brew"),
    ];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let mut matched_modules: Vec<&str> = Vec::new();
    for phase_item in &plan.phases {
        for (owner, action) in phase_item.owned_actions() {
            if action_matches_phase_filter(
                &phase_item.name,
                owner,
                action,
                &PhaseFilter::ModuleOwners,
            ) && let Action::Module(ma) = action
            {
                matched_modules.push(ma.module_name.as_str());
            }
        }
    }
    matched_modules.sort_unstable();
    assert_eq!(
        matched_modules,
        vec!["alpha", "beta"],
        "`--phase modules` selects module work in every kind-phase it routed to"
    );
}

#[test]
fn format_module_plan_items_packages() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: ModuleActionKind::InstallPackages {
                resolved: vec![
                    ResolvedPackage {
                        canonical_name: "neovim".to_string(),
                        resolved_name: "neovim".to_string(),
                        manager: "brew".to_string(),
                        manager_declared: false,
                        version: Some("0.10.2".to_string()),
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: None,
                    },
                    ResolvedPackage {
                        canonical_name: "fd".to_string(),
                        resolved_name: "fd-find".to_string(),
                        manager: "apt".to_string(),
                        manager_declared: false,
                        version: Some("8.7.0".to_string()),
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: None,
                    },
                ],
            },
            origin: None,
        })],
    );

    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    // Exact string: manager groups must render in first-appearance order of
    // the resolved list — this description is also the plan payload, and a
    // hashed grouping reshuffled multi-manager modules on every plan.
    assert_eq!(
        items[0],
        "brew install neovim (0.10.2); apt install fd-find (8.7.0, alias: fd)"
    );
}

#[test]
fn format_module_plan_items_files() {
    let phase = Phase::from_actions(
        PhaseName::Files,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: {
                let files = vec![ResolvedFile {
                    source: PathBuf::from("/cache/nvim/config"),
                    target: PathBuf::from("/home/user/.config/nvim"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                }];
                let declared_total = files.len();
                ModuleActionKind::DeployFiles {
                    files,
                    declared_total,
                }
            },
            origin: None,
        })],
    );

    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].starts_with("deploy "));
    assert!(items[0].contains(".config/nvim"));
}

#[test]
fn format_module_plan_items_skip() {
    let phase = Phase::from_actions(
        PhaseName::Modules,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction {
            module_name: "bad".to_string(),
            kind: ModuleActionKind::Skip {
                reason: "dependency not met".to_string(),
            },
            origin: None,
        })],
    );

    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0], "skip: dependency not met");
}

#[test]
fn format_module_action_description() {
    let action = Action::Module(ModuleAction {
        module_name: "nvim".to_string(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![ResolvedPackage {
                canonical_name: "neovim".to_string(),
                resolved_name: "neovim".to_string(),
                manager: "brew".to_string(),
                manager_declared: false,
                version: Some("0.10.2".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            }],
        },
        origin: None,
    });

    let desc = format_action_description(&action);
    assert!(desc.starts_with("module:nvim:packages:"));
    assert!(desc.contains("neovim"));
}

#[test]
fn module_state_stored_after_apply() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim"]),
    ));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![make_resolved_module("nvim")];
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules.clone(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let _result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Module state should be recorded
    let module_state = state.module_state_by_name("nvim").unwrap();
    assert!(module_state.is_some());
    let ms = module_state.unwrap();
    assert_eq!(ms.module_name, "nvim");
    assert_eq!(ms.status, "installed");
    assert!(!ms.packages_hash.is_empty());
    assert!(!ms.files_hash.is_empty());
}

#[test]
fn module_state_upsert_and_remove() {
    let state = test_state();

    state
        .upsert_module_state("nvim", None, "hash1", "hash2", None, "installed")
        .unwrap();

    let ms = state.module_state_by_name("nvim").unwrap().unwrap();
    assert_eq!(ms.packages_hash, "hash1");
    assert_eq!(ms.status, "installed");

    // Update
    state
        .upsert_module_state(
            "nvim",
            None,
            "hash3",
            "hash4",
            Some("[{\"url\":\"test\"}]"),
            "outdated",
        )
        .unwrap();

    let ms = state.module_state_by_name("nvim").unwrap().unwrap();
    assert_eq!(ms.packages_hash, "hash3");
    assert_eq!(ms.status, "outdated");
    assert!(ms.git_sources.is_some());

    // List all
    let all = state.module_states().unwrap();
    assert_eq!(all.len(), 1);

    // Remove
    state.remove_module_state("nvim").unwrap();
    assert!(state.module_state_by_name("nvim").unwrap().is_none());
}

#[test]
fn verify_module_drift_packages() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    // ripgrep is NOT installed — should drift
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim"]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![make_resolved_module("nvim")];
    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    // Should have a drift result for ripgrep, under the ONE per-package key
    // every live check spells (`<manager>:<name>`), module-declared or not.
    let drift = results
        .iter()
        .find(|r| r.resource_type == "package" && r.resource_id == "brew:ripgrep");
    assert!(drift.is_some());
    assert!(!drift.unwrap().matches);

    // neovim is installed → a passing per-package row (not absent, and not drift).
    let ok = results
        .iter()
        .find(|r| r.resource_type == "package" && r.resource_id == "brew:neovim");
    assert!(
        ok.is_some(),
        "installed module package must emit a pass row"
    );
    assert!(ok.unwrap().matches);

    // Pure compute: nothing lands in `drift_events` — recording is the
    // caller's, at the CLI seam, per its own scope.
    assert!(state.unresolved_drift().unwrap().is_empty());
}

#[test]
fn phase_name_modules_roundtrip() {
    let s = PhaseName::Modules.as_str();
    assert_eq!(s, "modules");
    let parsed = PhaseName::from_str(s).unwrap();
    assert_eq!(parsed, PhaseName::Modules);
    assert_eq!(PhaseName::Modules.display_name(), "Modules");
}

#[test]
fn plan_hash_includes_module_actions() {
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "nvim".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "neovim".to_string(),
                        resolved_name: "neovim".to_string(),
                        manager: "brew".to_string(),
                        manager_declared: false,
                        version: Some("0.10.2".to_string()),
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: None,
                    }],
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let hash = plan.to_hash_string();
    assert!(hash.contains("nvim"));
    assert!(hash.contains("neovim"));
    assert!(hash.contains("brew"));
}

#[test]
fn verify_module_all_installed_emits_per_package_pass_rows() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim", "ripgrep"]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![make_resolved_module("nvim")];
    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    // All packages installed → a passing per-package row each (no blanket
    // "module healthy" row, which would contradict folded-in file-drift rows).
    for pkg in ["brew:neovim", "brew:ripgrep"] {
        let row = results
            .iter()
            .find(|r| r.resource_type == "package" && r.resource_id == pkg);
        assert!(row.is_some(), "expected pass row for {pkg}: {results:?}");
        assert!(row.unwrap().matches);
        assert_eq!(row.unwrap().expected, "installed");
    }

    // The blanket healthy row is removed.
    let blanket = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "nvim");
    assert!(
        blanket.is_none(),
        "no blanket module healthy row: {results:?}"
    );

    // No drift entries.
    let drifts: Vec<_> = results
        .iter()
        .filter(|r| r.resource_type == "module" && !r.matches)
        .collect();
    assert!(drifts.is_empty());
}

#[test]
fn verify_routes_through_package_identity_for_name_remapping_manager() {
    // go installs `rsc.io/2fa` but lists the binary `2fa`. verify must compare
    // the desired name through package_identity, else the installed binary reads
    // as missing and reconcile reports permanent phantom drift. Reverting the
    // `package_identity` wire at verify.rs (raw `installed.contains(&ep.name)`)
    // turns this red — the guard for the case-insensitive/remapping routing.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    let mut go = TrackingPackageManager::with_installed("go", &["2fa"]);
    go.identity_strip = true;
    registry.add_package_manager(Box::new(go));

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![ResolvedModule {
        name: "gotools".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "2fa".to_string(),
            resolved_name: "rsc.io/2fa".to_string(),
            manager: "go".to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;
    let row = results
        .iter()
        .find(|r| r.resource_type == "package" && r.resource_id == "go:rsc.io/2fa")
        .expect("expected a verify row for the go package");
    assert!(
        row.matches,
        "installed binary `2fa` must match desired `rsc.io/2fa` through package_identity: {results:?}"
    );
    assert_eq!(row.actual, "installed");
}

#[test]
fn verify_module_script_packages_not_false_drift() {
    // Script-based packages should not cause false drift reports since
    // "script" isn't a registered package manager in the registry.
    let state = test_state();
    let registry = ProviderRegistry::new(); // no managers

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![ResolvedModule {
        name: "rustup".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "rustup".to_string(),
            resolved_name: "rustup".to_string(),
            manager: "script".to_string(),
            manager_declared: false,
            version: None,
            script: Some("curl -sSf https://sh.rustup.rs | sh".into()),
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    // Script packages are skipped in verification — they produce no row at all
    // (neither pass nor drift), so a script-only module yields no rows.
    assert!(
        results.is_empty(),
        "script-only module must not produce verify rows: {results:?}"
    );
}

/// Build a single-package `ResolvedModule` (no defaults) for verify tests.
fn module_one_pkg(name: &str, manager: &str, pkg: &str) -> ResolvedModule {
    let mut m = make_resolved_module(name);
    m.packages = vec![ResolvedPackage {
        canonical_name: pkg.to_string(),
        resolved_name: pkg.to_string(),
        manager: manager.to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    }];
    m
}

/// [`module_one_pkg`] with the entry's declared `minVersion` floor.
fn module_one_pinned_pkg(
    name: &str,
    manager: &str,
    pkg: &str,
    min_version: &str,
) -> ResolvedModule {
    let mut m = module_one_pkg(name, manager, pkg);
    m.packages[0].min_version = Some(min_version.to_string());
    m
}

/// A declaration that pins a version is checked against the version the
/// machine HOLDS, not against presence alone: a host carrying `ripgrep 1.0.0`
/// under a module declaring `minVersion: 2` is drifted, and the row states
/// both operands (`want: 2, have: 1.0.0`) so the reader can see the gap
/// without a second command.
///
/// The row keeps the SAME `<manager>:<name>` id the presence row mints — the
/// version fact lives in the operands — or the resolve/keep machinery, which
/// matches on the recorded id, would strand it the moment the machine
/// converged.
#[test]
fn a_pinned_package_below_its_bound_is_version_drift() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed_at("ripgrep", "1.0.0"),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pinned_pkg("dev", "brew", "ripgrep", "2")];

    let report = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap();

    let rows: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.resource_type == "package" && r.resource_id == "brew:ripgrep")
        .collect();
    let drifted = rows
        .iter()
        .find(|r| !r.matches)
        .unwrap_or_else(|| panic!("a package below its floor must be drift: {rows:?}"));
    assert_eq!(drifted.expected, "2", "the row states the declared floor");
    assert_eq!(
        drifted.actual, "1.0.0",
        "the row states the version the machine holds"
    );
    assert_eq!(
        crate::output::drift_terse_cause("package", &drifted.expected, &drifted.actual),
        "version mismatch",
        "two comparable versions read as a version mismatch"
    );
    assert!(
        report.check_errors.is_empty(),
        "a version the manager stated is not a check error: {:?}",
        report.check_errors
    );
}

/// The complement: a declaration that pins nothing asks nothing about
/// versions. A host holding an old copy of an UNPINNED package is converged —
/// the declaration never said otherwise — so presence stays the whole
/// question and no second row appears under the package's one id.
#[test]
fn an_unpinned_package_stays_a_presence_only_check() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed_at("ripgrep", "1.0.0"),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pkg("dev", "brew", "ripgrep")];

    let report = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap();

    let rows: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.resource_type == "package")
        .collect();
    assert_eq!(rows.len(), 1, "one row per declared package: {rows:?}");
    assert!(rows[0].matches, "an installed unpinned package is clean");
    assert!(report.check_errors.is_empty());
}

/// A pinned package whose manager states no version is a check that could not
/// RUN, never a silent pass: the floor is neither met nor missed, so the scan
/// reports an erroring check (which every `--exit-code` surface escalates to
/// `Error`) and contributes NO package row — the same discipline an erroring
/// system configurator follows, so its recorded rows stand instead of being
/// healed by a check that never answered.
#[test]
fn a_pinned_package_whose_version_the_manager_cannot_state_is_a_check_error() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    // No `with_installed_version`: the trait default lists every name at
    // `UNKNOWN_PACKAGE_VERSION`, which is what a manager that cannot state a
    // version answers.
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&["ripgrep"]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pinned_pkg("dev", "brew", "ripgrep", "2")];

    let report = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap();

    let err = report
        .check_errors
        .iter()
        .find(|e| e.key == "brew:ripgrep")
        .unwrap_or_else(|| {
            panic!(
                "an unreadable version is a check error: {report:?}",
                report = report.check_errors
            )
        });
    assert!(
        err.error.contains("2"),
        "the detail names the floor it could not judge: {}",
        err.error
    );
    assert!(
        !report
            .results
            .iter()
            .any(|r| r.resource_type == "package" && !r.matches),
        "an errored check contributes no drift verdict of its own: {:?}",
        report.results
    );
    // The floor is what could not be judged; presence was answered, so the
    // ledger keeps that verdict rather than losing the package entirely.
    let presence = report
        .results
        .iter()
        .find(|r| r.resource_type == "package" && r.resource_id == "brew:ripgrep")
        .unwrap_or_else(|| panic!("the presence verdict stands: {:?}", report.results));
    assert!(presence.matches, "the package IS installed: {presence:?}");
}

/// The other half of the same rule: a version the manager DID state but whose
/// scheme its comparator cannot judge is a check that could not run, not a
/// missed floor. A `false` from a comparator that could not parse its input is
/// an artifact, and `VersionFloor::Below`'s operands promise two comparable
/// versions — a terse report reads them as `version mismatch`.
#[test]
fn a_pinned_package_whose_version_nothing_can_compare_is_a_check_error() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed_at("ripgrep", "git-20240101"),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pinned_pkg("dev", "brew", "ripgrep", "2")];

    let report = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap();

    let err = report
        .check_errors
        .iter()
        .find(|e| e.key == "brew:ripgrep")
        .unwrap_or_else(|| {
            panic!(
                "an uncomparable version is a check error, not drift: {:?} / {:?}",
                report.check_errors, report.results
            )
        });
    assert!(
        err.error.contains("git-20240101"),
        "the detail names what the manager stated: {}",
        err.error
    );
    assert!(
        !report
            .results
            .iter()
            .any(|r| r.resource_type == "package" && !r.matches),
        "no drift verdict is invented from an unparseable operand: {:?}",
        report.results
    );
}

/// Two modules declaring one package, only one of them pinning a floor: the
/// claim rule ("earlier module wins") settles who INSTALLS it, but a floor is a
/// constraint the planner enforces per module, so the strictest one survives
/// the dedup. Without this the live check is blind to a floor `cfgd apply`
/// still acts on.
#[test]
fn the_strictest_declared_floor_survives_the_effective_dedup() {
    let resolved = make_empty_resolved();
    let bare = module_one_pkg("base", "brew", "ripgrep");
    let pinned = module_one_pinned_pkg("dev", "brew", "ripgrep", "2");
    let stricter = module_one_pinned_pkg("strict", "brew", "ripgrep", "3");

    let effective = crate::effective::effective_desired_packages(
        &resolved.merged,
        &[bare, pinned.clone(), stricter],
    );
    assert_eq!(effective.len(), 1, "one entry per package: {effective:?}");
    assert_eq!(
        effective[0].min_version.as_deref(),
        Some("3"),
        "the strictest floor any module declared survives: {effective:?}"
    );

    let unpinned_last = crate::effective::effective_desired_packages(
        &resolved.merged,
        &[pinned, {
            let mut m = module_one_pkg("base", "brew", "ripgrep");
            m.name = "later".to_string();
            m
        }],
    );
    assert_eq!(
        unpinned_last[0].min_version.as_deref(),
        Some("2"),
        "a later module declaring no floor does not drop one: {unpinned_last:?}"
    );
}

#[test]
fn verify_module_package_not_installed_is_package_drift() {
    // A module-only package the host lacks must surface as a `package`
    // non-match under the ONE `<manager>:<name>` key — the same identity
    // `diff`/`status --scan` mint, so the two full checks heal each other's
    // rows instead of churning them.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&[]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pkg("dev", "brew", "ripgrep")];

    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    let row = results
        .iter()
        .find(|r| r.resource_type == "package" && r.resource_id == "brew:ripgrep")
        .expect("module package must emit a per-package row");
    assert!(!row.matches, "uninstalled module package must be drift");
    assert!(
        !results
            .iter()
            .any(|r| r.resource_id.contains("dev/ripgrep")),
        "no module-qualified package key may survive: {results:?}"
    );

    // Pure compute: nothing lands in `drift_events` — recording is the
    // caller's, at the CLI seam, per its own scope.
    assert!(state.unresolved_drift().unwrap().is_empty());
}

#[test]
fn verify_package_in_profile_and_module_appears_once() {
    // The same (manager, name) declared by both the profile and a module must
    // verify once, under the one `<manager>:<name>` key both origins share.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&[]),
    ));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.brew = Some(crate::config::BrewSpec {
        formulae: vec!["ripgrep".to_string()],
        ..Default::default()
    });
    let printer = test_printer();
    let modules = vec![module_one_pkg("dev", "brew", "ripgrep")];

    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    let rows: Vec<_> = results
        .iter()
        .filter(|r| r.resource_id.contains("ripgrep"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "duplicate profile+module package must verify once: {rows:?}"
    );
    assert_eq!(rows[0].resource_type, "package");
    assert_eq!(rows[0].resource_id, "brew:ripgrep");
}

#[test]
fn verify_module_package_on_unavailable_manager_is_skipped() {
    // CONSISTENCY: a module package whose manager is unavailable on this host
    // cannot be installed or probed here, so it must NOT be reported missing —
    // matching how profile packages on unavailable managers are already skipped.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(MockPackageManager::new("brew").unavailable()));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pkg("dev", "brew", "ripgrep")];

    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    assert!(
        !results.iter().any(|r| r.resource_id.contains("ripgrep")),
        "unavailable-manager module package must be skipped, not reported missing: {results:?}"
    );
    assert!(
        state.unresolved_drift().unwrap().is_empty(),
        "no false drift may be recorded for an unavailable manager"
    );
}

#[test]
fn verify_profile_package_on_unavailable_manager_is_skipped() {
    // The profile-origin half of the same consistency rule.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(MockPackageManager::new("brew").unavailable()));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.brew = Some(crate::config::BrewSpec {
        formulae: vec!["ripgrep".to_string()],
        ..Default::default()
    });
    let printer = test_printer();

    let results = verify(
        &resolved,
        &registry,
        &state,
        &[],
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    assert!(
        !results
            .iter()
            .any(|r| r.resource_type == "package" && r.resource_id == "brew:ripgrep"),
        "unavailable-manager profile package must be skipped: {results:?}"
    );
}

#[test]
fn verify_module_system_tweak_surfaces_as_system_drift() {
    // A system configurator that drifts is only consulted when the desired map
    // has its key. Declaring that key ONLY in a module proves verify now reads
    // the effective (profile ⊕ modules) system map.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
        vec![crate::providers::SystemDrift {
            key: "vm.swappiness".to_string(),
            expected: "10".to_string(),
            actual: "60".to_string(),
        }],
    )));

    // Profile has NO system config; the module contributes the sysctl key.
    let resolved = make_empty_resolved();
    let printer = test_printer();
    let mut module = make_resolved_module("dev");
    module.packages = Vec::new();
    module.system.insert(
        "sysctl".to_string(),
        serde_yaml::to_value(serde_yaml::Mapping::new()).unwrap(),
    );

    let results = verify(
        &resolved,
        &registry,
        &state,
        &[module],
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    let row = results
        .iter()
        .find(|r| r.resource_type == "system" && r.resource_id == "sysctl.vm.swappiness")
        .expect("module system config must be verified via the effective map");
    assert!(!row.matches);
    assert_eq!(row.expected, "10");
    assert_eq!(row.actual, "60");
}

#[test]
fn verify_without_machine_surfaces_skips_the_system_and_env_halves() {
    // A module-scoped caller composes module-only config; diffed against the
    // live configurator state or the deployed env files, that is a claim
    // about the machine no single module can vouch for. `machine_surfaces:
    // false` is how the scoped caller keeps both halves out of the results
    // it renders, judges and records.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
        vec![crate::providers::SystemDrift {
            key: "vm.swappiness".to_string(),
            expected: "10".to_string(),
            actual: "60".to_string(),
        }],
    )));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let mut module = make_resolved_module("dev");
    module.packages = Vec::new();
    module.system.insert(
        "sysctl".to_string(),
        serde_yaml::to_value(serde_yaml::Mapping::new()).unwrap(),
    );
    module.env = vec![crate::config::EnvVar {
        name: "DEV_FLAG".to_string(),
        value: "on".to_string(),
        platforms: vec![],
    }];

    let results = verify(
        &resolved,
        &registry,
        &state,
        &[module],
        &crate::providers::PackageContext::new(&printer, &state),
        false,
    )
    .unwrap()
    .results;

    assert!(
        results.is_empty(),
        "a scoped verify computes neither system nor env rows: {results:?}"
    );
}

#[test]
fn plan_module_with_script_packages() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "rustup".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "rustup".to_string(),
            resolved_name: "rustup".to_string(),
            manager: "script".to_string(),
            manager_declared: false,
            version: None,
            script: Some("curl -sSf https://sh.rustup.rs | sh".into()),
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let module_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Packages)
        .unwrap();
    assert_eq!(module_phase.action_count(), 1);

    match module_phase
        .actions()
        .next()
        .expect("phase holds an action")
    {
        Action::Module(ma) => {
            assert_eq!(ma.module_name, "rustup");
            match &ma.kind {
                ModuleActionKind::InstallPackages { resolved } => {
                    assert_eq!(resolved.len(), 1);
                    assert_eq!(resolved[0].manager, "script");
                    assert!(resolved[0].script.is_some());
                }
                _ => panic!("expected InstallPackages action"),
            }
        }
        _ => panic!("expected Module action"),
    }
}

#[test]
fn format_module_plan_script_packages() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction {
            module_name: "rustup".to_string(),
            kind: ModuleActionKind::InstallPackages {
                resolved: vec![ResolvedPackage {
                    canonical_name: "rustup".to_string(),
                    resolved_name: "rustup".to_string(),
                    manager: "script".to_string(),
                    manager_declared: false,
                    version: None,
                    script: Some("install-rustup.sh".into()),
                    creates: None,
                    only_if: None,
                    unless: None,
                    min_version: None,
                }],
            },
            origin: None,
        })],
    );

    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("script"));
    assert!(items[0].contains("rustup"));
}

#[test]
fn empty_modules_produces_no_module_phase() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    // No modules ⇒ no module actions ⇒ the phase is dropped rather than carried
    // as an empty one.
    assert!(!plan.phases.iter().any(|p| p.name == PhaseName::Modules));
}

#[test]
fn conflict_detection_different_content() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "content A").unwrap();
    std::fs::write(&file_b, "content B").unwrap();

    let target = PathBuf::from("/home/user/.config/app");
    let file_actions = vec![FileAction::Create {
        source: file_a,
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![crate::modules::ResolvedFile {
            source: file_b,
            target,
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("conflict"), "expected conflict error: {err}");
}

#[test]
fn conflict_detection_two_profile_actions_same_target_different_content_errs() {
    // Covers plan.rs's file-conflict detection: two profile FileActions hitting the same
    // target with different content must surface as Conflict.
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "content A").unwrap();
    std::fs::write(&file_b, "DIFFERENT content B").unwrap();

    let target = PathBuf::from("/home/user/.config/app");
    let file_actions = vec![
        FileAction::Create {
            source: file_a,
            target: target.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
            patch: None,
        },
        FileAction::Update {
            source: file_b,
            target,
            diff: String::new(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
            patch: None,
        },
    ];

    let err = Reconciler::detect_file_conflicts(&file_actions, &[])
        .expect_err("profile-vs-profile conflict must error");
    assert!(err.to_string().contains("conflict"));
}

#[test]
fn conflict_detection_two_profile_actions_same_target_identical_content_ok() {
    // The dedup branch: same target, same content hash → no error.
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "identical").unwrap();
    std::fs::write(&file_b, "identical").unwrap();

    let target = PathBuf::from("/home/user/.config/app2");
    let file_actions = vec![
        FileAction::Create {
            source: file_a,
            target: target.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
            patch: None,
        },
        FileAction::Update {
            source: file_b,
            target,
            diff: String::new(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
            patch: None,
        },
    ];

    Reconciler::detect_file_conflicts(&file_actions, &[])
        .expect("identical-content profile actions must NOT conflict");
}

#[test]
fn conflict_detection_identical_content_ok() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "same content").unwrap();
    std::fs::write(&file_b, "same content").unwrap();

    let target = PathBuf::from("/home/user/.config/app");
    let file_actions = vec![FileAction::Create {
        source: file_a,
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![crate::modules::ResolvedFile {
            source: file_b,
            target: target.clone(),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
    assert!(
        result.is_ok(),
        "identical content targeting the same path should NOT conflict: {:?}",
        result.err()
    );
    // Prove the identical-content check is meaningful: different content WOULD conflict
    let file_c = dir.path().join("c.txt");
    std::fs::write(&file_c, "different content").unwrap();
    let conflicting_modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![crate::modules::ResolvedFile {
            source: file_c,
            target: target.clone(),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];
    assert!(
        Reconciler::detect_file_conflicts(&file_actions, &conflicting_modules).is_err(),
        "different content at same target should conflict (proves the Ok was meaningful)"
    );
}

#[test]
fn conflict_detection_no_overlap_ok() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "content A").unwrap();
    std::fs::write(&file_b, "content B").unwrap();

    let target_a = PathBuf::from("/target/a");
    let target_b = PathBuf::from("/target/b");
    let file_actions = vec![FileAction::Create {
        source: file_a.clone(),
        target: target_a,
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![crate::modules::ResolvedFile {
            source: file_b.clone(),
            target: target_b,
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
    assert!(
        result.is_ok(),
        "different targets should not conflict: {:?}",
        result.err()
    );
    // Prove this is meaningful: same target with different content WOULD conflict
    let overlapping_modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![crate::modules::ResolvedFile {
            source: file_b,
            target: PathBuf::from("/target/a"), // same as file_actions target
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];
    assert!(
        Reconciler::detect_file_conflicts(&file_actions, &overlapping_modules).is_err(),
        "different content at same target should conflict (proves the Ok was meaningful)"
    );
}

/// The bash/PowerShell `PATH` fold for a fixture whose declared value uses the
/// POSIX separator, so a generator under test renders the one line production
/// writes rather than a shape no file holds.
fn posix_path_fold(env: &[crate::config::EnvVar]) -> Option<super::FoldedPath> {
    super::primary_folded_path(
        env,
        &[],
        &Default::default(),
        &crate::expand_tilde(std::path::Path::new("~")),
        super::EnvPlatform::Linux,
    )
}

/// The same for a fixture whose declared value uses the Windows separator.
fn windows_path_fold(env: &[crate::config::EnvVar]) -> Option<super::FoldedPath> {
    super::primary_folded_path(
        env,
        &[],
        &Default::default(),
        &crate::expand_tilde(std::path::Path::new("~")),
        super::EnvPlatform::Windows,
    )
}

/// The same for fish, whose entries are quoted so hard that no reference in
/// them expands.
fn fish_path_fold(env: &[crate::config::EnvVar]) -> Option<super::FoldedPath> {
    super::fold_path_line(
        env,
        &[],
        &Default::default(),
        &crate::expand_tilde(std::path::Path::new("~")),
        super::EnvPlatform::Linux,
        None,
    )
}

#[test]
fn generate_env_file_quoted_and_unquoted() {
    let env = vec![
        crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
            platforms: vec![],
        },
        crate::config::EnvVar {
            name: "PATH".into(),
            value: "/usr/local/bin:$PATH".into(),
            platforms: vec![],
        },
    ];
    let content = super::generate_env_file_content(
        &env,
        &[],
        posix_path_fold(&env).as_ref(),
        &Default::default(),
    );
    assert!(content.starts_with("# managed by cfgd"));
    assert!(content.contains("export EDITOR=\"nvim\""));
    // PATH contains $, so double-quoted to allow expansion
    assert!(content.contains("export PATH=\"/usr/local/bin:$PATH\""));
}

#[test]
fn generate_fish_env_splits_path() {
    let env = vec![
        crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
            platforms: vec![],
        },
        crate::config::EnvVar {
            name: "PATH".into(),
            value: "/usr/local/bin:/home/user/.cargo/bin:$PATH".into(),
            platforms: vec![],
        },
    ];
    let content = super::generate_fish_env_content(
        &env,
        &[],
        fish_path_fold(&env).as_ref(),
        &Default::default(),
    );
    assert!(content.starts_with("# managed by cfgd"));
    assert!(content.contains("set -gx EDITOR 'nvim'"));
    // A bare `$PATH` splices fish's own list; a quoted one would be a literal.
    assert!(content.contains("set -gx PATH '/usr/local/bin' '/home/user/.cargo/bin' $PATH"));
}

// A leading (or `:`-prefixed) `~` in an env value is expanded to the absolute
// home by every shell generator: the managed file quotes values, so the shell
// itself never expands tilde and a literal `~/...` would be a broken path.
#[test]
#[serial_test::serial]
fn generate_env_files_expand_leading_tilde() {
    let home = tempfile::tempdir().unwrap();
    crate::with_test_home(home.path(), || {
        let h = home.path().posix().to_string();
        let env = vec![
            crate::config::EnvVar {
                name: "CLIFT_DIR".into(),
                value: "~/.local/share/clift".into(),
                platforms: vec![],
            },
            crate::config::EnvVar {
                name: "PATH".into(),
                value: "~/bin:/usr/bin".into(),
                platforms: vec![],
            },
        ];
        let bash = super::generate_env_file_content(
            &env,
            &[],
            posix_path_fold(&env).as_ref(),
            &Default::default(),
        );
        assert!(bash.contains(&format!("export CLIFT_DIR=\"{h}/.local/share/clift\"")));
        assert!(bash.contains(&format!("export PATH=\"{h}/bin:/usr/bin\"")));

        let fish = super::generate_fish_env_content(
            &env,
            &[],
            fish_path_fold(&env).as_ref(),
            &Default::default(),
        );
        assert!(fish.contains(&format!("set -gx CLIFT_DIR '{h}/.local/share/clift'")));
        assert!(fish.contains(&format!("set -gx PATH '{h}/bin' '/usr/bin'")));

        let ps = super::generate_powershell_env_content(
            &env,
            &[],
            posix_path_fold(&env).as_ref(),
            &Default::default(),
        );
        assert!(ps.contains(&format!("$env:CLIFT_DIR = '{h}/.local/share/clift'")));
    });
}

// Regression: the fish PATH generator must split on the `:` separator of the
// RAW value before tilde expansion. On Windows `~` expands to a drive-prefixed
// home (`C:/Users/...`); splitting post-expansion shattered that drive colon
// into a bogus extra PATH entry. A Linux home path *may* contain a literal `:`,
// which stands in for the Windows drive colon and reproduces the exact shatter
// on Linux: the colon-containing home segment must stay one quoted PATH part.
// Unix-only: the `a:b` directory simulation requires a colon in a dir name,
// which is illegal on Windows. Windows exercises the real drive-colon path
// (`C:/...`) natively via `generate_env_files_expand_leading_tilde`.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn generate_fish_path_keeps_colon_containing_home_intact() {
    let home = tempfile::tempdir().unwrap();
    let coloned = home.path().join("a:b");
    std::fs::create_dir_all(&coloned).unwrap();
    crate::with_test_home(&coloned, || {
        let h = coloned.posix().to_string();
        let env = vec![crate::config::EnvVar {
            name: "PATH".into(),
            value: "~/bin:/usr/bin".into(),
            platforms: vec![],
        }];
        let fish = super::generate_fish_env_content(
            &env,
            &[],
            fish_path_fold(&env).as_ref(),
            &Default::default(),
        );
        assert!(
            fish.contains(&format!("set -gx PATH '{h}/bin' '/usr/bin'")),
            "drive/colon-containing home must stay one PATH part, got: {fish}"
        );
    });
}

#[test]
fn plan_env_empty_when_no_env() {
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &[],
        &[],
        &Default::default(),
        crate::config::EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    )
    .actions;
    assert!(actions.is_empty());
}

#[test]
fn plan_env_module_wins_on_conflict() {
    let profile_env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "vim".into(),
        platforms: vec![],
    }];
    let modules = vec![ResolvedModule {
        name: "nvim".into(),
        packages: vec![],
        files: vec![],
        env: vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
            platforms: vec![],
        }],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];
    // plan_env merges and generates actions — the merged env should have EDITOR=nvim
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &profile_env,
        &[],
        &Default::default(),
        crate::config::EnvScope::Interactive,
        &modules,
        &[],
        &[],
        &[],
        tmp.path(),
    )
    .actions;
    // With non-empty env, there should be at least a WriteEnvFile action
    let has_write = actions
        .iter()
        .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })));
    assert!(has_write, "Expected WriteEnvFile action for non-empty env");
}

#[test]
fn plan_env_generates_file_matching_expected() {
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];

    // Write the expected content to a temp file to simulate "already applied"
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join(".cfgd.env");
    let expected = super::generate_env_file_content(&env, &[], None, &Default::default());
    std::fs::write(&env_path, &expected).unwrap();

    // plan_env checks the real ~/.cfgd.env path, not our temp file,
    // so it will still generate actions. This test validates the content generation.
    assert!(expected.contains("export EDITOR=\"nvim\""));
    assert!(expected.contains("# managed by cfgd"));
}

#[test]
fn phase_name_prerequisites_roundtrip() {
    assert_eq!(PhaseName::Prerequisites.as_str(), "prerequisites");
    assert_eq!(PhaseName::Prerequisites.display_name(), "Prerequisites");
    assert_eq!(
        "prerequisites".parse::<PhaseName>().unwrap(),
        PhaseName::Prerequisites
    );
    // The pre-merge spelling still selects the phase that holds the env work.
    assert_eq!(
        "env".parse::<PhaseName>().unwrap(),
        PhaseName::Prerequisites
    );
}

#[test]
fn generate_env_file_with_aliases() {
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let aliases = vec![
        crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
            platforms: vec![],
        },
        crate::config::ShellAlias {
            name: "ll".into(),
            command: "ls -la".into(),
            platforms: vec![],
        },
    ];
    let content = super::generate_env_file_content(&env, &aliases, None, &Default::default());
    assert!(content.contains("export EDITOR=\"nvim\""));
    assert!(content.contains("alias vim=\"nvim\""));
    assert!(content.contains("alias ll=\"ls -la\""));
}

#[test]
fn generate_fish_env_with_aliases() {
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
        platforms: vec![],
    }];
    let content = super::generate_fish_env_content(&env, &aliases, None, &Default::default());
    assert!(content.contains("set -gx EDITOR 'nvim'"));
    assert!(content.contains("abbr -a vim 'nvim'"));
}

#[test]
fn plan_env_aliases_only() {
    let aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
        platforms: vec![],
    }];
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &[],
        &aliases,
        &Default::default(),
        crate::config::EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    )
    .actions;
    let has_write = actions
        .iter()
        .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })));
    assert!(has_write, "Expected WriteEnvFile action for aliases-only");
}

#[test]
#[cfg(unix)]
fn plan_env_module_alias_wins_on_conflict() {
    let profile_aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "vi".into(),
        platforms: vec![],
    }];
    let modules = vec![ResolvedModule {
        name: "nvim".into(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
            platforms: vec![],
        }],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &[],
        &profile_aliases,
        &Default::default(),
        crate::config::EnvScope::Interactive,
        &modules,
        &[],
        &[],
        &[],
        tmp.path(),
    )
    .actions;
    // Find the WriteEnvFile action and check it has "nvim" not "vi"
    for action in &actions {
        if let Action::Env(EnvAction::WriteEnvFile { content, .. }) = action {
            assert!(
                content.contains("alias vim=\"nvim\""),
                "Module alias should override profile alias"
            );
            assert!(
                !content.contains("alias vim=\"vi\""),
                "Profile alias should be overridden"
            );
            return;
        }
    }
    panic!("Expected WriteEnvFile action");
}

#[test]
fn generate_env_file_alias_escapes_quotes() {
    let aliases = vec![crate::config::ShellAlias {
        name: "greet".into(),
        command: "echo \"hello world\"".into(),
        platforms: vec![],
    }];
    let content = super::generate_env_file_content(&[], &aliases, None, &Default::default());
    assert!(content.contains("alias greet=\"echo \\\"hello world\\\"\""));
}

// --- Secret env injection tests ---

#[test]
fn plan_secrets_envs_only_produces_resolve_env() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("secret-token"),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "vault://secret/data/github#token".to_string(),
        target: None,
        template: None,
        backend: None,
        envs: Some(vec!["GITHUB_TOKEN".to_string()]),
    });

    let actions = reconciler.plan_secrets(&profile);
    // Should produce exactly one ResolveEnv action
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Secret(SecretAction::ResolveEnv { provider, envs, .. }) => {
            assert_eq!(provider, "vault");
            assert_eq!(envs, &["GITHUB_TOKEN"]);
        }
        other => panic!("Expected ResolveEnv, got {:?}", other),
    }
}

#[test]
fn plan_secrets_target_and_envs_produces_both_actions() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("1password").with_resolve_result("ghp_abc123"),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "1password://Vault/GitHub/Token".to_string(),
        target: Some(PathBuf::from("/tmp/github-token")),
        template: None,
        backend: None,
        envs: Some(vec!["GITHUB_TOKEN".to_string()]),
    });

    let actions = reconciler.plan_secrets(&profile);
    // Should produce both a Resolve and a ResolveEnv action
    assert_eq!(actions.len(), 2);
    assert!(
        matches!(&actions[0], Action::Secret(SecretAction::Resolve { .. })),
        "First action should be Resolve, got {:?}",
        actions[0]
    );
    assert!(
        matches!(&actions[1], Action::Secret(SecretAction::ResolveEnv { .. })),
        "Second action should be ResolveEnv, got {:?}",
        actions[1]
    );
}

#[test]
fn plan_env_with_secret_envs_includes_them() {
    let secret_envs = vec![
        ("GITHUB_TOKEN".to_string(), "ghp_abc123".to_string()),
        ("NPM_TOKEN".to_string(), "npm_xyz789".to_string()),
    ];
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &[],
        &[],
        &Default::default(),
        crate::config::EnvScope::Interactive,
        &[],
        &secret_envs,
        &[],
        &[],
        tmp.path(),
    )
    .actions;
    // With non-empty secret envs, there should be at least a WriteEnvFile action
    let has_write = actions
        .iter()
        .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })));
    assert!(has_write, "Expected WriteEnvFile action for secret envs");
}

#[test]
#[cfg(unix)]
fn plan_env_secret_envs_appear_in_generated_content() {
    let regular_env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let secret_envs = vec![("GITHUB_TOKEN".to_string(), "ghp_abc123".to_string())];
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &regular_env,
        &[],
        &Default::default(),
        crate::config::EnvScope::Interactive,
        &[],
        &secret_envs,
        &[],
        &[],
        tmp.path(),
    )
    .actions;

    // Find the WriteEnvFile action and check its content
    for action in &actions {
        if let Action::Env(EnvAction::WriteEnvFile { content, .. }) = action {
            assert!(
                content.contains("export EDITOR=\"nvim\""),
                "Regular env should be present"
            );
            assert!(
                content.contains("export GITHUB_TOKEN=\"ghp_abc123\""),
                "Secret env should be present in content: {}",
                content
            );
            // Secret envs should appear after regular envs
            let editor_pos = content.find("EDITOR").unwrap_or(0);
            let token_pos = content.find("GITHUB_TOKEN").unwrap_or(0);
            assert!(
                token_pos > editor_pos,
                "Secret env should appear after regular env"
            );
            return;
        }
    }
    panic!("Expected WriteEnvFile action");
}

// --- Shell rc conflict detection tests ---

#[test]
fn rc_conflict_env_different_value_warns() {
    let dir = tempfile::tempdir().unwrap();
    let rc = dir.path().join(".bashrc");
    std::fs::write(
        &rc,
        "export EDITOR=\"vim\"\n[ -f ~/.cfgd.env ] && . ~/.cfgd.env\n",
    )
    .unwrap();
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let warnings = super::detect_rc_env_conflicts(&rc, &env, &[]);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("EDITOR"));
    assert!(warnings[0].contains("move it after the source line"));
}

#[test]
fn rc_conflict_env_same_value_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let rc = dir.path().join(".bashrc");
    std::fs::write(
        &rc,
        "export EDITOR=\"nvim\"\n[ -f ~/.cfgd.env ] && . ~/.cfgd.env\n",
    )
    .unwrap();
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let warnings = super::detect_rc_env_conflicts(&rc, &env, &[]);
    assert!(warnings.is_empty());
}

#[test]
fn rc_conflict_alias_different_value_warns() {
    let dir = tempfile::tempdir().unwrap();
    let rc = dir.path().join(".bashrc");
    std::fs::write(
        &rc,
        "alias vim=\"vi\"\n[ -f ~/.cfgd.env ] && . ~/.cfgd.env\n",
    )
    .unwrap();
    let aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
        platforms: vec![],
    }];
    let warnings = super::detect_rc_env_conflicts(&rc, &[], &aliases);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("alias vim"));
    assert!(warnings[0].contains("move it after the source line"));
}

#[test]
fn rc_conflict_after_source_line_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let rc = dir.path().join(".bashrc");
    std::fs::write(
        &rc,
        "[ -f ~/.cfgd.env ] && . ~/.cfgd.env\nexport EDITOR=\"vim\"\n",
    )
    .unwrap();
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let warnings = super::detect_rc_env_conflicts(&rc, &env, &[]);
    assert!(warnings.is_empty());
}

#[test]
fn rc_conflict_no_source_line_all_before() {
    let dir = tempfile::tempdir().unwrap();
    let rc = dir.path().join(".bashrc");
    std::fs::write(&rc, "export EDITOR=\"vim\"\nalias vim=\"vi\"\n").unwrap();
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
        platforms: vec![],
    }];
    let warnings = super::detect_rc_env_conflicts(&rc, &env, &aliases);
    assert_eq!(warnings.len(), 2);
}

#[test]
fn rc_conflict_nonexistent_file_no_warnings() {
    let warnings = super::detect_rc_env_conflicts(
        std::path::Path::new("/nonexistent/.bashrc"),
        &[crate::config::EnvVar {
            name: "FOO".into(),
            value: "bar".into(),
            platforms: vec![],
        }],
        &[],
    );
    assert!(warnings.is_empty());
}

#[test]
fn strip_shell_quotes_works() {
    assert_eq!(super::strip_shell_quotes("\"hello\""), "hello");
    assert_eq!(super::strip_shell_quotes("'hello'"), "hello");
    assert_eq!(super::strip_shell_quotes("hello"), "hello");
    assert_eq!(super::strip_shell_quotes("\"\""), "");
}

// --- PowerShell env generation tests ---

#[test]
fn generate_powershell_env_basic() {
    let env = vec![
        crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "code".into(),
            platforms: vec![],
        },
        crate::config::EnvVar {
            name: "PATH".into(),
            value: r"C:\Users\user\.cargo\bin;$env:PATH".into(),
            platforms: vec![],
        },
    ];
    let content = super::generate_powershell_env_content(
        &env,
        &[],
        windows_path_fold(&env).as_ref(),
        &Default::default(),
    );
    assert!(content.starts_with("# managed by cfgd"));
    assert!(content.contains("$env:EDITOR = 'code'"));
    // PATH references $env: so double-quoted to allow expansion
    assert!(content.contains(r#"$env:PATH = "C:\Users\user\.cargo\bin;$env:PATH""#));
}

#[test]
fn generate_powershell_env_with_aliases() {
    let aliases = vec![
        crate::config::ShellAlias {
            name: "g".into(),
            command: "git".into(),
            platforms: vec![],
        },
        crate::config::ShellAlias {
            name: "ll".into(),
            command: "Get-ChildItem -Force".into(),
            platforms: vec![],
        },
    ];
    let content = super::generate_powershell_env_content(&[], &aliases, None, &Default::default());
    assert!(content.contains("Set-Alias -Name g -Value 'git'"));
    assert!(content.contains("function ll {"));
    assert!(content.contains("Get-ChildItem -Force @args"));
}

#[test]
fn generate_powershell_env_escapes_quotes() {
    let env = vec![crate::config::EnvVar {
        name: "GREETING".into(),
        value: r#"say "hello""#.into(),
        platforms: vec![],
    }];
    let content = super::generate_powershell_env_content(&env, &[], None, &Default::default());
    // No $env: reference, so single-quoted (PS single quotes don't need escaping except ')
    assert!(content.contains("$env:GREETING = 'say \"hello\"'"));
}

#[test]
fn generate_powershell_env_empty() {
    let content = super::generate_powershell_env_content(&[], &[], None, &Default::default());
    assert!(content.starts_with("# managed by cfgd"));
    // Only header + trailing newline
    assert_eq!(content.lines().count(), 1);
}

// --- Apply execution path tests ---

/// A mock package manager that tracks which packages were installed/uninstalled.
struct TrackingPackageManager {
    name: String,
    installed: std::sync::Mutex<HashSet<String>>,
    install_calls: std::sync::Mutex<Vec<Vec<String>>>,
    uninstall_calls: std::sync::Mutex<Vec<Vec<String>>>,
    // When true, `package_identity` maps an entry to its last `/`-segment after
    // stripping `@<version>`, mimicking go (module path → binary name).
    identity_strip: bool,
}

impl TrackingPackageManager {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            installed: std::sync::Mutex::new(HashSet::new()),
            install_calls: std::sync::Mutex::new(Vec::new()),
            uninstall_calls: std::sync::Mutex::new(Vec::new()),
            identity_strip: false,
        }
    }

    fn with_installed(name: &str, pkgs: &[&str]) -> Self {
        let mut set = HashSet::new();
        for p in pkgs {
            set.insert(p.to_string());
        }
        Self {
            name: name.to_string(),
            installed: std::sync::Mutex::new(set),
            install_calls: std::sync::Mutex::new(Vec::new()),
            uninstall_calls: std::sync::Mutex::new(Vec::new()),
            identity_strip: false,
        }
    }

    fn go_like(name: &str) -> Self {
        let mut m = Self::new(name);
        m.identity_strip = true;
        m
    }
}

impl PackageManager for TrackingPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        None
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(self.installed.lock().unwrap().clone())
    }
    fn install(&self, packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        self.install_calls.lock().unwrap().push(packages.to_vec());
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.insert(p.clone());
        }
        Ok(())
    }
    fn uninstall(&self, packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        self.uninstall_calls.lock().unwrap().push(packages.to_vec());
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.remove(p);
        }
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn package_identity(&self, entry: &str) -> String {
        if self.identity_strip {
            entry
                .split('@')
                .next()
                .unwrap_or(entry)
                .rsplit('/')
                .next()
                .unwrap_or(entry)
                .to_string()
        } else {
            entry.to_string()
        }
    }
}

#[test]
fn stale_tracked_packages_core_identifies_gone_rows() {
    use super::stale_tracked_packages;
    // bat still installed → kept; ghost gone → stale.
    let mgr = TrackingPackageManager::with_installed("cargo", &["bat"]);
    let managers: Vec<&dyn PackageManager> = vec![&mgr];
    let cfgd_installed: HashSet<String> = ["cargo/bat".to_string(), "cargo/ghost".to_string()]
        .into_iter()
        .collect();
    let state = test_state();
    let printer = test_printer();
    let cx = test_package_context(&printer, &state);
    let stale = stale_tracked_packages(&managers, &cfgd_installed, &cx).unwrap();
    assert_eq!(stale, vec![("cargo".to_string(), "ghost".to_string())]);
}

#[test]
fn apply_package_install_tracks_under_identity_for_go_like_manager() {
    // go installs `rsc.io/2fa` but lists the binary `2fa`; the tracking key must
    // be the identity `go/2fa` so prune later matches installed state.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::go_like("go")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Install {
        manager: "go".to_string(),
        packages: vec!["rsc.io/2fa".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert!(
        state.is_resource_managed("package", "go/2fa").unwrap(),
        "install must track under the binary identity"
    );
    assert!(
        !state
            .is_resource_managed("package", "go/rsc.io/2fa")
            .unwrap(),
        "the module-path key must never be written"
    );
}

#[test]
fn apply_package_install_calls_mock_and_records_state() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string(), "fd".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // The install, and ahead of it the `Prerequisites` node refreshing the
    // index it reads.
    assert_eq!(result.action_results.len(), 2);
    assert!(result.action_results.iter().all(|r| r.success));
    assert!(result.action_results.iter().all(|r| r.error.is_none()));
    assert_eq!(
        result.action_results[0].description, "manager:refresh:brew",
        "the manager's index is refreshed before the packages that read it"
    );
    assert!(result.action_results[1].description.contains("ripgrep"));

    // Verify install was actually called on the tracking mock
    let pm = registry.package_managers()[0].as_ref();
    let cx = test_package_context(&printer, &state);
    let installed = pm.installed_packages(&cx).unwrap();
    assert!(installed.contains("ripgrep"));
    assert!(installed.contains("fd"));
}

/// A mock scripted manager that persists an uninstall command, mirroring how a
/// user-defined custom manager behaves.
struct ScriptedLikeManager {
    name: String,
    uninstall_cmd: String,
}

impl PackageManager for ScriptedLikeManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        None
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn persisted_uninstall(&self) -> Option<String> {
        Some(self.uninstall_cmd.clone())
    }
}

#[test]
fn apply_scripted_install_persists_uninstall_cmd_and_builtin_leaves_null() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(ScriptedLikeManager {
        name: "widgetmgr".to_string(),
        uninstall_cmd: "widgetmgr rm {package}".to_string(),
    }));
    registry.add_package_manager(Box::new(TrackingPackageManager::new("cargo")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![
        PackageAction::Install {
            manager: "widgetmgr".to_string(),
            packages: vec!["widget".to_string()],
            origin: "local".to_string(),
        },
        PackageAction::Install {
            manager: "cargo".to_string(),
            packages: vec!["bat".to_string()],
            origin: "local".to_string(),
        },
    ];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // The scripted install must have persisted its uninstall command; the
    // built-in install must leave it NULL. Probe via the orphan query with an
    // empty known set so every package row surfaces.
    let known = HashSet::new();
    let mut orphans = state.orphaned_package_resources(&known).unwrap();
    orphans.sort_by(|a, b| a.manager.cmp(&b.manager));
    assert_eq!(orphans.len(), 2);
    assert_eq!(orphans[0].manager, "cargo");
    assert!(
        orphans[0].uninstall_cmd.is_none(),
        "built-in install must not persist a script"
    );
    assert_eq!(orphans[1].manager, "widgetmgr");
    assert_eq!(
        orphans[1].uninstall_cmd.as_deref(),
        Some("widgetmgr rm {package}"),
        "scripted install must persist its uninstall command"
    );
}

#[test]
fn apply_package_uninstall_calls_mock() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::with_installed(
        "brew",
        &["ripgrep", "fd"],
    )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Uninstall {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);

    let pm = registry.package_managers()[0].as_ref();
    let cx = test_package_context(&printer, &state);
    let installed = pm.installed_packages(&cx).unwrap();
    assert!(!installed.contains("ripgrep"));
    assert!(installed.contains("fd"));
}

#[test]
fn apply_package_install_tracks_per_package_managed_resource() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string(), "fd".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Each installed package tracks under its own "<mgr>/<pkg>" key.
    assert!(
        state
            .is_resource_managed("package", "brew/ripgrep")
            .unwrap()
    );
    assert!(state.is_resource_managed("package", "brew/fd").unwrap());
    // The lossy "install:<pkg>" key must never be written.
    assert!(
        !state
            .is_resource_managed("package", "install:ripgrep")
            .unwrap()
    );
}

#[test]
fn apply_package_uninstall_untracks_managed_resource() {
    let state = test_state();
    // Pre-track a package as cfgd-installed.
    state
        .upsert_managed_resource("package", "brew/ripgrep", "local", None, None)
        .unwrap();

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::with_installed(
        "brew",
        &["ripgrep"],
    )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Uninstall {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // The tracking row is deleted, not re-added under a bogus key.
    assert!(
        !state
            .is_resource_managed("package", "brew/ripgrep")
            .unwrap()
    );
    assert!(
        !state
            .is_resource_managed("package", "uninstall:ripgrep")
            .unwrap()
    );
}

#[test]
fn apply_empty_plan_records_success_in_state_store() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 0);

    // Verify the state store has a record
    let last = state.last_apply().unwrap();
    assert!(last.is_some());
    let record = last.unwrap();
    assert_eq!(record.status, ApplyStatus::Success);
    assert_eq!(record.profile, "test");
    assert_eq!(record.id, result.apply_id);
}

#[test]
fn apply_records_correct_apply_id() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();
    let printer = test_printer();

    // First apply
    let result1 = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Second apply
    let result2 = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Each apply should get a unique, incrementing ID
    assert!(result2.apply_id > result1.apply_id);

    // Verify via state store
    let last = state.last_apply().unwrap().unwrap();
    assert_eq!(last.id, result2.apply_id);
}

#[test]
fn apply_env_write_env_file_to_tempdir() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join(".cfgd.env");

    let env = vec![
        crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
            platforms: vec![],
        },
        crate::config::EnvVar {
            name: "CARGO_HOME".into(),
            value: "/home/user/.cargo".into(),
            platforms: vec![],
        },
    ];
    let content = super::generate_env_file_content(&env, &[], None, &Default::default());

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content: content.clone(),
        vars: 0,
        aliases: 0,
    };

    let printer = test_printer();
    let desc =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap();

    // Verify file was written
    let written = std::fs::read_to_string(&env_path).unwrap();
    assert_eq!(written, content);
    assert!(written.contains("export EDITOR=\"nvim\""));
    assert!(written.contains("export CARGO_HOME=\"/home/user/.cargo\""));
    assert!(desc.starts_with("env:write:"));
}

#[test]
fn apply_env_write_skips_when_content_matches() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join(".cfgd.env");

    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let content = super::generate_env_file_content(&env, &[], None, &Default::default());

    // Pre-write identical content
    std::fs::write(&env_path, &content).unwrap();

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content,
        vars: 0,
        aliases: 0,
    };

    let printer = test_printer();
    let desc =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap();

    // Should report skipped
    assert!(desc.contains("skipped"), "Expected skip: {}", desc);
}

#[test]
fn apply_env_inject_source_line_creates_file() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };

    let printer = test_printer();
    let desc =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap();

    let written = std::fs::read_to_string(&rc_path).unwrap();
    assert!(written.contains(". ~/.cfgd.env"));
    assert!(desc.starts_with("env:inject:"));
}

#[test]
fn apply_env_inject_skips_when_already_present() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");

    // Pre-write content that already mentions cfgd.env
    std::fs::write(
        &rc_path,
        "# existing config\n[ -f ~/.cfgd.env ] && . ~/.cfgd.env\n",
    )
    .unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };

    let printer = test_printer();
    let desc =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap();

    assert!(desc.contains("skipped"), "Expected skip: {}", desc);
}

#[test]
fn apply_env_live_session_reports_the_planned_resource_id() {
    // Empty vars keeps this hermetic: `refresh_session_env` short-circuits
    // before any platform shell-out.
    let planned =
        crate::reconciler::format_action_description(&Action::Env(EnvAction::RefreshLiveSession {
            vars: Vec::new(),
        }));

    let printer = test_printer();
    let desc = Reconciler::apply_env_action(
        &EnvAction::RefreshLiveSession { vars: Vec::new() },
        &printer,
        crate::providers::NoteSink::discarded(),
    )
    .unwrap();

    assert_eq!(
        desc,
        format!("{planned}:skipped"),
        "the live-session result id must be the planned id plus the skip suffix, \
         or `merge_env_result` records the Env phase and the late regeneration \
         as two results for one action"
    );
}

#[test]
fn apply_env_inject_migrates_legacy_source_keyword() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".profile");

    // A dotfile written by an older cfgd used the bash-only `source` keyword,
    // which fails under a POSIX /bin/sh `.profile` (FreeBSD base, dash).
    std::fs::write(
        &rc_path,
        "# existing config\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
    )
    .unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };

    let printer = test_printer();
    Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
        .unwrap();

    let written = std::fs::read_to_string(&rc_path).unwrap();
    // The legacy line is upgraded in place, not duplicated.
    assert!(written.contains("[ -f ~/.cfgd.env ] && . ~/.cfgd.env"));
    assert!(!written.contains("source ~/.cfgd.env"));
    assert_eq!(
        written.matches(".cfgd.env").count(),
        2, // one `[ -f ~/.cfgd.env ]` test + one `. ~/.cfgd.env` loader, single line
        "exactly one managed source line must remain: {written:?}"
    );
    assert!(written.starts_with("# existing config\n"));
}

#[test]
fn apply_env_inject_appends_to_existing_content() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");

    std::fs::write(&rc_path, "# my config\nexport FOO=bar").unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };

    let printer = test_printer();
    Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
        .unwrap();

    let written = std::fs::read_to_string(&rc_path).unwrap();
    assert!(written.starts_with("# my config\n"));
    assert!(written.contains("export FOO=bar"));
    assert!(written.contains(". ~/.cfgd.env"));
}

#[test]
fn apply_full_flow_plan_apply_verify_consistent() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::with_installed(
        "brew",
        &["git"],
    )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // Plan: install ripgrep and fd via brew
    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string(), "fd".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();
    assert!(!plan.is_empty());

    // Apply
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // The install, and the `Prerequisites` node refreshing its manager's index.
    assert_eq!(result.succeeded(), 2);
    assert_eq!(result.failed(), 0);

    // State store should show the apply
    let last = state.last_apply().unwrap().unwrap();
    assert_eq!(last.id, result.apply_id);
    assert_eq!(last.status, ApplyStatus::Success);
    assert!(last.summary.is_some());

    // Managed resources should be recorded
    let resources = state.managed_resources().unwrap();
    assert!(
        !resources.is_empty(),
        "Expected managed resources after apply"
    );
}

#[test]
fn apply_records_summary_json() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["jq".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Verify the summary JSON in the state store
    let last = state.last_apply().unwrap().unwrap();
    let summary = last.summary.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&summary).unwrap();
    // The install, and the `Prerequisites` node refreshing its manager's index.
    assert_eq!(parsed["total"], 2);
    assert_eq!(parsed["succeeded"], 2);
    assert_eq!(parsed["failed"], 0);
    assert_eq!(result.apply_id, last.id);
}

#[test]
fn apply_with_phase_filter_only_runs_matching_phase() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // Create a plan with package actions
    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();

    // Apply with filter set to Env phase — should skip Packages
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Prerequisites)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // Only the manager node the `Prerequisites` phase owns: the install the
    // filter excluded did not run.
    let descriptions: Vec<&str> = result
        .action_results
        .iter()
        .map(|r| r.description.as_str())
        .collect();
    assert_eq!(descriptions, vec!["manager:refresh:brew"]);
}

#[test]
fn apply_with_phase_filter_runs_only_packages() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    // The plan model always includes brew's index refresh in Prerequisites —
    // proving the assertion below is the filter excluding an existing node,
    // not the node having never been planned in the first place.
    let prereq_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Prerequisites);
    assert!(
        prereq_phase.is_some(),
        "the plan model always includes the manager refresh: {:?}",
        plan.phases
    );
    assert!(
        prereq_phase
            .unwrap()
            .actions()
            .any(|a| format_action_description(a).starts_with("manager:")),
        "the unfiltered plan must carry a manager: node for the filter to exclude: {:?}",
        plan.phases
    );

    let printer = test_printer();

    // Apply with filter set to Packages phase — should run the install
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);
    // `--phase packages` filters, and never adds: no `manager:*` node — the
    // index refresh belongs to Prerequisites and must not run here.
    assert!(
        !result
            .action_results
            .iter()
            .any(|r| r.description.starts_with("manager:")),
        "`--phase packages` must plan zero manager actions: {:?}",
        result.action_results
    );
}

#[test]
fn apply_file_create_action_writes_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    let target = dir.path().join("subdir/target.txt");
    std::fs::write(&source, "hello world").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let file_actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            file_actions,
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);

    // Verify file was created
    assert!(target.exists());
    let content = std::fs::read_to_string(&target).unwrap();
    assert_eq!(content, "hello world");
}

#[test]
fn apply_multiple_package_actions_all_succeed() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));
    registry.add_package_manager(Box::new(TrackingPackageManager::new("cargo")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![
        PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["jq".to_string()],
            origin: "local".to_string(),
        },
        PackageAction::Install {
            manager: "cargo".to_string(),
            packages: vec!["bat".to_string()],
            origin: "local".to_string(),
        },
    ];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // Two installs, each preceded by its manager's index refresh.
    assert_eq!(result.action_results.len(), 4);
    assert_eq!(result.succeeded(), 4);
    assert_eq!(result.failed(), 0);

    // Verify both managers had their install called
    let brew = registry.package_managers()[0].as_ref();
    let cargo = registry.package_managers()[1].as_ref();
    let cx = test_package_context(&printer, &state);
    assert!(brew.installed_packages(&cx).unwrap().contains("jq"));
    assert!(cargo.installed_packages(&cx).unwrap().contains("bat"));
}

/// A package manager that counts its index refreshes — the evidence for a
/// refresh cfgd was NOT asked to perform.
struct UpdateCountingPackageManager {
    name: String,
    updates: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl UpdateCountingPackageManager {
    fn new(name: &str, updates: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self {
            name: name.to_string(),
            updates,
        }
    }
}

impl PackageManager for UpdateCountingPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        None
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        self.updates
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// A plan carrying package work and NO `Prerequisites` node — what a run whose
/// manager node was pruned (`--skip prerequisites.<name>`) hands to `apply`.
/// Built by hand because `Reconciler::plan` mints a node for every manager its
/// package work names.
fn plan_of_package_actions(actions: Vec<PackageAction>) -> Plan {
    let profile = Owner::profile("test");
    Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &profile,
            actions.into_iter().map(Action::Package).collect(),
        )],
        warnings: Vec::new(),
    }
}

#[test]
fn a_pruned_refresh_node_leaves_the_index_alone() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    let updates = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    registry.add_package_manager(Box::new(UpdateCountingPackageManager::new(
        "apt",
        updates.clone(),
    )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    // The shape `--skip prerequisites.apt` leaves behind: the install the user
    // kept, without the refresh they removed. The refresh belongs to the phase,
    // so nothing else may perform it on the phase's behalf.
    let plan = plan_of_package_actions(vec![PackageAction::Install {
        manager: "apt".to_string(),
        packages: vec!["ripgrep".to_string()],
        origin: "local".to_string(),
    }]);

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1, "only the install ran");

    assert_eq!(
        updates.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a refresh the user removed from the plan must not run anyway"
    );
}

#[test]
fn a_prerequisite_is_never_recorded_as_a_user_managed_resource() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("apt")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let profile = Owner::profile("test");
    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &profile,
                vec![
                    Action::Manager(ManagerAction::RefreshIndex {
                        manager: "apt".to_string(),
                    }),
                    Action::Manager(ManagerAction::Prerequisite {
                        tool: "curl".to_string(),
                        installer: "apt".to_string(),
                        required_by: vec!["brew".to_string()],
                        depends_on: vec![ManagerAction::refresh_node("apt")],
                    }),
                ],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &profile,
                vec![Action::Package(PackageAction::Install {
                    manager: "apt".to_string(),
                    packages: vec!["ripgrep".to_string()],
                    origin: "local".to_string(),
                })],
            ),
        ],
        warnings: Vec::new(),
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);

    let recorded: Vec<String> = state
        .managed_resources()
        .unwrap()
        .into_iter()
        .map(|r| format!("{}:{}", r.resource_type, r.resource_id))
        .collect();
    assert!(
        recorded.iter().all(|id| !id.starts_with("manager:")),
        "curl is a tool cfgd needed, not a resource the user declared: cfgd never \
         removes it and `cfgd status` never claims it: {recorded:?}"
    );
    assert!(
        recorded.iter().any(|id| id.contains("ripgrep")),
        "the user's own package is still recorded: {recorded:?}"
    );
    // The journal still carries the work, which is where a prerequisite belongs.
    let journalled: Vec<String> = state
        .journal_entries(result.apply_id)
        .unwrap()
        .into_iter()
        .map(|e| format!("{}:{}", e.action_type, e.resource_id))
        .collect();
    assert!(
        journalled.contains(&"manager:prereq:curl".to_string()),
        "the journal records what cfgd did: {journalled:?}"
    );
}

#[test]
fn a_refusal_states_its_reason_once() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let reason = "curl is missing and no system manager is available";
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Manager(ManagerAction::Refuse {
                manager: "nix".to_string(),
                reason: reason.to_string(),
            })],
        )],
        warnings: Vec::new(),
    };

    let (printer, buf) = Printer::for_test();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Failed);

    let rendered = crate::test_helpers::captured_text(&buf);
    assert_eq!(
        rendered.matches(reason).count(),
        1,
        "the subject already IS the reason; the error it settles through must not \
         reprint it: {rendered}"
    );
    assert!(
        rendered.contains("cannot provision nix — curl is missing"),
        "the line still names the manager and the cause: {rendered}"
    );
    // The journal keeps the self-contained reason, which is what a later reader has.
    let journalled: Vec<String> = state
        .journal_entries(result.apply_id)
        .unwrap()
        .into_iter()
        .filter_map(|e| e.error)
        .collect();
    assert!(
        journalled.iter().any(|e| e.contains(reason)),
        "the journal records why: {journalled:?}"
    );
}

#[test]
fn apply_package_skip_action_succeeds() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Skip {
        manager: "apt".to_string(),
        reason: "not available on macOS".to_string(),
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);
    assert!(result.action_results[0].description.contains("skip"));
}

#[test]
fn apply_env_write_with_aliases_produces_correct_file() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join(".cfgd.env");

    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let aliases = vec![crate::config::ShellAlias {
        name: "ll".into(),
        command: "ls -la".into(),
        platforms: vec![],
    }];
    let content = super::generate_env_file_content(&env, &aliases, None, &Default::default());

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content: content.clone(),
        vars: 0,
        aliases: 0,
    };

    let printer = test_printer();
    Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
        .unwrap();

    let written = std::fs::read_to_string(&env_path).unwrap();
    assert!(written.contains("export EDITOR=\"nvim\""));
    assert!(written.contains("alias ll=\"ls -la\""));
    assert!(written.starts_with("# managed by cfgd"));
}

#[test]
fn combine_script_output_both() {
    let result = super::combine_script_output("hello\nworld", "warn: something");
    assert_eq!(
        result,
        Some("hello\nworld\n--- stderr ---\nwarn: something".to_string())
    );
}

#[test]
fn combine_script_output_stdout_only() {
    let result = super::combine_script_output("output line", "");
    assert_eq!(result, Some("output line".to_string()));
}

#[test]
fn combine_script_output_stderr_only() {
    let result = super::combine_script_output("", "error msg");
    assert_eq!(result, Some("error msg".to_string()));
}

#[test]
fn combine_script_output_empty() {
    assert!(super::combine_script_output("", "").is_none());
    assert!(super::combine_script_output("  ", " \n ").is_none());
}

#[test]
fn continue_on_error_defaults_per_phase() {
    // Pre-hooks default to false (abort on failure)
    assert!(!super::default_continue_on_error(&ScriptPhase::PreApply));
    assert!(!super::default_continue_on_error(
        &ScriptPhase::PreReconcile
    ));
    // Post-hooks and event hooks default to true (continue on failure)
    assert!(super::default_continue_on_error(&ScriptPhase::PostApply));
    assert!(super::default_continue_on_error(
        &ScriptPhase::PostReconcile
    ));
    assert!(super::default_continue_on_error(&ScriptPhase::OnChange));
    assert!(super::default_continue_on_error(&ScriptPhase::OnDrift));
}

#[test]
fn effective_continue_on_error_uses_explicit_value() {
    let entry = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "echo test".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: Some(true),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    });
    // Should be true even for pre-apply (which defaults to false)
    assert!(super::effective_continue_on_error(
        &entry,
        &ScriptPhase::PreApply
    ));

    let entry_false = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "echo test".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: Some(false),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    });
    // Should be false even for post-apply (which defaults to true)
    assert!(!super::effective_continue_on_error(
        &entry_false,
        &ScriptPhase::PostApply
    ));
}

#[test]
fn effective_continue_on_error_falls_back_to_default() {
    let simple = ScriptEntry::Simple("echo test".to_string());
    assert!(!super::effective_continue_on_error(
        &simple,
        &ScriptPhase::PreApply
    ));
    assert!(super::effective_continue_on_error(
        &simple,
        &ScriptPhase::PostApply
    ));

    let full_no_override = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "echo test".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    });
    assert!(!super::effective_continue_on_error(
        &full_no_override,
        &ScriptPhase::PreApply
    ));
    assert!(super::effective_continue_on_error(
        &full_no_override,
        &ScriptPhase::PostApply
    ));
}

#[test]
fn plan_scripts_with_apply_context_uses_pre_post_apply() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Simple("scripts/pre.sh".to_string())];
    resolved.merged.scripts.post_apply = vec![ScriptEntry::Simple("scripts/post.sh".to_string())];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let pre_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::PreScripts)
        .unwrap();
    assert_eq!(pre_phase.action_count(), 1);
    match pre_phase.actions().next().expect("phase holds an action") {
        Action::Script(ScriptAction::Run { entry, phase, .. }) => {
            assert_eq!(entry.run_str(), "scripts/pre.sh");
            assert_eq!(*phase, ScriptPhase::PreApply);
        }
        _ => panic!("expected Script action"),
    }

    let post_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::PostScripts)
        .unwrap();
    assert_eq!(post_phase.action_count(), 1);
    match post_phase.actions().next().expect("phase holds an action") {
        Action::Script(ScriptAction::Run { entry, phase, .. }) => {
            assert_eq!(entry.run_str(), "scripts/post.sh");
            assert_eq!(*phase, ScriptPhase::PostApply);
        }
        _ => panic!("expected Script action"),
    }
}

#[test]
fn plan_scripts_carries_full_entry() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "scripts/check.sh".to_string(),
        timeout: Some("10s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(true),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    })];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let pre_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::PreScripts)
        .unwrap();
    assert_eq!(pre_phase.action_count(), 1);
    match pre_phase.actions().next().expect("phase holds an action") {
        Action::Script(ScriptAction::Run { entry, .. }) => match entry {
            ScriptEntry::Full(ScriptCommand {
                run,
                timeout,
                continue_on_error,
                ..
            }) => {
                assert_eq!(run, "scripts/check.sh");
                assert_eq!(timeout.as_deref(), Some("10s"));
                assert_eq!(*continue_on_error, Some(true));
            }
            _ => panic!("expected Full entry"),
        },
        _ => panic!("expected Script action"),
    }
}

#[test]
fn build_script_env_includes_expected_vars() {
    let env = super::build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/home/user/.config/cfgd"),
        profile_name: "default",
        context: ReconcileContext::Apply,
        phase: &ScriptPhase::PreApply,
        module_name: None,
        module_dir: None,
        path_dirs: &[],
    });
    let map: HashMap<String, String> = env.into_iter().collect();
    assert_eq!(
        map.get("CFGD_CONFIG_DIR").unwrap(),
        "/home/user/.config/cfgd"
    );
    assert_eq!(map.get("CFGD_PROFILE").unwrap(), "default");
    assert_eq!(map.get("CFGD_CONTEXT").unwrap(), "apply");
    assert_eq!(map.get("CFGD_PHASE").unwrap(), "preApply");
    assert!(!map.contains_key("CFGD_DRY_RUN"));
    assert!(!map.contains_key("CFGD_MODULE_NAME"));
    assert!(!map.contains_key("CFGD_MODULE_DIR"));
}

#[test]
fn build_script_env_includes_module_vars() {
    let env = super::build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/config"),
        profile_name: "work",
        context: ReconcileContext::Reconcile,
        phase: &ScriptPhase::PostApply,
        module_name: Some("nvim"),
        module_dir: Some(std::path::Path::new("/modules/nvim")),
        path_dirs: &[],
    });
    let map: HashMap<String, String> = env.into_iter().collect();
    assert_eq!(map.get("CFGD_MODULE_NAME").unwrap(), "nvim");
    assert_eq!(map.get("CFGD_MODULE_DIR").unwrap(), "/modules/nvim");
    assert_eq!(map.get("CFGD_CONTEXT").unwrap(), "reconcile");
}

#[test]
fn execute_script_inline_command() {
    let printer = test_printer();
    let entry = ScriptEntry::Simple("echo hello".to_string());
    let dir = tempfile::tempdir().unwrap();
    let (desc, changed, output) = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .unwrap();
    assert!(desc.contains("echo hello"));
    assert!(changed);
    assert_eq!(output, Some("hello".to_string()));
}

#[test]
fn execute_script_failure_returns_error() {
    let printer = test_printer();
    let entry = ScriptEntry::Simple("exit 1".to_string());
    let dir = tempfile::tempdir().unwrap();
    let result = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("exit 1"),
        "error should mention exit code: {err}"
    );
}

#[test]
fn execute_script_with_timeout_override() {
    let printer = test_printer();
    let entry = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "echo fast".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    });
    let dir = tempfile::tempdir().unwrap();
    let (_, _, output) = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(300),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .unwrap();
    assert_eq!(output, Some("fast".to_string()));
}

#[test]
#[cfg(unix)]
fn execute_script_injects_env_vars() {
    let printer = test_printer();
    let entry = ScriptEntry::Simple("echo $MY_VAR".to_string());
    let dir = tempfile::tempdir().unwrap();
    let env = vec![("MY_VAR".to_string(), "test_value".to_string())];
    let (_, _, output) = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &env,
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .unwrap();
    assert_eq!(output, Some("test_value".to_string()));
}

#[test]
#[cfg(unix)]
fn execute_script_runs_executable_file() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("test.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho from_file\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let printer = test_printer();
    let entry = ScriptEntry::Simple("test.sh".to_string());
    let (_, _, output) = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .unwrap();
    assert_eq!(output, Some("from_file".to_string()));
}

#[test]
fn execute_script_rejects_non_executable_file() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("noexec.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let printer = test_printer();
    let entry = ScriptEntry::Simple("noexec.sh".to_string());
    let result = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not executable"),
        "should say not executable: {err}"
    );
}

#[test]
#[cfg(unix)]
fn execute_script_idle_timeout_kills_idle_process() {
    let printer = test_printer();
    // Script prints once then sleeps forever — idle timeout should kill it
    let entry = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "echo started; sleep 60".to_string(),
        timeout: Some("30s".to_string()),
        idle_timeout: Some("1s".to_string()),
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    });
    let dir = tempfile::tempdir().unwrap();
    let result = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(30),
        &printer,
        None,
        None,
        ScriptReport::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("idle (no output)"),
        "should mention idle timeout: {err}"
    );
}

// --- Rollback tests ---

#[test]
fn rollback_restores_file_content() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.txt");
    let file_path = target.display().to_string();

    // Rollback restores to the state AFTER the target apply.
    // Setup: apply 1 writes "v1 content", apply 2 modifies to "v2 content"
    // (capturing "v1 content" as backup). Rollback to apply 1 → "v1 content".
    let state = test_state();

    // Apply 1: creates file with v1 content
    let apply_id_1 = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    let resource_id = format!("file:create:{}", target.display());
    let jid1 = state
        .journal_begin(apply_id_1, 0, "files", "file", &resource_id, None)
        .unwrap();
    state.journal_complete(jid1, 0, None, None).unwrap();
    std::fs::write(&target, "v1 content").unwrap();

    // Apply 2: modifies file to v2 content. Backup captures v1 content.
    let file_state = crate::capture_file_state(&target).unwrap().unwrap();
    let apply_id_2 = state
        .record_apply("test", "hash2", ApplyStatus::Success, None)
        .unwrap();
    let update_resource_id = format!("file:update:{}", target.display());
    state
        .store_file_backup(apply_id_2, &file_path, &file_state)
        .unwrap();
    let jid2 = state
        .journal_begin(apply_id_2, 0, "files", "file", &update_resource_id, None)
        .unwrap();
    state.journal_complete(jid2, 0, None, None).unwrap();
    std::fs::write(&target, "v2 content").unwrap();

    // Rollback to apply 1 — should restore v1 content
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let rollback_result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    assert_eq!(rollback_result.files_restored, 1);
    assert_eq!(rollback_result.files_removed, 0);
    assert!(rollback_result.non_file_actions.is_empty());

    let restored = std::fs::read_to_string(&target).unwrap();
    assert_eq!(restored, "v1 content");
}

#[test]
fn rollback_removes_files_created_by_later_apply() {
    // Contract: apply A creates F (v1). Apply B updates F->v2 AND creates G.
    // rollback(A) must restore F to v1 AND remove G (G did not exist when A
    // completed). This mirrors the exact backup mechanics apply.rs performs:
    // pre-action backups (existed=1 for an existing target, an absent marker
    // for a CREATE), plus post-apply resolved-content snapshots.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    let g = dir.path().join("g.txt");
    let f_path = f.display().to_string();
    let g_path = g.display().to_string();

    let state = test_state();

    // Apply A: creates F with v1. No prior state -> absent marker for F.
    let apply_a = state
        .record_apply("test", "hashA", ApplyStatus::Success, None)
        .unwrap();
    state.store_absent_backup(apply_a, &f_path).unwrap();
    let ja = state
        .journal_begin(
            apply_a,
            0,
            "files",
            "file",
            &format!("file:create:{}", f.display()),
            None,
        )
        .unwrap();
    state.journal_complete(ja, 0, None, None).unwrap();
    std::fs::write(&f, "v1").unwrap();
    // Post-apply snapshot for A captures F at v1.
    let f_snap_a = crate::capture_file_resolved_state(&f).unwrap().unwrap();
    state
        .store_file_backup(apply_a, &f_path, &f_snap_a)
        .unwrap();

    // Apply B: updates F->v2 (pre-action backup of existing F=v1) and creates G
    // (absent marker). Post-apply snapshots capture F=v2 and G content.
    let apply_b = state
        .record_apply("test", "hashB", ApplyStatus::Success, None)
        .unwrap();
    let f_pre_b = crate::capture_file_state(&f).unwrap().unwrap();
    state.store_file_backup(apply_b, &f_path, &f_pre_b).unwrap();
    let jb_f = state
        .journal_begin(
            apply_b,
            0,
            "files",
            "file",
            &format!("file:update:{}", f.display()),
            None,
        )
        .unwrap();
    state.journal_complete(jb_f, 0, None, None).unwrap();
    std::fs::write(&f, "v2").unwrap();

    state.store_absent_backup(apply_b, &g_path).unwrap();
    let jb_g = state
        .journal_begin(
            apply_b,
            1,
            "files",
            "file",
            &format!("file:create:{}", g.display()),
            None,
        )
        .unwrap();
    // The completion counter is monotonic within a run, so two rows of one
    // apply can never share an index.
    state.journal_complete(jb_g, 1, None, None).unwrap();
    std::fs::write(&g, "g-content").unwrap();

    // Post-apply snapshots for B.
    let f_snap_b = crate::capture_file_resolved_state(&f).unwrap().unwrap();
    state
        .store_file_backup(apply_b, &f_path, &f_snap_b)
        .unwrap();
    let g_snap_b = crate::capture_file_resolved_state(&g).unwrap().unwrap();
    state
        .store_file_backup(apply_b, &g_path, &g_snap_b)
        .unwrap();

    // Rollback to A.
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let result = reconciler.rollback_apply(apply_a, &printer).unwrap();

    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "v1",
        "F must be restored to v1"
    );
    assert!(
        !g.exists(),
        "G must be removed (did not exist when A completed)"
    );
    assert!(
        result.files_removed >= 1,
        "files_removed must count G's removal, got {}",
        result.files_removed
    );

    // A new rollback apply row must be recorded.
    let last = state.last_apply().unwrap().unwrap();
    assert_eq!(last.profile, "rollback");
}

#[test]
fn rollback_no_changes_when_at_latest_apply() {
    // Rollback to the most recent apply with no subsequent applies
    // should produce no changes (system is already at that state).
    let state = test_state();
    let apply_id = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();

    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let rollback_result = reconciler.rollback_apply(apply_id, &printer).unwrap();

    assert_eq!(rollback_result.files_restored, 0);
    assert_eq!(rollback_result.files_removed, 0);
    assert!(rollback_result.non_file_actions.is_empty());
}

#[test]
fn rollback_lists_non_file_actions() {
    let state = test_state();
    let apply_id_1 = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();

    // Apply 2 has a package action (non-file) after apply 1
    let apply_id_2 = state
        .record_apply("test", "hash2", ApplyStatus::Success, None)
        .unwrap();
    let journal_id = state
        .journal_begin(
            apply_id_2,
            0,
            "packages",
            "package",
            "package:brew:install:ripgrep",
            None,
        )
        .unwrap();
    state.journal_complete(journal_id, 0, None, None).unwrap();

    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let rollback_result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    assert_eq!(rollback_result.files_restored, 0);
    assert_eq!(rollback_result.files_removed, 0);
    assert_eq!(rollback_result.non_file_actions.len(), 1);
    assert!(rollback_result.non_file_actions[0].1.contains("ripgrep"));
}

#[test]
fn rollback_records_new_apply_entry() {
    let state = test_state();
    let apply_id = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();

    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    reconciler.rollback_apply(apply_id, &printer).unwrap();

    // The rollback should have created a new apply entry
    let last = state.last_apply().unwrap().unwrap();
    assert_eq!(last.profile, "rollback");
    assert!(last.id > apply_id);
}

#[test]
fn rollback_dedups_same_apply_backups_by_highest_id_every_run() {
    // A single apply can store more than one backup row for the same path —
    // an absent marker before the CREATE, then the post-apply resolved
    // snapshot once the write lands. The row with the HIGHEST id is the one
    // rollback must keep: it is the state the apply actually settled on, not
    // a step on the way there. `target_snapshot`'s dedup walks
    // `get_apply_backups` (already `ORDER BY id`) in reverse so the first
    // row seen per path is always that one; a `HashMap`-driven walk could
    // keep either row depending on process-random iteration order, silently
    // restoring the wrong content on some runs and not others.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("f.txt");
    let file_path = target.display().to_string();
    let state = test_state();

    let apply_id = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    // Row 1 (lowest id): pre-action absent marker.
    state.store_absent_backup(apply_id, &file_path).unwrap();
    // Row 2 (highest id): the post-apply resolved snapshot — the desired
    // dedup winner.
    std::fs::write(&target, "settled content").unwrap();
    let settled = crate::capture_file_resolved_state(&target)
        .unwrap()
        .unwrap();
    state
        .store_file_backup(apply_id, &file_path, &settled)
        .unwrap();

    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    for _ in 0..5 {
        std::fs::write(&target, "later content").unwrap();
        let result = reconciler.rollback_apply(apply_id, &printer).unwrap();
        assert_eq!(result.files_restored, 1, "must restore exactly once");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "settled content",
            "must restore the highest-id row (the settled post-apply state), \
             not the pre-action absent marker, every run"
        );
    }
}

// --- Partial apply tests ---

/// A package manager that always fails on install.
struct FailingPackageManager {
    name: String,
}

impl FailingPackageManager {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl PackageManager for FailingPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        None
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Err(crate::errors::PackageError::InstallFailed {
            manager: self.name.clone(),
            message: "simulated install failure".to_string(),
        }
        .into())
    }
    fn uninstall(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[test]
fn apply_partial_when_some_actions_fail() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    // One working manager, one failing
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));
    registry.add_package_manager(Box::new(FailingPackageManager::new("apt")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![
        PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["jq".to_string()],
            origin: "local".to_string(),
        },
        PackageAction::Install {
            manager: "apt".to_string(),
            packages: vec!["curl".to_string()],
            origin: "local".to_string(),
        },
    ];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Partial);
    // brew's install and both managers' index refreshes; only apt's install fails.
    assert_eq!(result.succeeded(), 3);
    assert_eq!(result.failed(), 1);

    // Verify state store records partial status
    let last = state.last_apply().unwrap().unwrap();
    assert_eq!(last.status, ApplyStatus::Partial);
}

/// A failed action that RAN is timed like a successful one.
///
/// The row slot measures what RAN, not what succeeded — the rule the success
/// arm states for itself, which the failure arm never reached. The failed row
/// was the one line in its phase with no elapsed suffix while being, in the
/// take that found this, the second most expensive action in the phase: the
/// run's `(N.Ns wall)` total exceeded the sum of its visible rows with nothing
/// on screen to account for the difference.
///
/// The two failure shapes that ran NOTHING keep their silence: a `Refuse` node
/// IS the refusal, and a dependent the coordinator swept was never dispatched.
#[test]
fn a_failed_action_that_ran_is_timed_like_a_successful_one() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(FailingPackageManager::new("apt")));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::cfgd("managers"),
                vec![Action::Manager(ManagerAction::Refuse {
                    manager: "brew".to_string(),
                    reason: "no installer on this host".to_string(),
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Package(PackageAction::Install {
                    manager: "apt".to_string(),
                    packages: vec!["curl".to_string()],
                    origin: "local".to_string(),
                })],
            ),
        ],
        warnings: vec![],
    };

    let (printer, buf) = Printer::for_test();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");
    let captured = crate::test_helpers::captured_text(&buf);

    let install = captured
        .lines()
        .find(|l| l.contains("apt install curl"))
        .unwrap_or_else(|| panic!("the failed install has a row:\n{captured}"));
    assert!(
        install.contains("s)"),
        "a failed action that ran carries its elapsed like any other row: {install:?}"
    );
    let refused = captured
        .lines()
        .find(|l| l.contains("cannot provision brew"))
        .unwrap_or_else(|| panic!("the refusal has a row:\n{captured}"));
    assert!(
        !refused.contains("s)"),
        "a refusal runs no command, so it measures nothing: {refused:?}"
    );

    // The other never-ran shape: a dependent the coordinator swept when the
    // node it waited on failed. It arrives with `Duration::ZERO`, which the
    // duration floor would print as `(<0.1s)` — a measurement of an action
    // that was never dispatched.
    let swept = Action::Package(PackageAction::Install {
        manager: "apt".to_string(),
        packages: vec!["curl".to_string()],
        origin: "local".to_string(),
    });
    assert!(
        !super::apply::failed_action_ran(
            &swept,
            &crate::errors::PackageError::DependencyFailed {
                dependency: "provision brew via curl".to_string(),
            }
            .into()
        ),
        "a swept dependent never ran, whatever elapsed the collector hands it"
    );
    assert!(
        super::apply::failed_action_ran(
            &swept,
            &crate::errors::PackageError::InstallFailed {
                manager: "apt".to_string(),
                message: "simulated install failure".to_string(),
            }
            .into()
        ),
        "the same action failing on its own command DID run"
    );
}

#[test]
fn apply_failed_when_all_actions_fail() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    registry.add_package_manager(Box::new(FailingPackageManager::new("apt")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let pkg_actions = vec![PackageAction::Install {
        manager: "apt".to_string(),
        packages: vec!["curl".to_string()],
        origin: "local".to_string(),
    }];

    // Hand-built so the failing install is the run's ONLY action, and "every
    // action failed" stays expressible.
    let plan = plan_of_package_actions(pkg_actions);

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.succeeded(), 0);
    assert_eq!(result.failed(), 1);
    assert!(result.action_results[0].error.is_some());

    let last = state.last_apply().unwrap().unwrap();
    assert_eq!(last.status, ApplyStatus::Failed);
}

/// A package manager whose `install` panics mid-call, standing in for a lane
/// worker that unwinds instead of returning — the shape a real bug (an
/// indexing slip, a `.unwrap()` a provider was never supposed to reach) takes
/// in production.
struct PanickingPackageManager {
    name: String,
}

impl PackageManager for PanickingPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        None
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        panic!("simulated lane worker panic");
    }
    fn uninstall(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[test]
fn apply_survives_a_panicking_lane_worker_instead_of_hanging() {
    // F12 regression: a panic anywhere in a lane worker used to leave
    // `Finished` unsent, `running` never decrementing, and the coordinator
    // blocked in `inbox.recv()` forever. `apply` must still return (failed,
    // not hanging), so the whole call runs on a detached thread and the test
    // bounds it with `recv_timeout`: a regression here fails the assertion
    // instead of hanging the suite.
    //
    // This panic is caught by `run_one_action`'s own (pre-existing) inner
    // `catch_unwind` before it ever reaches the outer worker-closure guard
    // added in the same fix (the one around `lane.finish()`/`notes.take()`/
    // `tx.send`). That outer arm has no black-box trigger: every
    // `Printer::for_test*` constructor pins `live_region: false`, so
    // `LaneHandle::finish()` reduces to `Mutex::into_inner`/`lock` calls that
    // already recover from poisoning and cannot panic under test. What this
    // test proves instead — and what a hang here would still catch — is the
    // end-to-end contract: a worker panic anywhere in the dispatch loop
    // fails the run rather than wedging the coordinator, exercising the
    // coordinator's `tx`-drop and `inbox.recv()` disconnect path along with
    // it.
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(PanickingPackageManager {
        name: "panicky".to_string(),
    }));

    let pkg_actions = vec![PackageAction::Install {
        manager: "panicky".to_string(),
        packages: vec!["whatever".to_string()],
        origin: "local".to_string(),
    }];

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let state = test_state();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();
        // Hand-built so the panicking install is the run's ONLY action, and
        // "every action failed" stays expressible.
        let plan = plan_of_package_actions(pkg_actions);
        let printer = test_printer();
        let result = reconciler.apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        );
        let last_status = state.last_apply().unwrap().map(|a| a.status);
        let _ = tx.send(result.map(|r| (r.status.clone(), r.succeeded(), r.failed(), last_status)));
    });

    let (status, succeeded, failed, last_status) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect(
            "apply did not complete within 10s — a lane worker panic outside \
             run_one_action's catch_unwind hung the coordinator in inbox.recv()",
        )
        .unwrap();

    assert_eq!(status, ApplyStatus::Failed);
    assert_eq!(succeeded, 0);
    assert_eq!(failed, 1);
    assert_eq!(last_status, Some(ApplyStatus::Failed));
}

// --- continueOnError script tests ---

#[test]
#[cfg(unix)]
fn apply_continue_on_error_post_script_continues() {
    // A post-apply script with continueOnError=true should not abort the apply
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();

    // Post-apply script that fails but has continueOnError=true
    resolved.merged.scripts.post_apply = vec![ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "exit 42".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(true),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    })];

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["jq".to_string()],
        origin: "local".to_string(),
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            pkg_actions,
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Package install and its manager's index refresh succeeded, post-script
    // failed but continued
    assert_eq!(result.status, ApplyStatus::Partial);
    assert_eq!(result.succeeded(), 2); // index refresh + package install
    assert_eq!(result.failed(), 1); // failed post-script

    // Verify the failed action is the script
    let failed = result.action_results.iter().find(|r| !r.success).unwrap();
    assert!(
        failed.description.contains("exit 42"),
        "failed action should be the script: {}",
        failed.description
    );
}

// A multi-line failing script's `format_action_description` output must stay
// raw in the persisted `ActionResult.description` (the SQLite managed-resource
// / drift-matching key) while the `continueOnError` warning status subject
// condenses it — the `Renderer::write_line` debug assert forbids embedded
// newlines in a rendered subject.
#[test]
#[cfg(unix)]
fn apply_continue_on_error_multiline_script_condenses_display_keeps_raw_description() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();

    // The second/third lines are a no-output comment and a bare `exit` — never
    // `echo`, whose printed argument would land in the script's own captured
    // stdout and then legitimately reappear in the (content-preserving)
    // collapsed error text, making a naive "not in output" assertion below a
    // false positive regardless of whether the display subject condenses.
    resolved.merged.scripts.post_apply = vec![ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "true\n# raw-body-second-line-marker\nexit 42".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(true),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    })];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    drop(printer);

    let failed = result.action_results.iter().find(|r| !r.success).unwrap();
    assert!(
        failed.description.contains("raw-body-second-line-marker"),
        "persisted ActionResult.description must stay the raw multi-line body: {:?}",
        failed.description
    );
    assert!(
        failed.description.contains('\n'),
        "persisted description must not be condensed: {:?}",
        failed.description
    );

    let output = crate::test_helpers::captured_text(&buf);
    assert!(
        !output.contains("raw-body-second-line-marker"),
        "display status subject must condense away subsequent lines, got: {output}"
    );
}

#[test]
#[cfg(unix)]
fn apply_continue_on_error_false_pre_script_aborts() {
    // A pre-apply script with continueOnError=false should abort the entire apply
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "exit 1".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(false),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    })];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler.apply(
        &plan,
        &resolved,
        Path::new("."),
        &printer,
        None,
        &[],
        ReconcileContext::Apply,
        false,
        None,
        &crate::AbortFlag::new(),
    );

    // Pre-script failure with continueOnError=false should return an error
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("pre-script failed"),
        "should mention pre-script failure: {err}"
    );
}

// The "pre-script failed, aborting apply: {desc}" error message must condense
// a multi-line script's `format_action_description` output, not interpolate
// it raw — a raw multi-line `desc` here would trip `Renderer::write_line`'s
// no-embedded-newline assert wherever this error string is later rendered as
// a status subject (e.g. `cli/apply.rs`).
#[test]
#[cfg(unix)]
fn apply_continue_on_error_false_pre_script_abort_message_condenses_multiline_desc() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "echo line-one\necho line-two\nexit 1".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(false),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    })];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler.apply(
        &plan,
        &resolved,
        Path::new("."),
        &printer,
        None,
        &[],
        ReconcileContext::Apply,
        false,
        None,
        &crate::AbortFlag::new(),
    );

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("pre-script failed"),
        "should mention pre-script failure: {err}"
    );
    assert!(
        !err.contains('\n'),
        "abort error message must not embed a raw newline: {err:?}"
    );
    assert!(
        !err.contains("line-two"),
        "abort error message must condense away subsequent lines: {err:?}"
    );
}

#[test]
#[cfg(unix)]
fn apply_continue_on_error_default_post_script_continues() {
    // Post-apply scripts default to continueOnError=true (no explicit flag)
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    // Simple entry — no explicit continueOnError, defaults to true for post phase
    resolved.merged.scripts.post_apply = vec![ScriptEntry::Simple("exit 1".to_string())];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Post-script fails but default continueOnError=true means we get a result
    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
}

// --- onChange script execution tests ---

#[test]
#[cfg(unix)]
fn apply_on_change_script_runs_when_changes_occur() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    let target = dir.path().join("target.txt");
    let marker = dir.path().join("on_change_marker");

    std::fs::write(&source, "hello").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();

    // Set up an onChange script that creates a marker file
    resolved.merged.scripts.on_change =
        vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

    let file_actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            file_actions,
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);

    // The file action should have triggered the onChange script
    assert!(
        marker.exists(),
        "onChange marker file should exist, proving the onChange script ran"
    );

    // The file should have been deployed
    assert!(target.exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
}

#[test]
#[cfg(unix)]
fn apply_on_change_script_does_not_run_when_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("on_change_marker_noop");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.on_change =
        vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

    // Empty plan — no file changes, no package changes
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // No changes occurred, so onChange should NOT have run
    assert!(
        !marker.exists(),
        "onChange marker should NOT exist when no changes occurred"
    );
}

// --- Idempotency-guard skip propagation through the apply layer ---
//
// These drive guarded scripts through the FULL `reconciler.apply(...)` (not
// `execute_script` directly) to prove the `changed=false` returned by a
// guard-skipped script survives the apply dispatch — and that a skipped MODULE
// script does NOT fire its module's onChange hooks.

#[test]
#[cfg(unix)]
fn apply_guard_skipped_profile_script_records_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = dir.path().join("body-ran");
    // `unless: true` always succeeds → the body is skipped.
    let entry = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: format!("touch {}", sentinel.display()),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: Some("true".to_string()),
        creates: None,
        interactive: false,
    });

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.post_apply = vec![entry];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let script_result = result
        .action_results
        .iter()
        .find(|r| r.description.contains("script:"))
        .expect("script action should be recorded");
    assert!(
        script_result.success,
        "a skip is a clean no-op, not a failure"
    );
    assert!(
        !script_result.changed,
        "guard-skipped profile script must record changed=false through apply"
    );
    assert!(!sentinel.exists(), "body must not run when unless holds");
}

#[test]
#[cfg(unix)]
fn apply_guard_skipped_module_script_does_not_fire_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = dir.path().join("module-body-ran");
    let on_change_marker = dir.path().join("module-on-change-fired");

    // `unless: true` → the module's RunScript body is skipped → changed=false.
    let guarded = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: format!("touch {}", sentinel.display()),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: Some("true".to_string()),
        creates: None,
        interactive: false,
    });

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "testmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        pre_apply_scripts: Vec::new(),
        post_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Simple(format!(
            "touch {}",
            on_change_marker.display()
        ))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::PostScripts,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "testmod".to_string(),
                kind: ModuleActionKind::RunScript {
                    script: guarded,
                    phase: ScriptPhase::PostApply,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::PostScripts)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let module_result = result
        .action_results
        .iter()
        .find(|r| r.description.contains("module:testmod:script"))
        .expect("module script action should be recorded");
    assert!(module_result.success);
    assert!(
        !module_result.changed,
        "guard-skipped module script must record changed=false"
    );
    assert!(
        !sentinel.exists(),
        "module body must not run when unless holds"
    );
    assert!(
        !on_change_marker.exists(),
        "module onChange must NOT fire when the module's script was skipped (nothing changed)"
    );
}

#[test]
#[cfg(unix)]
fn apply_guard_permitted_module_script_fires_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = dir.path().join("module-body-ran-pos");
    let on_change_marker = dir.path().join("module-on-change-fired-pos");

    // `unless: false` → condition does NOT hold → the body RUNS → changed=true.
    let guarded = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: format!("touch {}", sentinel.display()),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: Some("false".to_string()),
        creates: None,
        interactive: false,
    });

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "testmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        pre_apply_scripts: Vec::new(),
        post_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Simple(format!(
            "touch {}",
            on_change_marker.display()
        ))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::PostScripts,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "testmod".to_string(),
                kind: ModuleActionKind::RunScript {
                    script: guarded,
                    phase: ScriptPhase::PostApply,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::PostScripts)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let module_result = result
        .action_results
        .iter()
        .find(|r| r.description.contains("module:testmod:script"))
        .expect("module script action should be recorded");
    assert!(module_result.success);
    assert!(
        module_result.changed,
        "guard-permitted module script must record changed=true"
    );
    assert!(
        sentinel.exists(),
        "module body must run when unless does not hold"
    );
    assert!(
        on_change_marker.exists(),
        "module onChange MUST fire when the module's script ran (positive control)"
    );
}

#[test]
#[cfg(unix)]
fn apply_skipped_module_does_not_fire_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let on_change_marker = dir.path().join("skipped-module-on-change-fired");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // The module carries onChange scripts but its only action this run is a
    // planned Skip (the upcoming module-platforms scenario: a whole module is
    // skipped). The skip did nothing, so onChange must not fire.
    let modules = vec![ResolvedModule {
        name: "skippedmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        pre_apply_scripts: Vec::new(),
        post_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Simple(format!(
            "touch {}",
            on_change_marker.display()
        ))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Modules,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "skippedmod".to_string(),
                kind: ModuleActionKind::Skip {
                    reason: "platform not matched".to_string(),
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Modules)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let module_result = result
        .action_results
        .iter()
        .find(|r| r.description.contains("module:skippedmod:skip"))
        .expect("module skip action should be recorded");
    assert!(module_result.success);
    assert!(
        !module_result.changed,
        "a planned module skip must record changed=false"
    );
    assert!(
        !on_change_marker.exists(),
        "module onChange must NOT fire when the module was skipped (nothing changed)"
    );
}

// --- Pure function / decision logic tests to cover uncovered lines ---

#[test]
fn parse_resource_from_description_cases() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "file:create:/home/user/.config",
            "file",
            "/home/user/.config",
        ),
        ("system:skip", "system", "skip"),
        ("unknown-action", "unknown", "unknown-action"),
        (
            "secret:resolve:vault:path/to/secret",
            "secret",
            "vault:path/to/secret",
        ),
        // Two structural colons (`type:subtype:body`): the subtype is
        // dropped by design, and a colon embedded in the body must survive
        // intact rather than being swallowed by the subtype split.
        ("script:post:echo \"a: b\"", "script", "echo \"a: b\""),
        // One structural colon (`execute_script`'s `"Running script: {body}"`):
        // a colon embedded in the body used to be misread as a second
        // structural separator, dropping everything between it and the
        // (wrongly assumed) third field.
        (
            "Running script: echo \"a: b\"",
            "Running script",
            " echo \"a: b\"",
        ),
        // One structural colon on an unrecognized prefix (`system:configurator:skip`):
        // previously landed in the two-colon branch by accident (2 colons in
        // the string) and dropped the configurator name; now the whole
        // remainder after the first colon is preserved.
        ("system:brew:skip", "system", "brew:skip"),
        // Env rows: the write/inject verb is dropped, leaving the target path
        // as the id; the live-session refresh leaves the literal "refresh".
        // `JournalEntry::is_file_work` keys its env disjunct on exactly these
        // shapes (path = file-backed, "refresh" = nothing to restore), so a
        // change here must move that predicate with it.
        ("env:write:/home/u/.cfgd.env", "env", "/home/u/.cfgd.env"),
        ("env:inject:~/.bashrc", "env", "~/.bashrc"),
        (super::format::LIVE_SESSION_RESOURCE_ID, "env", "refresh"),
    ];
    for (input, expected_type, expected_id) in cases {
        let (rtype, rid) = super::parse_resource_from_description(input);
        assert_eq!(rtype, *expected_type, "wrong type for {input:?}");
        assert_eq!(rid, *expected_id, "wrong id for {input:?}");
    }
}

#[test]
fn a_manager_nodes_description_parses_back_to_the_id_it_is_recorded_under() {
    use super::types::{ManagerAction, action_resource_info};

    let actions = [
        Action::Manager(ManagerAction::RefreshIndex {
            manager: "brew".to_string(),
        }),
        Action::Manager(ManagerAction::Provision {
            manager: "npm".to_string(),
            via: "brew".to_string(),
            declared: None,
            batched: vec![],
            depends_on: vec![ManagerAction::refresh_node("brew")],
        }),
        Action::Manager(ManagerAction::Prerequisite {
            tool: "curl".to_string(),
            installer: "apt".to_string(),
            required_by: vec!["nix".to_string()],
            depends_on: vec![ManagerAction::refresh_node("apt")],
        }),
    ];
    for action in &actions {
        let desc = super::format_action_description(action);
        // The journal writes one of these and the restore path reads the
        // other; a manager node whose description parsed back to a different
        // id would be recorded under a row nothing can find again. The ID
        // half agrees for every variant. The TYPE half deliberately splits
        // for a provision (and a refusal): its journal row stays "manager" —
        // cfgd's own scaffolding, which `record_managed_resources` refuses —
        // while its DRIFT row is typed "package", the same identity the CLI's
        // live check mints, so either producer's next check heals the
        // other's finding.
        let (drift_type, drift_id) = action_resource_info(action);
        let (journal_type, journal_id) = super::parse_resource_from_description(&desc);
        assert_eq!(
            drift_id, journal_id,
            "the recorded id and the id parsed back out of {desc:?} must agree"
        );
        let expects_package = matches!(
            action,
            Action::Manager(ManagerAction::Provision { .. } | ManagerAction::Refuse { .. })
        );
        assert_eq!(
            journal_type, "manager",
            "the journal side stays scaffolding"
        );
        assert_eq!(
            drift_type,
            if expects_package {
                "package"
            } else {
                "manager"
            },
            "wrong drift type for {desc:?}"
        );
    }
}

/// One stored identity per provision finding, whichever producer minted it:
/// the daemon tick records a planned provision (or its refusal) through
/// `action_resource_info`, the CLI's live check through
/// `ManagerAction::provision_resource_id` under the literal `package` type
/// (`manager_action_drift`, `crates/cfgd/src/cli/live_drift.rs`) — and both
/// must land on the same `(resource_type, resource_id)` row, or the two
/// producers stack a second permanent row on one fact and neither check can
/// heal the other's.
#[test]
fn both_producers_mint_one_identity_for_a_provision_finding() {
    use super::types::{ManagerAction, action_resource_info};

    let provision = Action::Manager(ManagerAction::Provision {
        manager: "npm".to_string(),
        via: "brew".to_string(),
        declared: None,
        batched: vec![],
        depends_on: vec![],
    });
    assert_eq!(
        action_resource_info(&provision),
        (
            "package".to_string(),
            ManagerAction::provision_resource_id("npm")
        ),
        "the tick's provision row must be the CLI live check's row"
    );

    let refuse = Action::Manager(ManagerAction::Refuse {
        manager: "npm".to_string(),
        reason: "provision failed".to_string(),
    });
    let (rtype, rid) = action_resource_info(&refuse);
    assert_eq!((rtype.as_str(), rid.as_str()), ("package", "refuse:npm"));
}

/// The id-shape invariant both keep predicates rest on, judged over the
/// daemon's action grammar INNER variant by inner variant: a daemon `module`
/// row is the bare module name (never a `/`, which the CLI's
/// `module_file_resource_id` always carries), a daemon `package` Skip row the
/// bare manager name (never a `:`, which every CLI-minted package id
/// carries), a daemon `system` row the `:`-spelled key the keep predicates
/// split the two system grammars on, and a provision or refusal the CLI's own
/// `package`-typed id so either producer's next check heals the other's row.
/// The nested exhaustive matches are the trip-wire: a new inner variant of
/// ANY action enum fails this test's compile until its recorded shape is
/// judged against the two live grammars here.
#[test]
fn no_daemon_action_row_wears_the_live_checks_separator() {
    use super::types::{ManagerAction, ModuleAction, ModuleActionKind, action_resource_info};
    use crate::providers::{FileAction, PackageAction, SecretAction};
    use crate::reconciler::{EnvAction, ScriptAction, SystemAction};

    // One judgment per INNER variant, no wildcard anywhere — matching only
    // the outer `Action` would let a new module kind or manager node change
    // its recorded shape without failing anything here.
    fn judged(a: &Action) {
        let (rtype, rid) = action_resource_info(a);
        match a {
            Action::File(fa) => match fa {
                FileAction::Create { .. }
                | FileAction::Update { .. }
                | FileAction::Delete { .. }
                | FileAction::SetPermissions { .. }
                | FileAction::Skip { .. } => assert_eq!(rtype, "file"),
            },
            Action::Package(pa) => match pa {
                // The daemon's bare-manager spelling: identity, never the
                // CLI's `<manager>:<name>`.
                PackageAction::Skip { .. } => {
                    assert_eq!(rtype, "package");
                    assert!(
                        !rid.contains(':'),
                        "a daemon Skip row is the bare manager, got {rid:?}"
                    );
                }
                // A batch always carries `:`, and carries `,` exactly when it
                // holds several packages — a single-package batch IS the
                // CLI's per-package row, so the same fact recorded by either
                // producer is one row.
                PackageAction::Install { packages, .. }
                | PackageAction::Uninstall { packages, .. } => {
                    assert_eq!(rtype, "package");
                    assert!(rid.contains(':'), "a batch row names its manager: {rid:?}");
                    assert_eq!(
                        packages.len() > 1,
                        rid.contains(','),
                        "the `,` marks exactly the multi-package batch: {rid:?}"
                    );
                }
            },
            Action::Secret(sa) => match sa {
                SecretAction::Decrypt { .. }
                | SecretAction::Resolve { .. }
                | SecretAction::ResolveEnv { .. }
                | SecretAction::Skip { .. } => assert_eq!(rtype, "secret"),
            },
            Action::System(sa) => match sa {
                // The `:` is the discriminator the keep predicates split the
                // daemon's spelling from the CLI's `<configurator>.<key>` on.
                SystemAction::SetValue { .. } => {
                    assert_eq!(rtype, "system");
                    assert!(
                        rid.contains(':'),
                        "a daemon SetValue row wears the `:` spelling, got {rid:?}"
                    );
                }
                SystemAction::Skip { .. } => {
                    assert_eq!(rtype, "system");
                    assert!(
                        !rid.contains(':') && !rid.contains('.'),
                        "a daemon system Skip row is the bare configurator, got {rid:?}"
                    );
                }
            },
            Action::Script(sa) => match sa {
                ScriptAction::Run { .. } => assert_eq!(rtype, "script"),
            },
            Action::Module(ma) => match &ma.kind {
                // Whatever work the module plans, its row is the bare module
                // name — never the `/` every CLI module-file id carries.
                ModuleActionKind::InstallPackages { .. }
                | ModuleActionKind::DeployFiles { .. }
                | ModuleActionKind::RunScript { .. }
                | ModuleActionKind::Skip { .. } => {
                    assert_eq!(rtype, "module");
                    assert!(
                        !rid.contains('/'),
                        "a daemon module row is the bare name, got {rid:?}"
                    );
                }
            },
            Action::Env(ea) => match ea {
                EnvAction::WriteEnvFile { .. } => assert_eq!(rtype, "env"),
                EnvAction::InjectSourceLine { .. } => assert_eq!(rtype, "env-rc"),
                EnvAction::RefreshLiveSession { .. } => assert_eq!(rtype, "env-session"),
            },
            Action::Manager(man) => match man {
                // A provision finding (and its refusal) is a PACKAGE fact
                // under the id the CLI live check mints.
                ManagerAction::Provision { manager, .. } => {
                    assert_eq!(rtype, "package");
                    assert_eq!(rid, ManagerAction::provision_resource_id(manager));
                }
                ManagerAction::Refuse { manager, .. } => {
                    assert_eq!(rtype, "package");
                    assert_eq!(rid, ManagerAction::refuse_resource_id(manager));
                }
                // cfgd's own scaffolding keeps the `manager` type
                // `record_managed_resources` refuses to manage.
                ManagerAction::RefreshIndex { .. } | ManagerAction::Prerequisite { .. } => {
                    assert_eq!(rtype, "manager");
                }
            },
        }
    }

    let module_kinds = [
        ModuleActionKind::InstallPackages { resolved: vec![] },
        ModuleActionKind::DeployFiles {
            files: vec![],
            declared_total: 0,
        },
        ModuleActionKind::RunScript {
            script: ScriptEntry::Simple("echo hi".to_string()),
            phase: super::ScriptPhase::PostApply,
        },
        ModuleActionKind::Skip {
            reason: "gated".to_string(),
        },
    ];
    let mut actions: Vec<Action> = module_kinds
        .into_iter()
        .map(|kind| Action::Module(ModuleAction::local("nvim", kind)))
        .collect();
    actions.extend([
        Action::File(FileAction::Delete {
            target: PathBuf::from("/home/u/.conf"),
            origin: "profile".to_string(),
        }),
        Action::Package(PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["jq".to_string()],
            origin: "profile".to_string(),
        }),
        Action::Package(PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["jq".to_string(), "rg".to_string()],
            origin: "profile".to_string(),
        }),
        Action::Package(PackageAction::Uninstall {
            manager: "brew".to_string(),
            packages: vec!["fd".to_string()],
            origin: "profile".to_string(),
        }),
        Action::Package(PackageAction::Skip {
            manager: "brew".to_string(),
            reason: "up to date".to_string(),
            origin: "profile".to_string(),
        }),
        Action::Secret(SecretAction::Skip {
            source: "s.enc".to_string(),
            reason: "no backend".to_string(),
            origin: "profile".to_string(),
        }),
        Action::System(SystemAction::SetValue {
            configurator: "gsettings".to_string(),
            key: "org.gnome.x key".to_string(),
            desired: "1".to_string(),
            current: "0".to_string(),
            origin: "profile".to_string(),
        }),
        Action::System(SystemAction::Skip {
            configurator: "systemdUnits".to_string(),
            reason: "'systemdUnits' is not available on this host".to_string(),
            origin: "profile".to_string(),
            unknown: false,
        }),
        Action::Script(ScriptAction::Run {
            entry: ScriptEntry::Simple("echo hi".to_string()),
            phase: super::ScriptPhase::PreApply,
            origin: "profile".to_string(),
        }),
        Action::Env(EnvAction::WriteEnvFile {
            path: PathBuf::from("/home/u/.cfgd.env"),
            content: String::new(),
            vars: 0,
            aliases: 0,
        }),
        Action::Env(EnvAction::InjectSourceLine {
            rc_path: PathBuf::from("/home/u/.zshrc"),
            line: "source ~/.cfgd.env".to_string(),
        }),
        Action::Env(EnvAction::RefreshLiveSession { vars: vec![] }),
        Action::Manager(ManagerAction::RefreshIndex {
            manager: "apt".to_string(),
        }),
        Action::Manager(ManagerAction::Provision {
            manager: "npm".to_string(),
            via: "brew".to_string(),
            declared: None,
            batched: vec![],
            depends_on: vec![],
        }),
        Action::Manager(ManagerAction::Prerequisite {
            tool: "curl".to_string(),
            installer: "apt".to_string(),
            required_by: vec!["brew".to_string()],
            depends_on: vec![],
        }),
        Action::Manager(ManagerAction::Refuse {
            manager: "npm".to_string(),
            reason: "provision failed".to_string(),
        }),
    ]);
    for action in &actions {
        judged(action);
    }
}

#[test]
fn parse_resource_from_description_keeps_module_name_in_the_id() {
    // `module:{name}:{verb}` puts the module NAME where other prefixes put a
    // verb. Dropping that segment gave every module the same id, and
    // `UNIQUE(resource_type, resource_id)` then collapsed the whole fleet of
    // modules onto a single managed_resources row.
    let (ty_a, id_a) = super::parse_resource_from_description("module:nvim:script");
    let (ty_b, id_b) = super::parse_resource_from_description("module:zsh:script");
    assert_eq!(ty_a, "module");
    assert_eq!(ty_b, "module");
    assert_eq!(id_a, "nvim:script");
    assert_eq!(id_b, "zsh:script");
    assert_ne!(
        id_a, id_b,
        "two modules running a script must not share one resource id"
    );

    // Same for the other module verbs.
    assert_eq!(
        super::parse_resource_from_description("module:nvim:skip").1,
        "nvim:skip"
    );
    assert_eq!(
        super::parse_resource_from_description("module:nvim:files:3").1,
        "nvim:files:3"
    );
    assert_eq!(
        super::parse_resource_from_description("module:nvim:packages:fd,rg").1,
        "nvim:packages:fd,rg"
    );
}

#[test]
fn parse_resource_from_description_keeps_manager_name_in_the_package_id() {
    // `package:{manager}:{verb}` has the same shape hazard as `module`. Only
    // skip reaches this parser — install/uninstall are split per-package by
    // `parse_package_description` first — and it collapsed onto the bare
    // verb, so every manager shared one row.
    let (ty_brew, id_brew) = super::parse_resource_from_description("package:brew:skip");
    let (ty_apt, id_apt) = super::parse_resource_from_description("package:apt:skip");
    assert_eq!(ty_brew, "package");
    assert_eq!(ty_apt, "package");
    assert_eq!(id_brew, "brew:skip");
    assert_eq!(id_apt, "apt:skip");
    assert_ne!(
        id_brew, id_apt,
        "two managers skipping must not share one resource id"
    );
    assert_eq!(
        super::parse_resource_from_description("package:brew:bootstrap").1,
        "brew:bootstrap"
    );
    assert_ne!(
        super::parse_resource_from_description("package:apt:bootstrap").1,
        "brew:bootstrap"
    );
}

#[test]
fn provenance_suffix_local_is_empty() {
    assert_eq!(super::provenance_suffix("local"), "");
    assert_eq!(super::provenance_suffix(""), "");
}

#[test]
fn provenance_suffix_non_local() {
    assert_eq!(super::provenance_suffix("acme"), " <- acme");
    assert_eq!(super::provenance_suffix("corp/source"), " <- corp/source");
}

#[test]
fn action_target_path_file_create() {
    let target = PathBuf::from("/home/user/.zshrc");
    let action = Action::File(FileAction::Create {
        source: PathBuf::from("/src"),
        target: target.clone(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    });
    assert_eq!(
        super::action_target_path(&action).map(|b| b.path),
        Some(target)
    );
}

#[test]
fn action_target_path_file_update() {
    let target = PathBuf::from("/home/user/.bashrc");
    let action = Action::File(FileAction::Update {
        source: PathBuf::from("/src"),
        target: target.clone(),
        diff: String::new(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    });
    assert_eq!(
        super::action_target_path(&action).map(|b| b.path),
        Some(target)
    );
}

#[test]
fn action_target_path_file_delete() {
    let target = PathBuf::from("/home/user/.old");
    let action = Action::File(FileAction::Delete {
        target: target.clone(),
        origin: "local".into(),
    });
    assert_eq!(
        super::action_target_path(&action).map(|b| b.path),
        Some(target)
    );
}

#[test]
fn action_target_path_env_write() {
    let path = PathBuf::from("/home/user/.cfgd.env");
    let action = Action::Env(EnvAction::WriteEnvFile {
        path: path.clone(),
        content: "test".into(),
        vars: 0,
        aliases: 0,
    });
    assert_eq!(
        super::action_target_path(&action).map(|b| b.path),
        Some(path)
    );
}

#[test]
fn action_target_path_package_returns_none() {
    let action = Action::Package(PackageAction::Install {
        manager: "brew".into(),
        packages: vec!["jq".into()],
        origin: "local".into(),
    });
    assert!(super::action_target_path(&action).is_none());
}

#[test]
fn action_target_path_module_returns_none() {
    let action = Action::Module(ModuleAction {
        module_name: "test".into(),
        kind: ModuleActionKind::Skip {
            reason: "n/a".into(),
        },
        origin: None,
    });
    assert!(super::action_target_path(&action).is_none());
}

#[test]
fn action_target_path_env_inject_returns_the_rc_path() {
    // The injection rewrites a user-owned dotfile in full, so it must produce a
    // backup row: without one, a failed or unwanted rewrite of ~/.bashrc has
    // nothing for `cfgd rollback` to restore.
    let rc_path = PathBuf::from("/home/user/.bashrc");
    let action = Action::Env(EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: ". ~/.cfgd.env".into(),
    });
    let backup = super::action_target_path(&action).expect("an injection must be backed up");
    assert_eq!(backup.path, rc_path);
    assert!(
        backup.follow_symlink,
        "the injection writes through a symlinked rc, so the backup must read through it too"
    );
}

#[test]
fn phase_name_from_str_unknown_returns_error() {
    let result = PhaseName::from_str("unknown-phase");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "unknown phase: unknown-phase");
}

#[test]
fn script_phase_display_name_all_variants() {
    assert_eq!(ScriptPhase::PreApply.display_name(), "preApply");
    assert_eq!(ScriptPhase::PostApply.display_name(), "postApply");
    assert_eq!(ScriptPhase::PreReconcile.display_name(), "preReconcile");
    assert_eq!(ScriptPhase::PostReconcile.display_name(), "postReconcile");
    assert_eq!(ScriptPhase::OnDrift.display_name(), "onDrift");
    assert_eq!(ScriptPhase::OnChange.display_name(), "onChange");
}

#[test]
fn format_action_description_secret_decrypt() {
    let action = Action::Secret(SecretAction::Decrypt {
        source: PathBuf::from("secrets/token.enc"),
        target: PathBuf::from("/home/user/.token"),
        backend: "sops".into(),
        origin: "local".into(),
    });
    let desc = format_action_description(&action);
    assert!(desc.starts_with("secret:decrypt:"));
    assert!(desc.contains("sops"));
    assert!(desc.contains(".token"));
}

#[test]
fn format_action_description_secret_resolve_env() {
    let action = Action::Secret(SecretAction::ResolveEnv {
        provider: "vault".into(),
        reference: "secret/data/gh#token".into(),
        envs: vec!["GH_TOKEN".into(), "GITHUB_TOKEN".into()],
        template: None,
        origin: "local".into(),
    });
    let desc = format_action_description(&action);
    assert!(desc.contains("secret:resolve-env:vault"));
    assert!(desc.contains("GH_TOKEN,GITHUB_TOKEN"));
}

#[test]
fn format_action_description_secret_skip() {
    let action = Action::Secret(SecretAction::Skip {
        source: "vault://test".into(),
        reason: "no backend".into(),
        origin: "local".into(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "secret:skip:vault://test");
}

#[test]
fn format_action_description_system_set_value() {
    let action = Action::System(SystemAction::SetValue {
        configurator: "sysctl".into(),
        key: "net.ipv4.ip_forward".into(),
        desired: "1".into(),
        current: "0".into(),
        origin: "local".into(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "system:sysctl.net.ipv4.ip_forward");
}

#[test]
fn format_action_description_system_skip() {
    let action = Action::System(SystemAction::Skip {
        configurator: "custom".into(),
        reason: "no configurator".into(),
        origin: "local".into(),
        unknown: true,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "system:custom:skip");
}

#[test]
fn format_action_description_env_write_and_inject() {
    let write = Action::Env(EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/user/.cfgd.env"),
        content: "content".into(),
        vars: 0,
        aliases: 0,
    });
    assert!(format_action_description(&write).starts_with("env:write:"));

    let inject = Action::Env(EnvAction::InjectSourceLine {
        rc_path: PathBuf::from("/home/user/.bashrc"),
        line: ". ~/.cfgd.env".into(),
    });
    assert!(format_action_description(&inject).starts_with("env:inject:"));
}

#[test]
fn format_action_description_module_deploy_files() {
    let action = Action::Module(ModuleAction {
        module_name: "nvim".into(),
        kind: {
            let files = vec![
                crate::modules::ResolvedFile {
                    source: PathBuf::from("/src/a"),
                    target: PathBuf::from("/dst/a"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                },
                crate::modules::ResolvedFile {
                    source: PathBuf::from("/src/b"),
                    target: PathBuf::from("/dst/b"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                },
            ];
            let declared_total = files.len();
            ModuleActionKind::DeployFiles {
                files,
                declared_total,
            }
        },
        origin: None,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "module:nvim:files:2");
}

#[test]
fn format_action_description_module_skip() {
    let action = Action::Module(ModuleAction {
        module_name: "broken".into(),
        kind: ModuleActionKind::Skip {
            reason: "dependency unmet".into(),
        },
        origin: None,
    });
    assert_eq!(format_action_description(&action), "module:broken:skip");
}

#[test]
fn format_action_description_module_run_script() {
    let action = Action::Module(ModuleAction {
        module_name: "nvim".into(),
        kind: ModuleActionKind::RunScript {
            script: ScriptEntry::Simple("setup.sh".into()),
            phase: ScriptPhase::PostApply,
        },
        origin: None,
    });
    assert_eq!(format_action_description(&action), "module:nvim:script");
}

#[test]
fn plan_to_hash_string_empty_plan_is_empty() {
    let plan = Plan {
        phases: vec![],
        warnings: vec![],
    };
    assert_eq!(plan.to_hash_string(), "");
}

#[test]
fn plan_to_hash_string_multiple_phases() {
    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["jq".into()],
                    origin: "local".into(),
                })],
            ),
            Phase::from_actions(
                PhaseName::Files,
                &Owner::profile("test"),
                vec![Action::File(FileAction::Create {
                    source: PathBuf::from("/src"),
                    target: PathBuf::from("/dst"),
                    origin: "local".into(),
                    strategy: crate::config::FileStrategy::Copy,
                    source_hash: None,
                    patch: None,
                })],
            ),
        ],
        warnings: vec![],
    };
    let hash = plan.to_hash_string();
    assert!(hash.contains('|'));
    assert!(hash.contains("jq"));
}

#[test]
fn plan_total_actions_sums_across_phases() {
    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![
                    Action::Package(PackageAction::Install {
                        manager: "brew".into(),
                        packages: vec!["a".into()],
                        origin: "local".into(),
                    }),
                    Action::Package(PackageAction::Install {
                        manager: "brew".into(),
                        packages: vec!["b".into()],
                        origin: "local".into(),
                    }),
                ],
            ),
            Phase::from_actions(
                PhaseName::Files,
                &Owner::profile("test"),
                vec![Action::File(FileAction::Skip {
                    target: PathBuf::from("/x"),
                    reason: "n/a".into(),
                    origin: "local".into(),
                })],
            ),
        ],
        warnings: vec![],
    };
    assert_eq!(plan.total_actions(), 3);
    assert!(!plan.is_empty());
}

#[test]
fn plan_secrets_sops_file_target() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_backend = Some(Box::new(MockSecretBackend::new("sops")));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "secrets/token.enc".to_string(),
        target: Some(PathBuf::from("/home/user/.token")),
        template: None,
        backend: None,
        envs: None,
    });

    let actions = reconciler.plan_secrets(&profile);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Secret(SecretAction::Decrypt {
            backend, target, ..
        }) => {
            assert_eq!(backend, "sops");
            assert_eq!(*target, PathBuf::from("/home/user/.token"));
        }
        other => panic!("Expected Decrypt, got {:?}", other),
    }
}

#[test]
fn plan_secrets_no_backend_skips() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no backend, no providers
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "secrets/token.enc".to_string(),
        target: Some(PathBuf::from("/home/user/.token")),
        template: None,
        backend: None,
        envs: None,
    });

    let actions = reconciler.plan_secrets(&profile);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Secret(SecretAction::Skip { reason, .. }) => {
            assert!(
                reason.contains("no secret backend"),
                "expected no-backend skip, got: {reason}"
            );
        }
        other => panic!("Expected Skip, got {:?}", other),
    }
}

#[test]
fn plan_secrets_envs_only_without_provider_skips() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no providers, no backend
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "plain-source".to_string(),
        target: None,
        template: None,
        backend: None,
        envs: Some(vec!["MY_SECRET".to_string()]),
    });

    let actions = reconciler.plan_secrets(&profile);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Secret(SecretAction::Skip { reason, .. }) => {
            assert!(
                reason.contains("secret provider reference"),
                "expected env-needs-provider skip, got: {reason}"
            );
        }
        other => panic!("Expected Skip, got {:?}", other),
    }
}

#[test]
fn plan_secrets_provider_not_available_skips() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no providers registered
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "vault://secret/data/test#key".to_string(),
        target: Some(PathBuf::from("/tmp/test")),
        template: None,
        backend: None,
        envs: None,
    });

    let actions = reconciler.plan_secrets(&profile);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Secret(SecretAction::Skip { reason, .. }) => {
            assert!(
                reason.contains("not available"),
                "expected provider-unavailable skip, got: {reason}"
            );
        }
        other => panic!("Expected Skip, got {:?}", other),
    }
}

#[test]
fn plan_secrets_sops_with_envs_generates_skip_for_envs() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_backend = Some(Box::new(MockSecretBackend::new("sops")));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "secrets/token.enc".to_string(),
        target: Some(PathBuf::from("/home/user/.token")),
        template: None,
        backend: None,
        envs: Some(vec!["TOKEN".to_string()]),
    });

    let actions = reconciler.plan_secrets(&profile);
    // Should produce a Decrypt action for the file target AND a Skip for env injection
    assert_eq!(actions.len(), 2);
    assert!(matches!(
        &actions[0],
        Action::Secret(SecretAction::Decrypt { .. })
    ));
    match &actions[1] {
        Action::Secret(SecretAction::Skip { reason, .. }) => {
            assert!(
                reason.contains("SOPS file targets cannot inject env vars"),
                "got: {reason}"
            );
        }
        other => panic!("Expected Skip for SOPS env injection, got {:?}", other),
    }
}

#[test]
fn plan_secrets_provider_no_target_no_envs_skips() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("secret"),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "vault://secret/data/test#key".to_string(),
        target: None,
        template: None,
        backend: None,
        envs: None,
    });

    let actions = reconciler.plan_secrets(&profile);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Secret(SecretAction::Skip { reason, .. }) => {
            assert!(reason.contains("no target or envs"), "got: {reason}");
        }
        other => panic!("Expected Skip for no-target/no-envs, got {:?}", other),
    }
}

#[test]
fn plan_modules_reconcile_context_uses_pre_post_reconcile() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let modules = vec![ResolvedModule {
        name: "test".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        pre_apply_scripts: vec![ScriptEntry::Simple("pre-apply.sh".into())],
        post_apply_scripts: vec![ScriptEntry::Simple("post-apply.sh".into())],
        pre_reconcile_scripts: vec![ScriptEntry::Simple("pre-reconcile.sh".into())],
        post_reconcile_scripts: vec![ScriptEntry::Simple("post-reconcile.sh".into())],
        on_change_scripts: vec![],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    // Reconcile context should use pre/post reconcile scripts, not apply scripts
    let actions = reconciler
        .plan_modules(&modules, "test", ReconcileContext::Reconcile)
        .0;
    assert_eq!(actions.len(), 2); // pre-reconcile + post-reconcile

    // First action should be pre-reconcile
    match &actions[0].1 {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::RunScript { script, phase } => {
                assert_eq!(script.run_str(), "pre-reconcile.sh");
                assert_eq!(*phase, ScriptPhase::PreReconcile);
            }
            _ => panic!("expected RunScript"),
        },
        _ => panic!("expected Module action"),
    }

    // Second action should be post-reconcile
    match &actions[1].1 {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::RunScript { script, phase } => {
                assert_eq!(script.run_str(), "post-reconcile.sh");
                assert_eq!(*phase, ScriptPhase::PostReconcile);
            }
            _ => panic!("expected RunScript"),
        },
        _ => panic!("expected Module action"),
    }
}

#[test]
fn format_plan_items_all_action_types() {
    let phase = Phase::from_actions(
        PhaseName::System,
        &Owner::profile("test"),
        vec![
            Action::System(SystemAction::SetValue {
                configurator: "sysctl".into(),
                key: "net.ipv4.ip_forward".into(),
                desired: "1".into(),
                current: "0".into(),
                origin: "local".into(),
            }),
            Action::System(SystemAction::Skip {
                configurator: "custom".into(),
                reason: "no configurator".into(),
                origin: "local".into(),
                unknown: false,
            }),
        ],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 2);
    assert!(items[0].contains("set sysctl.net.ipv4.ip_forward"));
    assert!(items[0].contains("0 \u{2192} 1"));
    assert!(items[1].contains("skip custom: no configurator"));
}

#[test]
fn format_plan_items_secret_actions() {
    let phase = Phase::from_actions(
        PhaseName::Secrets,
        &Owner::profile("test"),
        vec![
            Action::Secret(SecretAction::Decrypt {
                source: PathBuf::from("secret.enc"),
                target: PathBuf::from("/out/secret"),
                backend: "sops".into(),
                origin: "corp".into(),
            }),
            Action::Secret(SecretAction::Resolve {
                provider: "vault".into(),
                reference: "secret/gh#token".into(),
                target: PathBuf::from("/tmp/token"),
                template: None,
                origin: "local".into(),
            }),
            Action::Secret(SecretAction::ResolveEnv {
                provider: "1password".into(),
                reference: "Vault/Secret".into(),
                envs: vec!["TOKEN".into()],
                template: None,
                origin: "local".into(),
            }),
            Action::Secret(SecretAction::Skip {
                source: "missing".into(),
                reason: "not available".into(),
                origin: "local".into(),
            }),
        ],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 4);
    assert!(items[0].contains("decrypt"));
    assert!(items[0].contains("<- corp"));
    assert!(items[1].contains("resolve vault://"));
    assert!(items[2].contains("resolve 1password://"));
    assert!(items[2].contains("env [TOKEN]"));
    assert!(items[3].contains("skip missing"));
}

#[test]
fn format_plan_items_env_actions() {
    let phase = Phase::from_actions(
        PhaseName::Prerequisites,
        &Owner::profile("test"),
        vec![
            Action::Env(EnvAction::WriteEnvFile {
                path: PathBuf::from("/home/user/.cfgd.env"),
                content: "content".into(),
                vars: 0,
                aliases: 0,
            }),
            Action::Env(EnvAction::InjectSourceLine {
                rc_path: PathBuf::from("/home/user/.bashrc"),
                line: ". ~/.cfgd.env".into(),
            }),
        ],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 2);
    assert!(items[0].contains("write"));
    assert!(items[0].contains(".cfgd.env"));
    assert!(items[1].contains("inject source line"));
    assert!(items[1].contains(".bashrc"));
}

#[test]
fn format_plan_items_script_action_with_provenance() {
    let phase = Phase::from_actions(
        PhaseName::PreScripts,
        &Owner::profile("test"),
        vec![Action::Script(ScriptAction::Run {
            entry: ScriptEntry::Simple("setup.sh".into()),
            phase: ScriptPhase::PreApply,
            origin: "corp-source".into(),
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("run preApply script: setup.sh"));
    assert!(items[0].contains("<- corp-source"));
}

// `format_plan_items`'s Script arm feeds BOTH the human `ApplyRun::preview`
// tree AND `build_plan_output`'s `PlanActionOutput.description` JSON payload —
// it must return the raw, uncondensed `run_str()` body; condensing is the
// exclusive job of the human render site.
#[test]
fn format_plan_items_script_action_preserves_raw_multiline_body() {
    let raw_body = "echo line-one\necho line-two\necho line-three";
    let phase = Phase::from_actions(
        PhaseName::PreScripts,
        &Owner::profile("test"),
        vec![Action::Script(ScriptAction::Run {
            entry: ScriptEntry::Simple(raw_body.into()),
            phase: ScriptPhase::PreApply,
            origin: "test".into(),
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(
        items[0].contains(raw_body),
        "format_plan_items must preserve the raw multi-line body byte-identical, got: {:?}",
        items[0]
    );
}

#[test]
fn format_module_action_item_deploy_names_every_file() {
    let files: Vec<crate::modules::ResolvedFile> = (0..5)
        .map(|i| crate::modules::ResolvedFile {
            source: PathBuf::from(format!("/src/{i}")),
            target: PathBuf::from(format!("/dst/{i}")),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        })
        .collect();
    let action = ModuleAction {
        module_name: "big".into(),
        kind: ModuleActionKind::DeployFiles {
            declared_total: files.len(),
            files,
        },
        origin: None,
    };
    let item = super::format_module_action_item(&action);
    assert!(item.starts_with("deploy "));
    assert!(
        !item.contains("files"),
        "the count is the row's detail, never the subject's trailer: {item}"
    );
    assert_eq!(
        super::action_produced_detail(&Action::Module(action), None, 0, &[]),
        None,
        "the subject names every target, so a detail would state the total twice"
    );
}

#[test]
fn detect_file_conflicts_skip_and_delete_actions_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "content A").unwrap();
    std::fs::write(&file_b, "content B").unwrap();

    let shared_target = PathBuf::from("/target/a");

    let file_actions = vec![
        FileAction::Skip {
            target: shared_target.clone(),
            reason: "unchanged".into(),
            origin: "local".into(),
        },
        FileAction::Delete {
            target: PathBuf::from("/target/b"),
            origin: "local".into(),
        },
    ];

    // Module targets the same path as Skip — should NOT conflict because
    // Skip/Delete actions are excluded from conflict detection
    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![crate::modules::ResolvedFile {
            source: file_a.clone(),
            target: shared_target.clone(),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
    assert!(
        result.is_ok(),
        "Skip/Delete actions should be excluded from conflict detection: {:?}",
        result.err()
    );

    // Prove this matters: if the Skip were a Create with different content, it WOULD conflict
    let create_actions = vec![FileAction::Create {
        source: file_b,
        target: shared_target,
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];
    assert!(
        Reconciler::detect_file_conflicts(&create_actions, &modules).is_err(),
        "Create with different content at same target should conflict (proves Skip exclusion is meaningful)"
    );
}

#[test]
fn content_hash_if_exists_returns_none_for_missing() {
    let hash = super::content_hash_if_exists(Path::new("/nonexistent/file"));
    assert!(hash.is_none());
}

#[test]
fn content_hash_if_exists_returns_hash_for_existing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();
    let hash = super::content_hash_if_exists(&file);
    assert!(hash.is_some());
    // Same content should give same hash
    let hash2 = super::content_hash_if_exists(&file);
    assert_eq!(hash, hash2);
}

#[test]
fn merge_module_env_aliases_merges_correctly() {
    let profile_env = vec![crate::config::EnvVar {
        name: "A".into(),
        value: "1".into(),
        platforms: vec![],
    }];
    let profile_aliases = vec![crate::config::ShellAlias {
        name: "g".into(),
        command: "git".into(),
        platforms: vec![],
    }];
    let modules = vec![ResolvedModule {
        name: "mod1".into(),
        packages: vec![],
        files: vec![],
        env: vec![
            crate::config::EnvVar {
                name: "A".into(),
                value: "2".into(),
                platforms: vec![],
            },
            crate::config::EnvVar {
                name: "B".into(),
                value: "3".into(),
                platforms: vec![],
            },
        ],
        aliases: vec![crate::config::ShellAlias {
            name: "g".into(),
            command: "git status".into(),
            platforms: vec![],
        }],
        post_apply_scripts: vec![],
        pre_apply_scripts: vec![],
        pre_reconcile_scripts: vec![],
        post_reconcile_scripts: vec![],
        on_change_scripts: vec![],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let (env, aliases, _origins) = super::merge_module_env_aliases(
        &profile_env,
        &profile_aliases,
        &Default::default(),
        &modules,
    );
    // Module overrides profile: A=2 (module wins), B=3 (new)
    assert_eq!(env.len(), 2);
    assert_eq!(env.iter().find(|e| e.name == "A").unwrap().value, "2");
    assert_eq!(env.iter().find(|e| e.name == "B").unwrap().value, "3");
    // Module overrides alias: g="git status" (module wins)
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].command, "git status");
}

#[test]
fn generate_powershell_env_escapes_single_quotes() {
    let env = vec![crate::config::EnvVar {
        name: "MSG".into(),
        value: "it's a test".into(),
        platforms: vec![],
    }];
    let content = super::generate_powershell_env_content(&env, &[], None, &Default::default());
    // Single quotes in values are doubled in PS
    assert!(content.contains("$env:MSG = 'it''s a test'"));
}

#[test]
fn generate_fish_env_escapes_single_quotes() {
    let env = vec![crate::config::EnvVar {
        name: "MSG".into(),
        value: "it's a test".into(),
        platforms: vec![],
    }];
    let content = super::generate_fish_env_content(&env, &[], None, &Default::default());
    assert!(content.contains("set -gx MSG 'it\\'s a test'"));
}

#[test]
fn reconcile_context_equality() {
    assert_eq!(ReconcileContext::Apply, ReconcileContext::Apply);
    assert_eq!(ReconcileContext::Reconcile, ReconcileContext::Reconcile);
    assert_ne!(ReconcileContext::Apply, ReconcileContext::Reconcile);
}

#[test]
#[cfg(unix)]
fn apply_on_change_skipped_when_skip_scripts_true() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    let target = dir.path().join("target.txt");
    let marker = dir.path().join("on_change_marker_skip");

    std::fs::write(&source, "data").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.on_change =
        vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

    let file_actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            file_actions,
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    // skip_scripts = true
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            true, // skip_scripts
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // onChange should NOT have run because skip_scripts=true
    assert!(
        !marker.exists(),
        "onChange should be skipped when skip_scripts=true"
    );
    // But the file action should still have been applied
    assert!(target.exists());
}

// --- apply_package_action: Bootstrap path ---

/// A package manager that starts unavailable but becomes available after bootstrap.
struct BootstrappablePackageManager {
    name: String,
    bootstrapped: std::sync::Mutex<bool>,
    installed: std::sync::Mutex<HashSet<String>>,
}

impl BootstrappablePackageManager {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            bootstrapped: std::sync::Mutex::new(false),
            installed: std::sync::Mutex::new(HashSet::new()),
        }
    }
}

impl PackageManager for BootstrappablePackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        *self.bootstrapped.lock().unwrap()
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        Some(crate::providers::BootstrapPlan::new("stub"))
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        *self.bootstrapped.lock().unwrap() = true;
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(self.installed.lock().unwrap().clone())
    }
    fn install(&self, packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.insert(p.clone());
        }
        Ok(())
    }
    fn uninstall(&self, packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.remove(p);
        }
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[test]
fn apply_manager_provision_makes_manager_available() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(BootstrappablePackageManager::new("snap")));

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: "snap".to_string(),
                via: "stub".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    };

    let (result, _) = apply_manager_plan(&registry, &state, &plan);

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0].description.contains("provision"),
        "desc: {}",
        result.action_results[0].description
    );

    // Manager should now be available
    assert!(registry.package_managers()[0].is_available());
}

/// An apply that installs a package settles the drift rows the two live
/// producers mint for it — the CLI's per-package `<mgr>:<pkg>` and the
/// daemon's batch `<mgr>:<a>,<b>` — immediately, not at the next scan.
/// `managed_resources` tracks under a third grammar (`<mgr>/<pkg>`), which no
/// drift writer mints; resolving under it healed nothing, so an installed
/// package kept reporting drift until a later check happened to re-look.
#[test]
fn an_apply_that_installs_packages_resolves_both_producers_drift_rows() {
    let state = test_state();
    for rid in ["brew:jq", "brew:rg", "brew:jq,rg", "brew:bystander"] {
        state
            .record_drift("package", rid, Some("installed"), Some("missing"), "local")
            .unwrap();
    }
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "brew",
    )));

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Package(crate::providers::PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["jq".to_string(), "rg".to_string()],
                origin: "profile".to_string(),
            })],
        )],
        warnings: vec![],
    };
    let (result, _) = apply_manager_plan(&registry, &state, &plan);
    assert_eq!(result.status, ApplyStatus::Success);

    let mut standing: Vec<String> = state
        .unresolved_drift()
        .unwrap()
        .into_iter()
        .map(|e| e.resource_id)
        .collect();
    standing.sort_unstable();
    assert_eq!(
        standing,
        vec!["brew:bystander".to_string()],
        "the per-package rows and the batch row resolve with the apply; \
         a package this apply did not install stands"
    );
}

/// An apply that provisions a manager settles the finding both producers
/// record for the missing tooling — `("package", "provision:<mgr>")` and its
/// `refuse:` twin — immediately. A row about a manager this apply did not
/// provision stands.
#[test]
fn an_apply_that_provisions_a_manager_resolves_both_provision_findings() {
    let state = test_state();
    for rid in ["provision:snap", "refuse:snap", "provision:other"] {
        state
            .record_drift("package", rid, Some("available"), Some("missing"), "local")
            .unwrap();
    }
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(BootstrappablePackageManager::new("snap")));

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: "snap".to_string(),
                via: "stub".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    };
    let (result, _) = apply_manager_plan(&registry, &state, &plan);
    assert_eq!(result.status, ApplyStatus::Success);

    let mut standing: Vec<String> = state
        .unresolved_drift()
        .unwrap()
        .into_iter()
        .map(|e| e.resource_id)
        .collect();
    standing.sort_unstable();
    assert_eq!(
        standing,
        vec!["provision:other".to_string()],
        "the landed provision resolves its own finding and its refuse: twin; \
         another manager's finding stands"
    );
}

/// The registry answers availability from a memoized sweep, and the dispatcher
/// keeps ASKING it rather than snapshotting — so a provision that lands a
/// manager mid-run has to retire the sweep taken before it, or every question
/// asked for the rest of the run is answered from a picture of the machine that
/// predates the install.
#[test]
fn a_provisioned_manager_appears_in_the_registrys_next_availability_sweep() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(BootstrappablePackageManager::new("snap")));

    // The sweep the dispatcher would already be holding when the node runs.
    assert!(registry.available_package_managers().is_empty());

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: "snap".to_string(),
                via: "stub".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    };
    let (result, _) = apply_manager_plan(&registry, &state, &plan);
    assert_eq!(result.status, ApplyStatus::Success);

    let available = registry.available_package_managers();
    assert_eq!(
        available.len(),
        1,
        "the provision must retire the old sweep"
    );
    assert_eq!(available[0].name(), "snap");
}

/// A manager whose `install()` lands a real executable in a directory that was
/// already on `PATH`, and whose `uninstall()` takes it away again — the
/// `apt install curl` / `apt remove curl` shape. Neither direction registers or
/// unregisters a directory, so nothing about `PATH` changes and the action
/// itself is the only thing that can report the machine moved.
struct PathPopulatingManager {
    dir: std::path::PathBuf,
    stem: String,
}

impl PathPopulatingManager {
    /// Where this manager's binary lives, under the name the host resolves it by.
    fn binary(&self) -> std::path::PathBuf {
        let name = if cfg!(windows) {
            format!("{}.exe", self.stem)
        } else {
            self.stem.clone()
        };
        self.dir.join(name)
    }
}

impl PackageManager for PathPopulatingManager {
    fn name(&self) -> &str {
        "writer"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        None
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        let path = self.binary();
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n")?;
        crate::set_file_permissions(&path, 0o755)?;
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        std::fs::remove_file(self.binary())?;
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// An install that lands a binary somewhere `PATH` already pointed makes the
/// answer to "is this tool here" different, with no directory registered and no
/// `PATH` change to notice — so the install has to say so itself.
#[test]
#[serial_test::serial]
fn an_install_into_a_directory_already_on_path_retires_the_memoized_miss() {
    // Declared before the `EnvVarGuard` below so it drops last.
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
    let dir = tempfile::tempdir().expect("tempdir");
    let _path = crate::test_helpers::EnvVarGuard::set("PATH", &dir.path().to_string_lossy());
    let stem = "cfgd-probe-installed-by-apply";

    assert!(
        !crate::command_available(stem),
        "the tool is not there yet — and this miss is what gets memoized"
    );

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(PathPopulatingManager {
        dir: dir.path().to_path_buf(),
        stem: stem.to_string(),
    }));
    // The action runs through `PackageExec` directly rather than through
    // `apply()`: the lane dispatcher refuses to run while this thread holds the
    // PATH mutation guard, and a worker's own read guard would deadlock behind
    // it. What is under test is the exec's own invalidation either way.
    let (printer, _buf) = Printer::for_test();
    let notes = crate::providers::NoteSink::discarded();
    let exec = crate::reconciler::packages::PackageExec::new(&registry, &state, &printer, notes);
    exec.apply_package_action(&PackageAction::Install {
        manager: "writer".to_string(),
        packages: vec!["tool".to_string()],
        origin: "local".to_string(),
    })
    .expect("install");

    assert!(
        crate::command_available(stem),
        "a tool this run installed must be resolvable to the actions after it"
    );
}

/// The mirror claim: an uninstall takes a binary off `PATH` with no directory
/// change either, so a memoized HIT outlives the thing it describes unless the
/// removal reports itself. Reachable as a daemon tick that removes a tool and a
/// following tick that plans against `command_available` for it.
#[test]
#[serial_test::serial]
fn an_uninstall_that_removes_a_binary_retires_the_memoized_hit() {
    // Declared before the `EnvVarGuard` below so it drops last.
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
    // The claim is that the UNINSTALL retires the entry; a TTL expiry between
    // the two lookups would retire it for an unrelated reason and pass anyway.
    let _ttl = crate::test_helpers::CommandPathMemoTtlGuard::never_expires();
    let dir = tempfile::tempdir().expect("tempdir");
    let _path = crate::test_helpers::EnvVarGuard::set("PATH", &dir.path().to_string_lossy());
    let stem = "cfgd-probe-removed-by-apply";

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(PathPopulatingManager {
        dir: dir.path().to_path_buf(),
        stem: stem.to_string(),
    }));
    let (printer, _buf) = Printer::for_test();
    let notes = crate::providers::NoteSink::discarded();
    // Driven through `PackageExec` directly for the same reason the install
    // sibling is: the lane dispatcher refuses to run while this thread holds the
    // PATH mutation guard.
    let exec = crate::reconciler::packages::PackageExec::new(&registry, &state, &printer, notes);
    exec.apply_package_action(&PackageAction::Install {
        manager: "writer".to_string(),
        packages: vec!["tool".to_string()],
        origin: "local".to_string(),
    })
    .expect("install");

    assert!(
        crate::command_available(stem),
        "the tool is here — and this hit is what gets memoized"
    );

    exec.apply_package_action(&PackageAction::Uninstall {
        manager: "writer".to_string(),
        packages: vec!["tool".to_string()],
        origin: "local".to_string(),
    })
    .expect("uninstall");

    assert!(
        !crate::command_available(stem),
        "a tool this run removed must not still resolve for the actions after it"
    );
}

#[test]
fn apply_manager_provision_unknown_manager_errors() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no managers

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: "nonexistent".to_string(),
                via: "stub".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    };

    let (result, _) = apply_manager_plan(&registry, &state, &plan);

    // Should fail — unknown manager
    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
    assert!(result.action_results[0].error.is_some());
}

/// A declared route's verification failure names the package the route chose.
///
/// The install ran and succeeded — it was `apt-get install rustc` — and the
/// tool is still absent, because `rustc` does not provide `/usr/bin/cargo`.
/// Reporting only "cargo still not available after bootstrap" asserts a
/// post-condition and names neither the installer that ran nor the package it
/// landed, so the reader is told cfgd's provisioning broke rather than that
/// their own `aliases:` entry cannot deliver the tool.
#[test]
fn a_declared_routes_verification_failure_names_the_package_it_installed() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("cargo").unavailable(),
    ));
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "apt",
    )));

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::cfgd("managers"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: "cargo".to_string(),
                via: "apt".to_string(),
                declared: Some(crate::reconciler::types::DeclaredProvision {
                    installer: "apt".to_string(),
                    package: "rustc".to_string(),
                }),
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    };

    let (result, _) = apply_manager_plan(&registry, &state, &plan);
    assert_eq!(result.failed(), 1);
    let error = result.action_results[0]
        .error
        .clone()
        .expect("the verification refuses an absent tool");
    assert!(
        error.contains("rustc") && error.contains("apt"),
        "the failure must name the installer that ran and the package it landed, got {error:?}"
    );
    assert!(
        !error.contains("bootstrap"),
        "a declared route runs no cascade, so its failure may not call itself a bootstrap: {error:?}"
    );
    assert!(
        !error.contains("package error"),
        "the sentence names its own subject, so it opens on no category label: {error:?}"
    );
}

#[test]
fn an_unprovisioned_managers_install_names_a_recovery_that_holds_off_a_filter() {
    // The reach path the error's own comment once denied: no phase filter, a
    // provision node that ran and failed, and the install behind it still
    // dispatched — so a recovery naming `--phase` would name a flag the user
    // never typed.
    let harness = crate::test_helpers::ReconcilerTestHarness::builder()
        .with_package_manager(
            crate::test_helpers::MockPackageManager::new("stub")
                .unavailable()
                .bootstrappable(),
        )
        .build();
    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::cfgd("managers"),
                vec![Action::Manager(ManagerAction::Provision {
                    manager: "stub".to_string(),
                    via: "mock".to_string(),
                    declared: None,
                    batched: vec![],
                    depends_on: vec![],
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Package(PackageAction::Install {
                    manager: "stub".to_string(),
                    packages: vec!["foo".to_string()],
                    origin: "local".to_string(),
                })],
            ),
        ],
        warnings: vec![],
    };

    let (result, _) = apply_manager_plan(&harness.registry, &harness.state, &plan);

    assert_eq!(result.status, ApplyStatus::Failed);
    let install = result
        .action_results
        .iter()
        .find(|r| r.description.contains("package:stub"))
        .expect("the install is reported");
    let err = install.error.clone().unwrap_or_default();
    assert!(
        err.contains("stub is not provisioned") && err.contains("--phase prerequisites.managers"),
        "the install must name where provisioning happens: {err}"
    );
    assert!(
        !err.contains("drop --phase"),
        "this run carried no --phase to drop: {err}"
    );
}

#[test]
fn apply_package_install_unknown_manager_errors() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no managers
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Package(PackageAction::Install {
                manager: "nonexistent".to_string(),
                packages: vec!["foo".to_string()],
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
    let error = result.action_results[0]
        .error
        .as_deref()
        .unwrap_or_default();
    assert!(
        error.contains("nonexistent") && !error.contains("prerequisites"),
        "a manager never registered at all gets no phase-run guidance — nothing can provision a name that doesn't exist: {error}"
    );
}

#[test]
fn apply_package_uninstall_unknown_manager_errors() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Package(PackageAction::Uninstall {
                manager: "nonexistent".to_string(),
                packages: vec!["foo".to_string()],
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
    let error = result.action_results[0]
        .error
        .as_deref()
        .unwrap_or_default();
    assert!(
        error.contains("nonexistent") && !error.contains("prerequisites"),
        "a manager never registered at all gets no phase-run guidance — nothing can provision a name that doesn't exist: {error}"
    );
}

// --- apply_secret_action: Decrypt, Resolve, ResolveEnv ---

struct TestSecretBackend {
    decrypted_value: String,
}

impl crate::providers::SecretBackend for TestSecretBackend {
    fn name(&self) -> &str {
        "test-sops"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn encrypt_file(&self, _path: &std::path::Path) -> Result<()> {
        Ok(())
    }
    fn decrypt_file(&self, _path: &std::path::Path) -> Result<secrecy::SecretString> {
        Ok(secrecy::SecretString::from(self.decrypted_value.clone()))
    }
    fn edit_file(&self, _path: &std::path::Path) -> Result<()> {
        Ok(())
    }
}

#[test]
fn apply_secret_decrypt_writes_decrypted_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("token.enc");
    let target = dir.path().join("token.txt");
    std::fs::write(&source, "encrypted-data").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_backend = Some(Box::new(TestSecretBackend {
        decrypted_value: "my-secret-token".to_string(),
    }));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![Action::Secret(SecretAction::Decrypt {
                source: source.clone(),
                target: target.clone(),
                backend: "test-sops".to_string(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Secrets)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0].description.contains("decrypt"),
        "desc: {}",
        result.action_results[0].description
    );

    // Verify decrypted file was written
    let content = std::fs::read_to_string(&target).unwrap();
    assert_eq!(content, "my-secret-token");
}

#[test]
fn plan_secret_decrypt_target_is_tilde_expanded() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_backend = Some(Box::new(TestSecretBackend {
        decrypted_value: "x".to_string(),
    }));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.secrets.push(crate::config::SecretSpec {
        source: "secrets/token.age".to_string(),
        target: Some(PathBuf::from("~/cfgd-secret")),
        template: None,
        backend: None,
        envs: None,
    });

    let actions = reconciler.plan_secrets(&profile);
    let target = actions
        .iter()
        .find_map(|a| match a {
            Action::Secret(SecretAction::Decrypt { target, .. }) => Some(target),
            _ => None,
        })
        .expect("expected a Decrypt action");
    // The plan must report the same absolute path apply writes to — not a literal "~".
    assert_eq!(target, &tmp_home.path().join("cfgd-secret"));
}

#[test]
fn apply_secret_decrypt_no_backend_errors() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("token.enc");
    let target = dir.path().join("token.txt");
    std::fs::write(&source, "encrypted-data").unwrap();

    let state = test_state();
    let registry = ProviderRegistry::new(); // no backend

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![Action::Secret(SecretAction::Decrypt {
                source: source.clone(),
                target: target.clone(),
                backend: "sops".to_string(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Secrets)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
}

#[test]
fn apply_secret_resolve_writes_provider_value_to_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("resolved-secret.txt");

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("provider-secret-value"),
    ));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![Action::Secret(SecretAction::Resolve {
                provider: "vault".to_string(),
                reference: "secret/data/app#key".to_string(),
                target: target.clone(),
                template: None,
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Secrets)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0].description.contains("resolve"),
        "desc: {}",
        result.action_results[0].description
    );

    let content = std::fs::read_to_string(&target).unwrap();
    assert_eq!(content, "provider-secret-value");
}

/// `spec.secrets[].template` wraps the resolved value on BOTH delivery paths
/// of one entry: the file gets the rendered template, and so does every env
/// var, so a reader of either sees the same bytes. The value is substituted
/// for every `${secret:value}` and nothing else in the template is touched.
#[test]
fn apply_secret_resolve_renders_template_around_the_value_for_file_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("gh-token.yaml");

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("1password").with_resolve_result("ghp_abc123"),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let template = Some("token: ${secret:value}\nhome: $HOME\n".to_string());

    let mut collector: Vec<(String, String)> = Vec::new();
    reconciler
        .apply_secret_action(
            &SecretAction::Resolve {
                provider: "1password".to_string(),
                reference: "Work/GitHub/token".to_string(),
                target: target.clone(),
                template: template.clone(),
                origin: "local".to_string(),
            },
            dir.path(),
            &mut collector,
        )
        .expect("resolve should succeed");
    reconciler
        .apply_secret_action(
            &SecretAction::ResolveEnv {
                provider: "1password".to_string(),
                reference: "Work/GitHub/token".to_string(),
                envs: vec!["GITHUB_TOKEN".to_string()],
                template,
                origin: "local".to_string(),
            },
            dir.path(),
            &mut collector,
        )
        .expect("resolve-env should succeed");

    let rendered = "token: ghp_abc123\nhome: $HOME\n";
    assert_eq!(std::fs::read_to_string(&target).unwrap(), rendered);
    assert_eq!(
        collector,
        vec![("GITHUB_TOKEN".to_string(), rendered.to_string())]
    );
}

#[test]
fn apply_secret_resolve_unknown_provider_errors() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nope.txt");

    let state = test_state();
    let registry = ProviderRegistry::new(); // no providers

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![Action::Secret(SecretAction::Resolve {
                provider: "vault".to_string(),
                reference: "secret/data/app#key".to_string(),
                target: target.clone(),
                template: None,
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Secrets)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
}

#[test]
fn apply_secret_resolve_env_collects_env_vars() {
    // Unit test the collector-population behaviour directly via
    // `apply_secret_action`. The full `Reconciler::apply` path calls
    // `plan_env()` which resolves `~` to the real `$HOME` and writes
    // `~/.cfgd.env` + injects a source line into `~/.bashrc` — tests must
    // never touch the user's home.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("env-secret-value"),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let tmp = tempfile::tempdir().unwrap();

    let mut collector: Vec<(String, String)> = Vec::new();
    let action = SecretAction::ResolveEnv {
        provider: "vault".to_string(),
        reference: "secret/data/gh#token".to_string(),
        envs: vec!["GH_TOKEN".to_string(), "GITHUB_TOKEN".to_string()],
        template: None,
        origin: "local".to_string(),
    };

    let desc = reconciler
        .apply_secret_action(&action, tmp.path(), &mut collector)
        .expect("resolve-env should succeed");

    assert!(desc.contains("resolve-env"), "desc: {}", desc);
    assert_eq!(
        collector,
        vec![
            ("GH_TOKEN".to_string(), "env-secret-value".to_string()),
            ("GITHUB_TOKEN".to_string(), "env-secret-value".to_string()),
        ]
    );
}

#[test]
fn apply_secret_action_resource_ids_fold_the_target_path_to_posix() {
    // Both ids embed the write target. Rendering it natively made a
    // Windows-written key (`secret:decrypt:C:\…`) miss the POSIX key every
    // other code path produces, so the same secret was tracked twice.
    // `to_posix_string` folds on every host — unlike `posix()`, which is a
    // no-op on unix — so the fold is observable here without cross-compiling,
    // using a backslash-bearing file name (legal on unix).
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_backend = Some(Box::new(TestSecretBackend {
        decrypted_value: "plaintext".to_string(),
    }));
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("resolved"),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let tmp = tempfile::tempdir().unwrap();

    let source = tmp.path().join("token.enc");
    std::fs::write(&source, "encrypted").unwrap();

    let mut collector: Vec<(String, String)> = Vec::new();
    for (action, expected) in [
        (
            SecretAction::Decrypt {
                source: source.clone(),
                target: tmp.path().join(r"win\token.txt"),
                backend: "test-sops".to_string(),
                origin: "local".to_string(),
            },
            format!(
                "secret:decrypt:{}/win/token.txt",
                crate::to_posix_string(tmp.path())
            ),
        ),
        (
            SecretAction::Resolve {
                provider: "vault".to_string(),
                reference: "secret/data/gh#token".to_string(),
                target: tmp.path().join(r"win\resolved.txt"),
                template: None,
                origin: "local".to_string(),
            },
            format!(
                "secret:resolve:vault:{}/win/resolved.txt",
                crate::to_posix_string(tmp.path())
            ),
        ),
    ] {
        let desc = reconciler
            .apply_secret_action(&action, tmp.path(), &mut collector)
            .expect("secret action should succeed");
        assert!(
            !desc.contains('\\'),
            "resource id must carry no native separator: {desc}"
        );
        assert_eq!(desc, expected);
    }
}

/// A provider that counts how many times it was asked to resolve, so a test can
/// assert on SPAWNS rather than on the value that came back.
struct CountingSecretProvider {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::providers::SecretProvider for CountingSecretProvider {
    fn name(&self) -> &str {
        "vault"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn resolve(&self, _reference: &str) -> Result<secrecy::SecretString> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(secrecy::SecretString::from("resolved-once".to_string()))
    }
}

/// The backend counterpart: counts decryptions of a file reference.
struct CountingSecretBackend {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::providers::SecretBackend for CountingSecretBackend {
    fn name(&self) -> &str {
        "test-sops"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn encrypt_file(&self, _path: &std::path::Path) -> Result<()> {
        Ok(())
    }
    fn decrypt_file(&self, _path: &std::path::Path) -> Result<secrecy::SecretString> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(secrecy::SecretString::from("decrypted-once".to_string()))
    }
    fn edit_file(&self, _path: &std::path::Path) -> Result<()> {
        Ok(())
    }
}

#[test]
fn one_reference_spawns_its_provider_once_across_both_of_its_occurrences() {
    // A declared secret with both a target and `envs` plans a `Resolve` for the
    // file and a `ResolveEnv` for the variables — two actions, ONE value. Each
    // used to spawn `op read` / `vault kv get` for itself.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .secret_providers
        .push(Box::new(CountingSecretProvider {
            calls: std::sync::Arc::clone(&calls),
        }));
    let reconciler = Reconciler::new(&registry, &state);
    let tmp = tempfile::tempdir().unwrap();

    let mut collector: Vec<(String, String)> = Vec::new();
    reconciler
        .apply_secret_action(
            &SecretAction::Resolve {
                provider: "vault".to_string(),
                reference: "secret/data/gh#token".to_string(),
                target: tmp.path().join("token.txt"),
                template: None,
                origin: "local".to_string(),
            },
            tmp.path(),
            &mut collector,
        )
        .expect("resolve should succeed");
    reconciler
        .apply_secret_action(
            &SecretAction::ResolveEnv {
                provider: "vault".to_string(),
                reference: "secret/data/gh#token".to_string(),
                envs: vec!["GH_TOKEN".to_string()],
                template: None,
                origin: "local".to_string(),
            },
            tmp.path(),
            &mut collector,
        )
        .expect("resolve-env should succeed");

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the file and env occurrences of one reference must share one resolution"
    );
    // Both surfaces still carry the value.
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("token.txt")).unwrap(),
        "resolved-once"
    );
    assert_eq!(
        collector,
        vec![("GH_TOKEN".to_string(), "resolved-once".to_string())]
    );
}

#[test]
fn two_references_of_one_provider_each_spawn_their_own_resolution() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .secret_providers
        .push(Box::new(CountingSecretProvider {
            calls: std::sync::Arc::clone(&calls),
        }));
    let reconciler = Reconciler::new(&registry, &state);
    let tmp = tempfile::tempdir().unwrap();

    let mut collector: Vec<(String, String)> = Vec::new();
    for reference in ["secret/data/gh#token", "secret/data/aws#key"] {
        reconciler
            .apply_secret_action(
                &SecretAction::ResolveEnv {
                    provider: "vault".to_string(),
                    reference: reference.to_string(),
                    envs: vec!["TOKEN".to_string()],
                    template: None,
                    origin: "local".to_string(),
                },
                tmp.path(),
                &mut collector,
            )
            .expect("resolve-env should succeed");
    }

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "distinct references are distinct questions"
    );
}

#[test]
fn one_encrypted_file_decrypts_once_however_many_targets_it_feeds() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_backend = Some(Box::new(CountingSecretBackend {
        calls: std::sync::Arc::clone(&calls),
    }));
    let reconciler = Reconciler::new(&registry, &state);

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("token.enc");
    std::fs::write(&source, "encrypted").unwrap();

    let mut collector: Vec<(String, String)> = Vec::new();
    for target in ["a.txt", "b.txt"] {
        reconciler
            .apply_secret_action(
                &SecretAction::Decrypt {
                    source: source.clone(),
                    target: tmp.path().join(target),
                    backend: "test-sops".to_string(),
                    origin: "local".to_string(),
                },
                tmp.path(),
                &mut collector,
            )
            .expect("decrypt should succeed");
    }

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one source file is one decryption however many targets it lands in"
    );
    for target in ["a.txt", "b.txt"] {
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(target)).unwrap(),
            "decrypted-once"
        );
    }
}

#[test]
fn a_second_run_resolves_its_own_secrets() {
    // The memo is the RUN's, never the process's: a rotated secret must be
    // re-fetched by the next reconciler rather than answered out of the last
    // one's memory.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .secret_providers
        .push(Box::new(CountingSecretProvider {
            calls: std::sync::Arc::clone(&calls),
        }));
    let tmp = tempfile::tempdir().unwrap();

    for _ in 0..2 {
        let reconciler = Reconciler::new(&registry, &state);
        let mut collector: Vec<(String, String)> = Vec::new();
        reconciler
            .apply_secret_action(
                &SecretAction::ResolveEnv {
                    provider: "vault".to_string(),
                    reference: "secret/data/gh#token".to_string(),
                    envs: vec!["GH_TOKEN".to_string()],
                    template: None,
                    origin: "local".to_string(),
                },
                tmp.path(),
                &mut collector,
            )
            .expect("resolve-env should succeed");
    }

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn apply_secret_resolve_env_unknown_provider_errors() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no providers

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![Action::Secret(SecretAction::ResolveEnv {
                provider: "vault".to_string(),
                reference: "secret/data/gh#token".to_string(),
                envs: vec!["GH_TOKEN".to_string()],
                template: None,
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Secrets)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
}

#[test]
fn apply_secret_skip_succeeds() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![Action::Secret(SecretAction::Skip {
                source: "vault://test".to_string(),
                reason: "not available".to_string(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Secrets)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(result.action_results[0].description.contains("skip"));
}

// --- apply_file_action: Delete and SetPermissions ---

#[test]
fn apply_file_delete_action_removes_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("to-delete.txt");
    std::fs::write(&target, "delete me").unwrap();
    assert!(target.exists());

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::File(FileAction::Delete {
                target: target.clone(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(!target.exists(), "file should be deleted");
    assert!(
        result.action_results[0].description.contains("delete"),
        "desc: {}",
        result.action_results[0].description
    );
}

#[test]
#[cfg(unix)]
fn apply_file_set_permissions_action() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("script.sh");
    std::fs::write(&target, "#!/bin/sh\necho hi").unwrap();

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::File(FileAction::SetPermissions {
                target: target.clone(),
                mode: 0o755,
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0].description.contains("chmod"),
        "desc: {}",
        result.action_results[0].description
    );

    // Verify permissions
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&target).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o755);
}

#[test]
fn apply_file_skip_action_succeeds() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::File(FileAction::Skip {
                target: PathBuf::from("/nonexistent"),
                reason: "unchanged".to_string(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(result.action_results[0].description.contains("skip"));
}

#[test]
fn apply_file_update_action_overwrites_target() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("new-content.txt");
    let target = dir.path().join("existing.txt");
    std::fs::write(&source, "updated content").unwrap();
    std::fs::write(&target, "old content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::File(FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: "diff output".to_string(),
                origin: "local".to_string(),
                strategy: crate::config::FileStrategy::Copy,
                source_hash: None,
                patch: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    let content = std::fs::read_to_string(&target).unwrap();
    assert_eq!(content, "updated content");
    assert!(
        result.action_results[0].description.contains("update"),
        "desc: {}",
        result.action_results[0].description
    );
}

// --- apply_system_action: SetValue and Skip ---

#[test]
fn apply_system_set_value_calls_configurator() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
        vec![crate::providers::SystemDrift {
            key: "test.key".to_string(),
            expected: "desired-val".to_string(),
            actual: "current-val".to_string(),
        }],
    )));

    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    // Put desired system config in the profile
    resolved.merged.system.insert(
        "sysctl".to_string(),
        serde_yaml::from_str("{net.ipv4.ip_forward: 1}").unwrap(),
    );

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::System,
            &Owner::profile("test"),
            vec![Action::System(SystemAction::SetValue {
                configurator: "sysctl".to_string(),
                key: "net.ipv4.ip_forward".to_string(),
                desired: "1".to_string(),
                current: "0".to_string(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::System)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0]
            .description
            .contains("system:sysctl"),
        "desc: {}",
        result.action_results[0].description
    );
}

#[test]
fn apply_system_set_value_applies_module_contributed_system() {
    // Regression: a `spec.system` key declared only in a MODULE (absent from the
    // profile) must still be applied. The plan emits the SetValue from the
    // effective (profile ⊕ modules) map; the executor must resolve the desired
    // value from the same map, not profile.system alone — otherwise the action
    // plans but the apply silently no-ops (no ~/.gitconfig write, etc.).
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
        vec![crate::providers::SystemDrift {
            key: "net.ipv4.ip_forward".to_string(),
            expected: "1".to_string(),
            actual: "0".to_string(),
        }],
    )));

    let reconciler = Reconciler::new(&registry, &state);
    // Profile carries NO system config — the setting exists only in the module.
    let resolved = make_empty_resolved();
    let mut module = crate::test_helpers::make_resolved_module("netmod");
    module.packages.clear();
    module.system.insert(
        "sysctl".to_string(),
        serde_yaml::from_str("{net.ipv4.ip_forward: 1}").unwrap(),
    );

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::System,
            &Owner::profile("test"),
            vec![Action::System(SystemAction::SetValue {
                configurator: "sysctl".to_string(),
                key: "net.ipv4.ip_forward".to_string(),
                desired: "1".to_string(),
                current: "0".to_string(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::System)),
            std::slice::from_ref(&module),
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // The "→" arrow in the description appears ONLY in the apply branch (the
    // configurator's apply() was actually invoked). The pre-fix no-op path
    // returned a bare "system:sysctl.<key>" with no arrow.
    assert!(
        result.action_results[0].description.contains('\u{2192}'),
        "module-contributed system setting must reach the configurator's apply (got: {})",
        result.action_results[0].description
    );
}

#[test]
fn apply_system_skip_logs_warning() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::System,
            &Owner::profile("test"),
            vec![Action::System(SystemAction::Skip {
                configurator: "customThing".to_string(),
                reason: "no configurator registered".to_string(),
                origin: "local".to_string(),
                unknown: true,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::System)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0].description.contains("skipped"),
        "desc: {}",
        result.action_results[0].description
    );
}

/// Drive a one-action `System` plan through the full apply and return the
/// stripped human transcript, so a skip's role is asserted where it renders.
fn system_skip_transcript(action: SystemAction) -> String {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::System,
            &Owner::profile("test"),
            vec![Action::System(action)],
        )],
        warnings: vec![],
    };

    let (printer, cap) = crate::output::Printer::for_test_doc();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("a system skip applies cleanly");
    crate::output::strip_ansi(&cap.human())
}

#[test]
fn apply_system_action_unknown_key_renders_warn() {
    // An unknown system key (no configurator registered) is a likely typo and
    // must surface as a real warning (⚠) on its own action line in the tree,
    // not a neutral skip.
    let out = system_skip_transcript(SystemAction::Skip {
        configurator: "gti".to_string(),
        reason: "no configurator registered for 'gti'".to_string(),
        origin: "local".to_string(),
        unknown: true,
    });

    assert!(
        out.contains('\u{26A0}'),
        "unknown key must warn (⚠), got: {out}"
    );
    // Byte-identical, not a substring: the sentence moved out of
    // `apply_system_action` into `format_plan_items` includes the
    // ` — no such configurator (ignored)` half, and a substring assertion
    // would pass with that half missing.
    let line = out
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with('\u{26A0}'))
        .unwrap_or_else(|| panic!("no warn line in: {out}"));
    assert_eq!(
        line,
        "\u{26A0} unknown system key 'gti' — no such configurator (ignored)"
    );
}

#[test]
fn apply_system_action_unavailable_renders_non_warn() {
    // A registered-but-unavailable configurator is expected; it must render
    // neutrally (Skipped, — glyph), never as a warning.
    let out = system_skip_transcript(SystemAction::Skip {
        configurator: "systemdUnits".to_string(),
        reason: "'systemdUnits' is not available on this host".to_string(),
        origin: "local".to_string(),
        unknown: false,
    });

    assert!(
        !out.contains('\u{26A0}'),
        "an expected platform skip must not warn (⚠), got: {out}"
    );
    assert!(
        out.contains("not available on this host"),
        "neutral skip must still explain why, got: {out}"
    );
}

#[test]
fn plan_system_emits_set_value_actions_per_drift() {
    // Covers plan.rs's system-drift branch: when a configurator returns drift entries,
    // each one becomes a SystemAction::SetValue with the drift fields.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
        vec![
            crate::providers::SystemDrift {
                key: "net.ipv4.ip_forward".to_string(),
                expected: "1".to_string(),
                actual: "0".to_string(),
            },
            crate::providers::SystemDrift {
                key: "vm.swappiness".to_string(),
                expected: "10".to_string(),
                actual: "60".to_string(),
            },
        ],
    )));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.system.insert(
        "sysctl".to_string(),
        serde_yaml::from_str("{net.ipv4.ip_forward: 1, vm.swappiness: 10}").unwrap(),
    );

    let actions = reconciler.plan_system(&profile, &[]).unwrap();
    let set_values: Vec<&SystemAction> = actions
        .iter()
        .filter_map(|a| {
            if let Action::System(sa @ SystemAction::SetValue { .. }) = a {
                Some(sa)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(set_values.len(), 2, "one SetValue per drift entry");
    for sa in &set_values {
        if let SystemAction::SetValue {
            configurator, key, ..
        } = sa
        {
            assert_eq!(configurator, "sysctl");
            assert!(["net.ipv4.ip_forward", "vm.swappiness"].contains(&key.as_str()));
        }
    }
}

#[test]
fn plan_system_generates_skip_for_unregistered_configurator() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no configurators
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.system.insert(
        "unknownConf".to_string(),
        serde_yaml::from_str("{key: value}").unwrap(),
    );

    let actions = reconciler.plan_system(&profile, &[]).unwrap();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::System(SystemAction::Skip {
            configurator,
            reason,
            unknown,
            ..
        }) => {
            assert_eq!(configurator, "unknownConf");
            assert!(reason.contains("no configurator registered"));
            assert!(*unknown, "unregistered key must be flagged unknown (typo)");
        }
        other => panic!("Expected SystemAction::Skip, got {:?}", other),
    }
}

#[test]
fn plan_system_skip_distinguishes_unavailable_from_unregistered() {
    // A configurator that IS registered but is unavailable on this host (e.g.
    // systemdUnits where systemctl is absent) must skip with an accurate reason,
    // not masquerade as "no configurator registered".
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(
        crate::test_helpers::MockSystemConfigurator::new("systemdUnits").unavailable(),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let mut profile = MergedProfile::default();
    profile.system.insert(
        "systemdUnits".to_string(),
        serde_yaml::from_str("[{name: x.service, enabled: true}]").unwrap(),
    );

    let actions = reconciler.plan_system(&profile, &[]).unwrap();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::System(SystemAction::Skip {
            configurator,
            reason,
            unknown,
            ..
        }) => {
            assert_eq!(configurator, "systemdUnits");
            assert!(
                reason.contains("not available on this host"),
                "reason should name the host-availability gap, got: {reason}"
            );
            assert!(
                !reason.contains("no configurator registered"),
                "registered-but-unavailable must not read as unregistered: {reason}"
            );
            assert!(
                !*unknown,
                "registered-but-unavailable must not be flagged unknown"
            );
        }
        other => panic!("Expected SystemAction::Skip, got {:?}", other),
    }
}

// --- apply_module_action: InstallPackages, DeployFiles, Skip ---

#[test]
fn apply_module_install_packages_calls_manager() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "neovim".to_string(),
            resolved_name: "neovim".to_string(),
            manager: "brew".to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "nvim".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "neovim".to_string(),
                        resolved_name: "neovim".to_string(),
                        manager: "brew".to_string(),
                        manager_declared: false,
                        version: None,
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: None,
                    }],
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0]
            .description
            .contains("module:nvim:packages"),
        "desc: {}",
        result.action_results[0].description
    );

    // Verify install was called
    let cx = test_package_context(&printer, &state);
    let installed = registry.package_managers()[0]
        .installed_packages(&cx)
        .unwrap();
    assert!(installed.contains("neovim"));
}

#[test]
fn apply_module_deploy_files_creates_target() {
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("module-source.txt");
    let target_file = dir.path().join("subdir/module-target.txt");
    std::fs::write(&source_file, "module content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source_file.clone(),
            target: target_file.clone(),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: {
                    let files = vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(target_file.exists(), "target file should be deployed");
    assert_eq!(
        std::fs::read_to_string(&target_file).unwrap(),
        "module content"
    );
}

#[test]
fn apply_module_deploy_files_leaves_a_target_that_already_holds_the_source_bytes() {
    // Re-deploying content the target already holds must not back it up,
    // rewrite it, or report the run as having changed anything.
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("module-source.txt");
    let target_file = dir.path().join("module-target.txt");
    std::fs::write(&source_file, "module content").unwrap();
    std::fs::write(&target_file, "module content").unwrap();
    // A hard link shares the target's inode; an atomic rewrite rename-replaces
    // the target and breaks that identity, so this witnesses the write itself
    // rather than a timestamp the filesystem may round.
    let witness = dir.path().join("witness");
    std::fs::hard_link(&target_file, &witness).unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let file = ResolvedFile {
        source: source_file.clone(),
        target: target_file.clone(),
        is_git_source: false,
        strategy: Some(crate::config::FileStrategy::Copy),
        encryption: None,
        permissions: None,
        patch: None,
    };
    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![file.clone()],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![file],
                    declared_total: 1,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        !result.action_results[0].changed,
        "a deployment that wrote nothing must not claim a change"
    );
    assert!(
        crate::is_same_inode(&target_file, &witness),
        "a converged target must not be rewritten"
    );
    // No rows at all: no pre-write backup, because nothing was overwritten, and
    // therefore no post-apply snapshot either — the snapshot follows the
    // touched set, and this target is not in it.
    let key = crate::to_posix_fs_key(&target_file);
    let rows = state
        .get_apply_backups(result.apply_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.file_path == key)
        .count();
    assert_eq!(
        rows, 0,
        "a file that was never overwritten needs neither a pre-write backup row nor a post-apply snapshot"
    );
    // The manifest row still records the file as this module's, so removal
    // still cleans it up.
    assert!(
        state
            .module_deployed_files("mymod")
            .unwrap()
            .iter()
            .any(|f| f.file_path == crate::to_posix_fs_key(&target_file)),
        "the module must still own the file it deployed earlier"
    );
}

/// Deploy one module file under a global `fileStrategy: copy`, the way a config
/// that sets `spec.fileStrategy` and a module that declares nothing per-file do.
fn deploy_one_module_file_under_global_copy(
    dir: &std::path::Path,
    state: &crate::state::StateStore,
    file: ResolvedFile,
) -> crate::reconciler::ApplyResult {
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![file.clone()],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![file],
                    declared_total: 1,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            dir,
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap()
}

#[test]
fn apply_module_deploy_files_short_circuits_a_file_that_declares_no_strategy() {
    // The GLOBAL `fileStrategy` is what decides the write, so it is what has to
    // decide convergence. Judged on the per-file field, a module file that
    // declares no strategy answers "not a whole-content write" and is copied
    // aside and rewritten on every single apply.
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("module-source.txt");
    let target_file = dir.path().join("module-target.txt");
    std::fs::write(&source_file, "module content").unwrap();
    std::fs::write(&target_file, "module content").unwrap();
    let witness = dir.path().join("witness");
    std::fs::hard_link(&target_file, &witness).unwrap();

    let state = test_state();
    let result = deploy_one_module_file_under_global_copy(
        dir.path(),
        &state,
        ResolvedFile {
            source: source_file,
            target: target_file.clone(),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        },
    );

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        !result.action_results[0].changed,
        "a deployment that wrote nothing must not claim a change"
    );
    assert!(
        crate::is_same_inode(&target_file, &witness),
        "a file inheriting the global copy strategy must not be rewritten"
    );
}

#[cfg(unix)]
#[test]
fn apply_module_deploy_files_short_circuits_a_target_carrying_its_declared_setuid_bit() {
    // A declared mode may name a setuid/setgid/sticky bit (`parse_octal_mode`
    // accepts up to 0o7777). Compared against an actual masked to 0o777, such a
    // mode can never match, and the short-circuit is dead for the file's life.
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("module-source.sh");
    let target_file = dir.path().join("module-target.sh");
    std::fs::write(&source_file, "#!/bin/sh\n").unwrap();
    std::fs::write(&target_file, "#!/bin/sh\n").unwrap();
    crate::set_file_permissions(&target_file, 0o4755).unwrap();
    let witness = dir.path().join("witness");
    std::fs::hard_link(&target_file, &witness).unwrap();

    let state = test_state();
    let result = deploy_one_module_file_under_global_copy(
        dir.path(),
        &state,
        ResolvedFile {
            source: source_file,
            target: target_file.clone(),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: None,
            permissions: Some("4755".to_string()),
            patch: None,
        },
    );

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        !result.action_results[0].changed,
        "a target already carrying its declared special bits is converged"
    );
    assert!(
        crate::is_same_inode(&target_file, &witness),
        "a converged target must not be rewritten over a special-bit comparison"
    );
    let mode = crate::file_permissions_mode_full(&std::fs::metadata(&target_file).unwrap());
    assert_eq!(mode, Some(0o4755), "the declared special bit must survive");
}

#[test]
fn apply_module_deploy_files_patch_merges_into_the_target() {
    // A `Patch` module file has no source; the merge must run against the
    // target's own content and leave everything the spec does not name alone.
    let dir = tempfile::tempdir().unwrap();
    let target_file = dir.path().join("subdir/settings.json");
    std::fs::create_dir_all(target_file.parent().unwrap()).unwrap();
    std::fs::write(&target_file, "{\n  \"runtimeToken\": \"keep-me\"\n}\n").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let patch = crate::config::PatchSpec {
        format: None,
        ensure: Some(serde_yaml::from_str("telemetry: false").unwrap()),
        script: None,
        blocked_by: None,
    };
    let file = ResolvedFile {
        source: PathBuf::new(),
        target: target_file.clone(),
        is_git_source: false,
        strategy: Some(crate::config::FileStrategy::Patch),
        encryption: None,
        permissions: None,
        patch: Some(patch),
    };

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![file.clone()],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![file],
                    declared_total: 1,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target_file).unwrap()).unwrap();
    assert_eq!(
        written["runtimeToken"], "keep-me",
        "a key the spec never mentions must survive the merge"
    );
    assert_eq!(written["telemetry"], false);
}

/// Deploy one `Patch` module file (`ensure: telemetry: false`) against
/// `target` through the module dispatch site.
#[cfg(unix)]
fn deploy_patch_module_file(module_dir: &std::path::Path, target: &std::path::Path) {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let file = ResolvedFile {
        source: PathBuf::new(),
        target: target.to_path_buf(),
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
    };

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![file.clone()],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: module_dir.to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![file],
                    declared_total: 1,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            module_dir,
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);
}

#[test]
#[cfg(unix)]
fn apply_module_deploy_files_patch_preserves_the_targets_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let target_file = dir.path().join("subdir/settings.json");
    std::fs::create_dir_all(target_file.parent().unwrap()).unwrap();
    std::fs::write(&target_file, "{\n  \"runtimeToken\": \"keep-me\"\n}\n").unwrap();
    std::fs::set_permissions(&target_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    deploy_patch_module_file(dir.path(), &target_file);

    assert_eq!(
        std::fs::metadata(&target_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "the target's mode must survive the merge"
    );
}

#[test]
#[cfg(unix)]
fn apply_module_deploy_files_patch_writes_through_a_symlinked_target() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("repo").join("settings.json");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "{\n  \"runtimeToken\": \"keep-me\"\n}\n").unwrap();
    let target_file = dir.path().join("settings.json");
    crate::create_symlink(&real, &target_file).unwrap();

    deploy_patch_module_file(dir.path(), &target_file);

    assert!(
        target_file.is_symlink(),
        "the symlink must survive the merge"
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
    assert_eq!(written["runtimeToken"], "keep-me");
    assert_eq!(
        written["telemetry"], false,
        "the merge must land in the file the link points at"
    );
}

#[test]
#[cfg(unix)]
fn apply_module_deploy_files_symlink_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("source.txt");
    let target_file = dir.path().join("link-target.txt");
    std::fs::write(&source_file, "linked content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Symlink;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "linkmod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source_file.clone(),
            target: target_file.clone(),
            is_git_source: false,
            strategy: None, // uses default = Symlink
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "linkmod".to_string(),
                kind: {
                    let files = vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(target_file.is_symlink(), "target should be a symlink");
    assert_eq!(
        std::fs::read_to_string(&target_file).unwrap(),
        "linked content"
    );
}

#[test]
fn apply_module_skip_reports_skipped() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Modules,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "broken".to_string(),
                kind: ModuleActionKind::Skip {
                    reason: "dependency not met".to_string(),
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Modules)),
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(
        !result.action_results[0].changed,
        "a planned module skip did nothing and must record changed=false"
    );
    assert!(
        result.action_results[0].description.contains("skip"),
        "desc: {}",
        result.action_results[0].description
    );
}

#[test]
fn apply_module_install_packages_provisions_manager_when_needed() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(BootstrappablePackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "jq".to_string(),
            resolved_name: "jq".to_string(),
            manager: "brew".to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::profile("test"),
                vec![provision_node("brew", "stub", &[])],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "tools".to_string(),
                    kind: ModuleActionKind::InstallPackages {
                        resolved: vec![ResolvedPackage {
                            canonical_name: "jq".to_string(),
                            resolved_name: "jq".to_string(),
                            manager: "brew".to_string(),
                            manager_declared: false,
                            version: None,
                            script: None,
                            creates: None,
                            only_if: None,
                            unless: None,
                            min_version: None,
                        }],
                    },
                    origin: None,
                })],
            ),
        ],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results.iter().all(|r| r.success));

    // Manager should have been provisioned and package installed
    assert!(registry.package_managers()[0].is_available());
    let cx = test_package_context(&printer, &state);
    assert!(
        registry.package_managers()[0]
            .installed_packages(&cx)
            .unwrap()
            .contains("jq")
    );
}

/// A package a PREREQUISITE landed is not installed again by the `Packages`
/// phase.
///
/// The hero recording's own shape: `Phase: Prerequisites` runs `provision npm,
/// pipx via apt` — one `apt install npm pipx` — and `Phase: Packages` then
/// carried `npm` in the module's apt list, because the plan was priced before
/// the provision ran and the elision that dropped every other already-present
/// entry could not see this one. The cost was never cosmetic: an action with
/// nothing left to do still counted as a change, and that is what re-ran the
/// module's postApply hooks.
///
/// Both halves are pinned here: the surviving entry settles as a skip (RAN,
/// changed nothing) rather than as an install, and no `module:` result carries
/// `changed`, which is the exact predicate the postApply gate reads.
#[test]
fn a_package_a_prerequisite_landed_is_not_installed_again_by_the_packages_phase() {
    let installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provisioned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("sys")
            .recording_installs(std::sync::Arc::clone(&installs))
            .raising(std::sync::Arc::clone(&provisioned)),
    ));
    for mediated in ["npm", "pipx"] {
        registry.add_package_manager(Box::new(
            crate::test_helpers::MockPackageManager::new(mediated)
                .mediated_by("sys", &[mediated])
                .available_when(std::sync::Arc::clone(&provisioned)),
        ));
    }

    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // The module declares `npm` under the SYSTEM manager on purpose — apt's
    // nodejs package does not always carry npm — which is exactly the entry the
    // provision's own `apt install npm pipx` lands.
    let declared = || ResolvedPackage {
        canonical_name: "npm".to_string(),
        resolved_name: "npm".to_string(),
        manager: "sys".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![declared()],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::profile("test"),
                vec![Action::Manager(ManagerAction::Provision {
                    manager: "npm".to_string(),
                    via: "sys".to_string(),
                    declared: None,
                    batched: vec!["pipx".to_string()],
                    depends_on: Vec::new(),
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "tools".to_string(),
                    kind: ModuleActionKind::InstallPackages {
                        resolved: vec![declared()],
                    },
                    origin: None,
                })],
            ),
        ],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(
        installs.lock().unwrap().as_slice(),
        [vec!["npm".to_string(), "pipx".to_string()]],
        "the provision is the only `sys` install the run performs"
    );

    let packages = result
        .action_results
        .iter()
        .find(|r| r.description.starts_with("module:tools:packages:"))
        .expect("the module's package action settles a result");
    assert!(packages.success, "the action ran and did not fail");
    assert!(
        !packages.changed && packages.skipped,
        "an install whose every entry a prerequisite already landed is a skip: {packages:?}"
    );
    assert!(
        !result
            .action_results
            .iter()
            .any(|r| r.changed && r.description.starts_with("module:tools:")),
        "nothing marks the module changed, so its postApply hooks do not re-run"
    );
}

/// The RENDER half of the same finding: a row that installed fewer entries
/// than it named says so.
///
/// `e916beb6` fixed the machine and left the report — the executed row still
/// carried the PLANNED list, so a `✓` asserted an install of every package it
/// named while the manager had only been asked for one of them. The subject
/// stays the planned list on purpose (one string across the preview bullet,
/// the alignment column and the executed row) and so does the recorded
/// description, which is a wire contract; the shortfall belongs in the DETAIL,
/// the slot `deploy …/init.lua — 5 already deployed` already states its own in.
#[test]
fn an_install_that_landed_fewer_than_it_named_says_so_on_its_row() {
    let installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provisioned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("sys")
            .recording_installs(std::sync::Arc::clone(&installs))
            .raising(std::sync::Arc::clone(&provisioned)),
    ));
    for mediated in ["npm", "pipx"] {
        registry.add_package_manager(Box::new(
            crate::test_helpers::MockPackageManager::new(mediated)
                .mediated_by("sys", &[mediated])
                .available_when(std::sync::Arc::clone(&provisioned)),
        ));
    }

    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // Two entries under the system manager: `npm` is the one the provision's
    // own `sys install npm pipx` lands, `jq` is the one still to do.
    let declared = |name: &str| ResolvedPackage {
        canonical_name: name.to_string(),
        resolved_name: name.to_string(),
        manager: "sys".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![declared("npm"), declared("jq")],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::profile("test"),
                vec![Action::Manager(ManagerAction::Provision {
                    manager: "npm".to_string(),
                    via: "sys".to_string(),
                    declared: None,
                    batched: vec!["pipx".to_string()],
                    depends_on: Vec::new(),
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "tools".to_string(),
                    kind: ModuleActionKind::InstallPackages {
                        resolved: vec![declared("npm"), declared("jq")],
                    },
                    origin: None,
                })],
            ),
        ],
        warnings: vec![],
    };

    // Normal verbosity: the row under test is a `Role::Ok`, which `Quiet`
    // suppresses.
    let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(
        installs.lock().unwrap().as_slice(),
        [
            vec!["npm".to_string(), "pipx".to_string()],
            vec!["jq".to_string()]
        ],
        "the manager is asked only for the entry the provision did not land"
    );

    let rendered = crate::test_helpers::captured_text(&buf);
    // The alignment column pads between the two halves, so the assertion reads
    // the row rather than one substring of it.
    let row = rendered
        .lines()
        .find(|l| l.contains("sys install"))
        .unwrap_or_default();
    assert!(
        row.contains("✓ sys install npm, jq") && row.contains("— 1 provisioned by this run"),
        "the row names the planned set and attributes the entry this run's own \
         provision delivered to the run, never to the machine: {rendered}"
    );

    let packages = result
        .action_results
        .iter()
        .find(|r| r.description.starts_with("module:tools:packages:"))
        .expect("the module's package action settles a result");
    assert_eq!(
        packages.description, "module:tools:packages:npm,jq",
        "the recorded description keeps the planned set — it is the wire contract"
    );
    assert_eq!(
        packages.installed,
        Some(1),
        "`-o json` carries the landed count beside the planned set: {packages:?}"
    );
}

/// A provision that found its manager already there says so, and does not
/// claim a green tick for work it did not do.
///
/// The executor half of `provisioned_managers_summary`: the count is the
/// executor's own re-read, carried out on `ActionRun::installed` the way the
/// package arm's is, and a node whose members were all available already ran
/// nothing — the run's own `Prerequisites` phase, or an earlier node, may have
/// delivered one between the plan being priced and the node being dispatched.
#[test]
fn a_provision_whose_manager_was_already_delivered_states_the_count_that_says_so() {
    let provisioned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sys_installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("sys")
            .recording_installs(std::sync::Arc::clone(&sys_installs)),
    ));
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("tool")
            .without_index()
            .bootstrappable_via("sys")
            .mediated_by("sys", &["tool"])
            .available_when(std::sync::Arc::clone(&provisioned)),
    ));

    let widget = ResolvedPackage {
        canonical_name: "widget".to_string(),
        resolved_name: "widget".to_string(),
        manager: "tool".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let install = Action::Module(ModuleAction {
        module_name: "tools".to_string(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![widget],
        },
        origin: None,
    });
    let nodes = super::plan_managers(&registry, &[], &[(PhaseName::Packages, install)]);
    let node_id = nodes
        .iter()
        .find_map(|a| match a {
            Action::Manager(node @ ManagerAction::Provision { .. }) => Some(node.node_id()),
            _ => None,
        })
        .expect("the absent manager is provisioned");

    // Between the plan being priced and the node being dispatched, something
    // else put the manager on the machine.
    provisioned.store(true, std::sync::atomic::Ordering::SeqCst);

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            nodes,
        )],
        warnings: vec![],
    };
    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");

    assert!(
        sys_installs.lock().unwrap().is_empty(),
        "the manager was already there, so its installer never ran"
    );
    let settled = result
        .action_results
        .iter()
        .find(|r| r.description == node_id)
        .expect("the provision settles a result");
    assert_eq!(
        settled.installed,
        Some(0),
        "the re-read the row is worded from reaches the result: {settled:?}"
    );
    assert!(
        !settled.changed && settled.skipped,
        "a node that ran nothing changed nothing: {settled:?}"
    );
}

/// Every manager node that changes the machine states the fact it produced
/// for at least one thing the executor can observe, or is hatched here with
/// the reason its subject already is the whole fact.
///
/// A provision that landed hundreds of packages reported only its elapsed
/// time, one row below package installs that do say what they produced —
/// `action_produced_detail` had arms for env, files and packages alone. The
/// count is the executor's own re-read, carried out on `ActionRun::installed`
/// exactly as the package arm's is: a node promises an AVAILABLE manager, and
/// an earlier node or the `Prerequisites` phase may already have delivered one
/// of the managers it names. The count only fires on a shortfall, so every
/// single-manager node that lands its manager stated nothing; the VERSION the
/// landed binary reports (`ActionRun::versions`) is the fact the subject
/// cannot hold, and it fires whenever the manager answers one.
///
/// Every variant is bound with no `..`, so a new manager node is classified
/// here before this file compiles.
#[test]
fn every_manager_node_states_what_it_produced() {
    use super::types::{DeclaredProvision, ManagerAction};

    let landed = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(m, v)| ((*m).to_string(), (*v).to_string()))
            .collect()
    };
    let brew = Action::Manager(ManagerAction::Provision {
        manager: "brew".to_string(),
        via: "homebrew installer".to_string(),
        declared: None,
        batched: Vec::new(),
        depends_on: Vec::new(),
    });
    assert_eq!(
        super::action_produced_detail(&brew, Some(1), 0, &landed(&[("brew", "4.6.3")])).as_deref(),
        Some("4.6.3"),
        "a node naming one manager states the version it delivered, bare"
    );
    assert_eq!(
        super::action_produced_detail(&brew, Some(1), 0, &[]),
        None,
        "a manager that answers no version leaves the slot as it was"
    );

    let batch = ManagerAction::Provision {
        manager: "cargo".to_string(),
        via: "apt".to_string(),
        declared: None,
        batched: vec!["npm".to_string()],
        depends_on: Vec::new(),
    };
    let solo = ManagerAction::Provision {
        manager: "npm".to_string(),
        via: "apt".to_string(),
        declared: Some(DeclaredProvision {
            installer: "apt".to_string(),
            package: "npm".to_string(),
        }),
        batched: Vec::new(),
        depends_on: Vec::new(),
    };
    let batch = Action::Manager(batch);
    assert_eq!(
        super::action_produced_detail(&batch, Some(1), 0, &[]).as_deref(),
        Some("1 of 2 managers"),
        "a node that landed fewer managers than it named says how many"
    );
    assert_eq!(
        super::action_produced_detail(&batch, Some(2), 0, &[]),
        None,
        "a node that landed every manager it named would only restate its subject"
    );
    assert_eq!(
        super::action_produced_detail(&batch, None, 0, &[]),
        None,
        "a preview has not run, so it has no count of its own to state"
    );
    assert_eq!(
        super::action_produced_detail(
            &batch,
            Some(2),
            0,
            &landed(&[("cargo", "1.89.0"), ("npm", "11.4.2")])
        )
        .as_deref(),
        Some("cargo 1.89.0, npm 11.4.2"),
        "a batch names each version beside its manager"
    );
    assert_eq!(
        super::action_produced_detail(&batch, Some(1), 0, &landed(&[("npm", "11.4.2")])).as_deref(),
        Some("1 of 2 managers (npm 11.4.2)"),
        "a shortfall keeps its count and parenthesises what did land"
    );
    assert_eq!(
        super::action_produced_detail(&Action::Manager(solo), Some(0), 0, &[]).as_deref(),
        Some("0 of 1 manager"),
        "a node whose one manager was already there states the count that says so"
    );
    // The hatched variants, each with the reason its subject is the whole
    // fact: an index refresh names no artifact; a refusal produces nothing by
    // construction and its subject carries the reason; a prerequisite's
    // subject already spends the detail grammar on `required by`, and the
    // tool is a means the run needed rather than a product it delivered.
    for action in [
        ManagerAction::RefreshIndex {
            manager: "apt".to_string(),
        },
        ManagerAction::Prerequisite {
            tool: "curl".to_string(),
            installer: "apt".to_string(),
            required_by: vec!["brew".to_string()],
            depends_on: Vec::new(),
        },
        ManagerAction::Refuse {
            manager: "brew".to_string(),
            reason: "unsupported host".to_string(),
        },
    ] {
        let node = Action::Manager(action);
        assert_eq!(
            super::action_produced_detail(&node, Some(1), 0, &landed(&[("curl", "8.5.0")])),
            None,
            "a hatched node states nothing beyond its subject: {node:?}"
        );
    }
}

/// A produced detail never restates a total the subject already gives.
///
/// A subject names every operand it acts on, so the names ARE the total; a
/// detail that then says `6 files` states one number twice on one row
/// (`deploy a, b, c, d, e, f — 6 files`). Every COMPLEMENT arm of
/// `action_produced_detail` is rendered here over five operands, twice: with
/// the executor's re-read saying everything landed, and with it two short. A
/// detail carrying the operand total fails on either pass — a SHORTFALL
/// states the complement (`2 already installed`), never a ratio over a total
/// the row already spells out, because `— 7 of 9 packages` puts two numbers
/// over one set on one row.
///
/// `DeployFiles` states its total a different way — a bare count in the
/// subject (`deploy 5 files`), never the operand names — but the same rule
/// holds: the total is already on the row, so `deploy_files_summary` reads
/// only the action's own `files`/`declared_total` fields, never the
/// executor's re-read, and this fixture's full deploy (nothing left to state
/// a complement over) produces no detail on either pass.
///
/// The provision arm is deliberately not a complement arm: its shortfall is
/// a failure to deliver rather than a set already in place, so `1 of 2
/// managers` is exactly what it has to say. The env write names no operands
/// at all.
#[test]
fn no_produced_detail_restates_a_total_the_subject_already_gives() {
    use super::types::ManagerAction;

    let total = 5;
    let names: Vec<String> = (0..total).map(|i| format!("op{i}")).collect();
    let file = |target: &str| ResolvedFile {
        source: std::path::PathBuf::from("src"),
        target: std::path::PathBuf::from(target),
        is_git_source: false,
        strategy: None,
        encryption: None,
        permissions: None,
        patch: None,
    };
    let pkg = |name: &str| ResolvedPackage {
        canonical_name: name.to_string(),
        resolved_name: name.to_string(),
        manager: "brew".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let actions = [
        Action::Module(ModuleAction::local(
            "m",
            ModuleActionKind::DeployFiles {
                files: names.iter().map(|n| file(n)).collect(),
                declared_total: total,
            },
        )),
        Action::Module(ModuleAction::local(
            "m",
            ModuleActionKind::InstallPackages {
                resolved: names.iter().map(|n| pkg(n)).collect(),
            },
        )),
        Action::Package(PackageAction::Install {
            manager: "brew".to_string(),
            packages: names.clone(),
            origin: "local".to_string(),
        }),
        Action::Manager(ManagerAction::Provision {
            manager: names[0].clone(),
            via: "apt".to_string(),
            declared: None,
            batched: names[1..].to_vec(),
            depends_on: Vec::new(),
        }),
        Action::Env(super::types::EnvAction::WriteEnvFile {
            path: std::path::PathBuf::from("/home/u/.cfgd.env"),
            content: String::new(),
            vars: total,
            aliases: 0,
        }),
    ];
    let mut walked = 0;
    let total_word = regex::Regex::new(&format!(r"\b{total}\b")).unwrap();
    for action in &actions {
        let complement = matches!(
            action,
            Action::Module(ModuleAction {
                kind: ModuleActionKind::DeployFiles { .. }
                    | ModuleActionKind::InstallPackages { .. },
                ..
            }) | Action::Package(PackageAction::Install { .. })
        );
        if !complement {
            continue;
        }
        let subject = super::format::action_display_subject(action).to_string();
        let is_deploy = matches!(
            action,
            Action::Module(ModuleAction {
                kind: ModuleActionKind::DeployFiles { .. },
                ..
            })
        );
        if is_deploy {
            assert!(
                total_word.is_match(&subject),
                "a deploy's subject already states the total as a count: {subject}"
            );
        } else {
            assert!(
                names.iter().all(|name| subject.contains(name.as_str())),
                "the subject names every operand, which is what makes the total its own: {subject}"
            );
        }
        walked += 1;
        for landed in [total, total - 2] {
            let detail = super::action_produced_detail(action, Some(landed), 0, &[]);
            assert!(
                detail.as_deref().is_none_or(|d| !total_word.is_match(d)),
                "the subject already names all {total}; the detail says it again \
                 (landed {landed}): `{subject} — {}`",
                detail.unwrap_or_default()
            );
        }
    }
    assert_eq!(
        walked, 3,
        "every complement arm of `action_produced_detail` is walked"
    );
}

/// The executor reads the version off the manager it just verified, and only
/// for a manager THIS node landed: the row states `— <version>` and `-o json`
/// carries it under `versions`.
#[test]
fn a_landed_provision_states_the_version_it_delivered() {
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("brew")
            .unavailable()
            .bootstrappable_via("homebrew installer")
            .bootstrap_succeeds()
            .reporting_version("4.6.3"),
    ));
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("apt").reporting_version("2.8.3"),
    ));
    let state = test_state();
    let plan = prerequisites_phase(vec![
        provision_node("brew", "homebrew installer", &[]),
        provision_node("apt", "system", &[]),
    ]);

    let (result, rendered) =
        apply_manager_plan_at(&registry, &state, &plan, crate::output::Verbosity::Normal);

    assert_eq!(result.status, ApplyStatus::Success, "{rendered}");
    let row = rendered
        .lines()
        .find(|l| l.contains("provision brew via homebrew installer"))
        .unwrap_or_else(|| panic!("no brew row: {rendered}"));
    assert!(
        row.contains("— 4.6.3"),
        "the landed provision states the version it delivered: {row}"
    );
    let brew = result
        .action_results
        .iter()
        .find(|r| r.description == "manager:provision:brew")
        .expect("brew result");
    assert_eq!(brew.versions.get("brew").map(String::as_str), Some("4.6.3"));
    let apt = result
        .action_results
        .iter()
        .find(|r| r.description == "manager:provision:apt")
        .expect("apt result");
    assert!(
        apt.versions.is_empty(),
        "a manager that was here already produced nothing this row can claim: {apt:?}"
    );
    let apt_row = rendered
        .lines()
        .find(|l| l.contains("provision apt via system"))
        .unwrap_or_else(|| panic!("no apt row: {rendered}"));
    assert!(
        !apt_row.contains("2.8.3"),
        "an already-present manager's version is not this run's product: {apt_row}"
    );
}

/// A run that installs everything it named states no count: the subject
/// already lists every entry, so a trailing `— 2 packages` could only restate
/// the row. The deploy arm's own rule, at the threshold a package subject sits
/// permanently below — it never elides.
#[test]
fn an_install_that_landed_everything_it_named_states_no_count() {
    let action = Action::Module(ModuleAction {
        module_name: "tools".to_string(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![
                ResolvedPackage {
                    canonical_name: "jq".to_string(),
                    resolved_name: "jq".to_string(),
                    manager: "brew".to_string(),
                    manager_declared: false,
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
                    min_version: None,
                },
                ResolvedPackage {
                    canonical_name: "fd".to_string(),
                    resolved_name: "fd".to_string(),
                    manager: "brew".to_string(),
                    manager_declared: false,
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
                    min_version: None,
                },
            ],
        },
        origin: None,
    });
    assert_eq!(
        super::action_produced_detail(&action, Some(2), 0, &[]),
        None
    );
    assert_eq!(
        super::action_produced_detail(&action, Some(1), 0, &[]).as_deref(),
        Some("1 already installed")
    );
    // A preview has not run, so it has no count of its own to state.
    assert_eq!(super::action_produced_detail(&action, None, 0, &[]), None);
}

/// A tool the module declares as a PACKAGE is provisioned by the module's own
/// route, not by cfgd's default cascade.
///
/// The coherence half of the route predicate: with no declaration there is no
/// route, so the manager's own cascade provisions the tool — and the module's
/// entry for that same tool, sitting under whatever manager cfgd defaulted it
/// to, must not then install a SECOND copy through that manager.
///
/// `package_survives_elision` cannot catch it: it asks the entry's OWN manager
/// what it holds, and `alt`'s listing does not know about the `tool` that the
/// cascade just landed. Two copies of one toolchain with `PATH` order deciding
/// is exactly what the route feature exists to prevent, so the elision keys on
/// the tool the provision DELIVERED rather than on who delivered it.
#[test]
fn a_tool_this_run_provisioned_is_not_installed_again_by_a_module_entry() {
    let alt_installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let tool_installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("alt")
            .recording_installs(std::sync::Arc::clone(&alt_installs)),
    ));
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "sys",
    )));
    // Absent, and its OWN cascade is what puts it on the machine.
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("tool")
            .without_index()
            .unavailable()
            .bootstrappable_via("sys")
            .bootstrap_succeeds()
            .recording_installs(std::sync::Arc::clone(&tool_installs)),
    ));

    let pkg = |canonical: &str, manager: &str| ResolvedPackage {
        canonical_name: canonical.to_string(),
        resolved_name: canonical.to_string(),
        manager: manager.to_string(),
        // A bare `- name: tool`: no `prefer`, no `aliases`, so `alt` is cfgd's
        // own platform default rather than anything the module said.
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let defaulted_tool = pkg("tool", "alt");
    let widget = pkg("widget", "tool");

    let module_action = |packages: Vec<ResolvedPackage>| {
        Action::Module(ModuleAction {
            module_name: "tools".to_string(),
            kind: ModuleActionKind::InstallPackages { resolved: packages },
            origin: None,
        })
    };
    let routed = vec![
        (
            PhaseName::Packages,
            module_action(vec![defaulted_tool.clone()]),
        ),
        (PhaseName::Packages, module_action(vec![widget.clone()])),
    ];
    let nodes = super::plan_managers(&registry, &[], &routed);
    let provision = nodes
        .iter()
        .find_map(|a| match a {
            Action::Manager(ManagerAction::Provision {
                manager,
                via,
                declared,
                ..
            }) if manager == "tool" => Some((via.clone(), declared.clone())),
            _ => None,
        })
        .expect("the absent manager is provisioned");
    assert_eq!(
        provision,
        ("sys".to_string(), None),
        "a defaulted manager mints no route, so the cascade provisions the tool"
    );

    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![defaulted_tool.clone(), widget.clone()],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];
    let plan = Plan {
        phases: vec![
            Phase::from_actions(PhaseName::Prerequisites, &Owner::profile("test"), nodes),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![
                    module_action(vec![defaulted_tool]),
                    module_action(vec![widget]),
                ],
            ),
        ],
        warnings: vec![],
    };

    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");

    assert_eq!(
        result.status,
        ApplyStatus::Success,
        "{:?}",
        result
            .action_results
            .iter()
            .map(|r| (r.description.clone(), r.error.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        alt_installs.lock().unwrap().is_empty(),
        "the module entry cfgd defaulted onto `alt` must not land a second copy: {:?}",
        alt_installs.lock().unwrap()
    );
    assert_eq!(
        tool_installs.lock().unwrap().as_slice(),
        [vec!["widget".to_string()]],
        "the provisioned manager still installs what needed it"
    );
}

/// The same shape, one step earlier: the PLAN. The hero recording showed
/// `√ provision npm via brew` in `Prerequisites` beside `apt install …, npm,
/// …` in `Packages` — the apply's execute-time elision dropped the apt copy
/// (`11 of 12 packages`), but the plan had promised it, priced it, and counted
/// it. A bare entry naming a tool this plan's own cascade provisions is
/// elided from the plan itself, by the same predicate, and `Actions planned`
/// no longer counts an install the run will never perform.
#[test]
fn a_tool_this_plan_provisions_is_not_planned_again_by_a_module_entry() {
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "alt",
    )));
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "sys",
    )));
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("tool")
            .without_index()
            .unavailable()
            .bootstrappable_via("sys"),
    ));
    let pkg = |canonical: &str, manager: &str| ResolvedPackage {
        canonical_name: canonical.to_string(),
        resolved_name: canonical.to_string(),
        manager: manager.to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let mut module = make_resolved_module("tools");
    module.packages = vec![
        pkg("tool", "alt"),
        pkg("other", "alt"),
        pkg("widget", "tool"),
    ];

    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let plan = reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("provision tool via sys"),
        "the absent manager is provisioned by its cascade, got:\n{items}"
    );
    assert!(
        items.contains("alt install other") && !items.contains("tool, other"),
        "the entry naming the provisioned tool is elided from the plan, its sibling kept, got:\n{items}"
    );
    assert!(
        items.contains("tool install widget"),
        "the provisioned manager still installs what needed it, got:\n{items}"
    );
}

/// The same elision over the two entries a provision delivers under a name of
/// its own, which `manager_declared` bars from the arm above.
///
/// The hero recording applied a module declaring `node` (`prefer: [brew]`) and
/// `pipx` (`prefer: [brew]`) while cfgd needed npm and pipx as MANAGERS. The
/// `Prerequisites` phase ran `provision npm via brew` (a `brew install node`)
/// and `provision pipx via brew` through the module's own route, and the
/// `Packages` row underneath still read `brew install neovim, fd, zoxide,
/// node, pipx, go, stylua, sops, age — 2 provisioned by this run`: two tools
/// named on two rows, one of them an install the run never performed.
///
/// A provision delivers a PACKAGE, and that package's name is the entry's, not
/// the manager's — the module's route installs `tool-alias` through `alt`, and
/// `dep`'s cascade installs `dep-pkg` through the same `alt`. Both are elided
/// by the pair the apply's settle records, so the row names neither.
///
/// A cascade delivers the MANAGER's own literal, which an entry writing
/// `aliases: {alt: alias-spelling}` never spells — so the elision asks the
/// canonical name the alias resolves to as well, and `aliased-pkg` goes the
/// same way as its unaliased sibling. The
/// strings asserted here are what `-o json` carries: the plan payload's
/// `description` IS `format_plan_item`.
#[test]
fn a_declared_route_entry_this_plan_provisions_is_not_planned_again_on_the_packages_row() {
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "alt",
    )));
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "sys",
    )));
    // Absent, and the module's own entry routes its provision through `alt`
    // under the name that installer knows it by.
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("tool")
            .without_index()
            .unavailable()
            .bootstrappable_via("sys"),
    ));
    // Absent, and its own cascade installs it through `alt` as `dep-pkg` —
    // the npm/node shape, where the package the provision lands carries no
    // route of its own.
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("dep")
            .without_index()
            .unavailable()
            .bootstrappable_via("alt")
            .mediated_by("alt", &["dep-pkg", "aliased-pkg"]),
    ));

    let pkg = |canonical: &str, resolved: &str, manager: &str, declared: bool| ResolvedPackage {
        canonical_name: canonical.to_string(),
        resolved_name: resolved.to_string(),
        manager: manager.to_string(),
        manager_declared: declared,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let mut module = make_resolved_module("tools");
    module.packages = vec![
        pkg("tool", "tool-alias", "alt", true),
        pkg("dep-pkg", "dep-pkg", "alt", true),
        pkg("aliased-pkg", "alias-spelling", "alt", true),
        pkg("keep", "keep", "alt", true),
        pkg("widget", "widget", "tool", false),
        pkg("gadget", "gadget", "dep", false),
    ];

    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let plan = reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("provision tool via alt (tool-alias)"),
        "the module's route still provisions the tool it named, got:\n{items}"
    );
    assert!(
        items.contains("provision dep via alt"),
        "the cascade still provisions the manager it delivers, got:\n{items}"
    );
    assert_eq!(
        items.matches("tool-alias").count(),
        1,
        "the package the route installs is named by the provision row alone, got:\n{items}"
    );
    assert_eq!(
        items.matches("dep-pkg").count(),
        0,
        "the package the cascade installs is named by no packages row, got:\n{items}"
    );
    assert_eq!(
        items.matches("alias-spelling").count(),
        0,
        "and so is the entry that spells that package with an `aliases:` name, got:\n{items}"
    );
    assert!(
        items.contains("alt install keep"),
        "an entry no provision delivers is kept, got:\n{items}"
    );
    assert!(
        items.contains("tool install widget") && items.contains("dep install gadget"),
        "each provisioned manager still installs what needed it, got:\n{items}"
    );
}

/// The elision is judged against the FIRST pass's nodes, and the second pass
/// must still ship the route that justified it.
///
/// `module_routed` feeds only `wanted_managers`, and a cascade's arm is priced
/// off the managers the graph has already settled — so eliding a mediator's
/// last consuming entry retires the mediator on the second pass, the cascade
/// falls to its host arm, and the package dropped because the pass-1 route
/// would have delivered it is then delivered by nothing: `paket` here was
/// declared by the module and named NOWHERE in the shipped plan. The milder
/// form of the same mechanism re-routes a surviving `via` (`provision npm via
/// apt` in place of the brew arm the elision was justified by), which is the
/// 534-package regression `bootstrap_plan_given`'s `delivered` predicate
/// exists to prevent.
///
/// So the installers of the pairs the elision acted on stay WANTED across the
/// re-plan. A manager whose consumers genuinely vanished still retires: the
/// seed names only the mediators a provision of this very plan installs
/// through.
#[test]
fn a_cascade_the_elision_relied_on_survives_the_re_plan() {
    let registry_of = || {
        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
            "sys",
        )));
        // The mediator: absent, and itself bootstrappable from the system manager.
        registry.add_package_manager(Box::new(
            crate::test_helpers::MockPackageManager::new("alt")
                .without_index()
                .unavailable()
                .bootstrappable_via("sys"),
        ));
        // Prefers the mediator whenever the run delivers one, and delivers
        // `paket` when it takes that arm — the module's only entry under `alt`.
        registry.add_package_manager(Box::new(
            crate::test_helpers::MockPackageManager::new("dep")
                .without_index()
                .unavailable()
                .preferring_delivered(&["alt"])
                .bootstrappable_via("sys")
                .mediated_by("alt", &["paket"]),
        ));
        registry
    };
    let registry = registry_of();

    let pkg = |canonical: &str, manager: &str, declared: bool| ResolvedPackage {
        canonical_name: canonical.to_string(),
        resolved_name: canonical.to_string(),
        manager: manager.to_string(),
        manager_declared: declared,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let mut module = make_resolved_module("tools");
    module.packages = vec![pkg("paket", "alt", true), pkg("gadget", "dep", false)];

    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let plan = reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("provision alt via sys"),
        "the mediator the elision relied on is still provisioned, got:\n{items}"
    );
    assert!(
        items.contains("provision dep via alt"),
        "and the cascade still takes the arm that delivers the elided package, got:\n{items}"
    );
    assert_eq!(
        items.matches("paket").count(),
        0,
        "the delivered package is named by no packages row, got:\n{items}"
    );
    assert!(
        items.contains("dep install gadget"),
        "the provisioned manager still installs what needed it, got:\n{items}"
    );

    // The same shape one step out: the entry that kept the DELIVERING manager
    // wanted is itself elided (a bare entry naming the mediator `alt`, which
    // the provisioned arm drops), so pass 2 retires `provision dep via alt`
    // unless the elision seeds that node's own managers too — and `paket`,
    // dropped because the pass-1 route delivered it, is delivered by nothing
    // while `alt` is provisioned for no consumer at all.
    let registry = registry_of();
    let mut module = make_resolved_module("tools");
    module.packages = vec![pkg("paket", "alt", true), pkg("alt", "dep", false)];
    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let plan = reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("provision alt via sys") && items.contains("provision dep via alt"),
        "the node that DELIVERED the elided package stays wanted across the re-plan, got:\n{items}"
    );
    assert_eq!(
        items.matches("paket").count(),
        0,
        "and the package it delivers is still named by no packages row, got:\n{items}"
    );
}

/// The hero recording's second shape: `Prerequisites` ran `provision cargo via
/// rustup` and `provision npm, pipx via apt` while the module declared `pipx`
/// with `prefer: [brew, apt]` and `cargo` with `aliases: {brew: rust, apt:
/// rustc}`. cfgd needed those MANAGERS to satisfy other entries and bootstrapped
/// them by its own default route without ever reading the module's entry for
/// the same tool, so the machine ended with two pipx and two cargo toolchains
/// and `PATH` order decided which one every later command meant.
/// `package_survives_elision` cannot catch it: it is asked against ONE
/// manager's listing, and brew's listing does not know about apt's pipx.
///
/// The module's entry is the more specific statement, so the provision resolves
/// through its `prefer`/`aliases` chain and the `Packages` phase then elides the
/// entry through the predicate it already has. Here `tool` is declared under
/// `alt` as `tool-alias` while its own cascade would install it via `sys`.
#[test]
fn a_tool_a_module_declares_is_provisioned_by_the_modules_own_route() {
    let alt_installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sys_installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let tool_installs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provisioned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("alt")
            .recording_installs(std::sync::Arc::clone(&alt_installs))
            .raising(std::sync::Arc::clone(&provisioned)),
    ));
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("sys")
            .recording_installs(std::sync::Arc::clone(&sys_installs)),
    ));
    // Absent, and its own cascade installs it through `sys` — the default route
    // this fixture proves is not taken.
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("tool")
            .without_index()
            .bootstrappable_via("sys")
            .mediated_by("sys", &["tool"])
            .available_when(std::sync::Arc::clone(&provisioned))
            .recording_installs(std::sync::Arc::clone(&tool_installs)),
    ));

    let pkg = |canonical: &str, resolved: &str, manager: &str, declared: bool| ResolvedPackage {
        canonical_name: canonical.to_string(),
        resolved_name: resolved.to_string(),
        manager: manager.to_string(),
        manager_declared: declared,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    // `widget` is why cfgd needs `tool` as a MANAGER at all; the `tool` entry
    // beside it is the module's statement about where that tool comes from —
    // `prefer: [alt]` with `aliases: {alt: tool-alias}`, which is what
    // `manager_declared` records and what makes this entry a route at all.
    let declared_tool = pkg("tool", "tool-alias", "alt", true);
    let widget = pkg("widget", "widget", "tool", false);

    let module_action = |packages: Vec<ResolvedPackage>| {
        Action::Module(ModuleAction {
            module_name: "tools".to_string(),
            kind: ModuleActionKind::InstallPackages { resolved: packages },
            origin: None,
        })
    };
    let routed = vec![
        (
            PhaseName::Packages,
            module_action(vec![declared_tool.clone()]),
        ),
        (PhaseName::Packages, module_action(vec![widget.clone()])),
    ];

    let nodes = super::plan_managers(&registry, &[], &routed);
    let provision = nodes
        .iter()
        .find_map(|a| match a {
            Action::Manager(node @ ManagerAction::Provision { manager, .. })
                if manager == "tool" =>
            {
                Some(node)
            }
            _ => None,
        })
        .expect("the absent manager is provisioned");
    let ManagerAction::Provision { via, declared, .. } = provision else {
        panic!("expected a provision node");
    };
    assert_eq!(
        via, "alt",
        "the module's `prefer` chain picks the installer"
    );
    assert_eq!(
        declared
            .as_ref()
            .map(|d| (d.installer.as_str(), d.package.as_str())),
        Some(("alt", "tool-alias")),
        "and its `aliases` map picks the name that installer knows it by"
    );

    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![declared_tool.clone(), widget.clone()],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];
    let plan = Plan {
        phases: vec![
            Phase::from_actions(PhaseName::Prerequisites, &Owner::profile("test"), nodes),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![
                    module_action(vec![declared_tool]),
                    module_action(vec![widget]),
                ],
            ),
        ],
        warnings: vec![],
    };

    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(
        alt_installs.lock().unwrap().as_slice(),
        [vec!["tool-alias".to_string()]],
        "the tool is installed exactly once, by the manager the module named"
    );
    assert!(
        sys_installs.lock().unwrap().is_empty(),
        "the default cascade never runs: two installers is two toolchains"
    );
    assert_eq!(
        tool_installs.lock().unwrap().as_slice(),
        [vec!["widget".to_string()]],
        "the provisioned manager still installs what needed it"
    );
    let declared_entry = result
        .action_results
        .iter()
        .find(|r| r.description == "module:tools:packages:tool-alias")
        .expect("the declared entry settles a result");
    assert!(
        !declared_entry.changed && declared_entry.skipped,
        "the `Packages` phase elides the entry the provision already landed: {declared_entry:?}"
    );
}

// --- rollback_apply: symlink restore (restore to state after target apply) ---

#[test]
#[cfg(unix)]
fn rollback_restores_symlink_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("link-file");
    let link_dest = dir.path().join("original-dest.txt");
    let file_path = target.display().to_string();
    std::fs::write(&link_dest, "link content").unwrap();

    let state = test_state();

    // Apply 1: creates the symlink
    let apply_id_1 = state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    std::os::unix::fs::symlink(&link_dest, &target).unwrap();
    assert!(target.is_symlink());
    let resource_id = format!("file:create:{}", target.display());
    let jid1 = state
        .journal_begin(apply_id_1, 0, "files", "file", &resource_id, None)
        .unwrap();
    state.journal_complete(jid1, 0, None, None).unwrap();

    // Apply 2: replaces symlink with a regular file. Backup captures symlink state.
    let file_state = crate::capture_file_state(&target).unwrap().unwrap();
    assert!(file_state.is_symlink);
    let apply_id_2 = state
        .record_apply("test", "hash2", ApplyStatus::Success, None)
        .unwrap();
    state
        .store_file_backup(apply_id_2, &file_path, &file_state)
        .unwrap();
    let update_resource_id = format!("file:update:{}", target.display());
    let jid2 = state
        .journal_begin(apply_id_2, 0, "files", "file", &update_resource_id, None)
        .unwrap();
    state.journal_complete(jid2, 0, None, None).unwrap();

    // Replace the symlink with a regular file (simulating apply 2)
    std::fs::remove_file(&target).unwrap();
    std::fs::write(&target, "replaced").unwrap();
    assert!(!target.is_symlink());

    // Rollback to apply 1 — should restore the symlink
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    let rollback_result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    assert_eq!(rollback_result.files_restored, 1);
    assert!(target.is_symlink(), "symlink should be restored");
    assert_eq!(
        std::fs::read_link(&target).unwrap(),
        link_dest,
        "symlink should point to original destination"
    );
}

// --- plan_modules: encryption validation ---

#[test]
fn plan_modules_encryption_always_with_symlink_skips() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Symlink;
    let reconciler = Reconciler::new(&registry, &state);

    let modules = vec![ResolvedModule {
        name: "secrets-mod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: PathBuf::from("/nonexistent/secret.enc"),
            target: PathBuf::from("/home/user/.secret"),
            is_git_source: false,
            strategy: None, // defaults to Symlink
            encryption: Some(crate::config::EncryptionSpec {
                backend: "sops".to_string(),
                mode: crate::config::EncryptionMode::Always,
            }),
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let actions = reconciler
        .plan_modules(&modules, "test", ReconcileContext::Apply)
        .0;
    // Should produce a Skip action because encryption=Always + symlink is incompatible
    assert_eq!(actions.len(), 1);
    match &actions[0].1 {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::Skip { reason } => {
                assert!(
                    reason.contains("encryption mode Always incompatible"),
                    "got: {reason}"
                );
            }
            other => panic!("Expected Skip, got {:?}", other),
        },
        other => panic!("Expected Module action, got {:?}", other),
    }
}

#[test]
fn plan_modules_platform_skipped_emits_single_skip_and_no_other_actions() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    // A platform-gated module carries a skip reason plus (defensively) packages
    // and scripts. plan_modules must emit exactly one Skip and nothing else.
    let modules = vec![ResolvedModule {
        name: "macstuff".to_string(),
        packages: vec![crate::modules::ResolvedPackage {
            canonical_name: "rectangle".to_string(),
            resolved_name: "rectangle".to_string(),
            manager: "brew".to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![crate::config::ScriptEntry::Simple("echo post".to_string())],
        pre_apply_scripts: vec![crate::config::ScriptEntry::Simple("echo pre".to_string())],
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: Some("platform not matched (requires: macos)".to_string()),
    }];

    let actions = reconciler
        .plan_modules(&modules, "test", ReconcileContext::Apply)
        .0;
    assert_eq!(actions.len(), 1, "expected exactly one action: {actions:?}");
    match &actions[0].1 {
        Action::Module(ma) => {
            assert_eq!(ma.module_name, "macstuff");
            match &ma.kind {
                ModuleActionKind::Skip { reason } => {
                    assert!(reason.contains("macos"), "got: {reason}");
                }
                other => panic!("Expected Skip, got {other:?}"),
            }
        }
        other => panic!("Expected Module action, got {other:?}"),
    }
}

#[test]
fn plan_modules_encryption_always_with_copy_proceeds() {
    let dir = tempfile::tempdir().unwrap();
    // Create a fake SOPS-encrypted file with required `mac` and `lastmodified` keys
    let source = dir.path().join("secret.enc");
    std::fs::write(
            &source,
            "{\"sops\":{\"mac\":\"abc123\",\"lastmodified\":\"2024-01-01T00:00:00Z\",\"version\":\"3.0\"}, \"data\": \"ENC[AES256_GCM,data:abc]\"}",
        )
        .unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);

    let modules = vec![ResolvedModule {
        name: "secrets-mod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source.clone(),
            target: PathBuf::from("/home/user/.secret"),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: Some(crate::config::EncryptionSpec {
                backend: "sops".to_string(),
                mode: crate::config::EncryptionMode::Always,
            }),
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let actions = reconciler
        .plan_modules(&modules, "test", ReconcileContext::Apply)
        .0;
    // Should produce DeployFiles (encryption=Always + copy is OK, and file has sops marker)
    assert_eq!(actions.len(), 1);
    match &actions[0].1 {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::DeployFiles { files, .. } => {
                assert_eq!(files.len(), 1);
            }
            other => panic!("Expected DeployFiles, got {:?}", other),
        },
        other => panic!("Expected Module action, got {:?}", other),
    }
}

#[test]
fn plan_modules_encryption_check_err_skips_with_error_reason() {
    // is_file_encrypted returns Err for unknown backends (gpg, pgp, etc.) —
    // the planner records a Skip with the wrapped error reason rather than
    // crashing. Covers the planner's skip-on-error arm.
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("data.bin");
    std::fs::write(&source, "anything").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);

    let modules = vec![ResolvedModule {
        name: "gpg-mod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source.clone(),
            target: PathBuf::from("/home/user/.gpg-secret"),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: Some(crate::config::EncryptionSpec {
                backend: "gpg".to_string(),
                mode: crate::config::EncryptionMode::Always,
            }),
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let actions = reconciler
        .plan_modules(&modules, "test", ReconcileContext::Apply)
        .0;
    assert_eq!(actions.len(), 1);
    match &actions[0].1 {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::Skip { reason } => {
                assert!(reason.contains("encryption check failed"), "got: {reason}");
            }
            other => panic!("Expected Skip, got {:?}", other),
        },
        other => panic!("Expected Module action, got {:?}", other),
    }
}

#[test]
fn plan_modules_encryption_check_err_breaks_after_first_file() {
    // When the first encrypted file's backend check errors, planner records
    // a single Skip and short-circuits the rest of the module's files.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    std::fs::write(&a, "anything").unwrap();
    std::fs::write(&b, "anything").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);

    let modules = vec![ResolvedModule {
        name: "multi".to_string(),
        packages: vec![],
        files: vec![
            ResolvedFile {
                source: a.clone(),
                target: PathBuf::from("/home/user/.a"),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: Some(crate::config::EncryptionSpec {
                    backend: "unsupported".to_string(),
                    mode: crate::config::EncryptionMode::Always,
                }),
                permissions: None,
                patch: None,
            },
            ResolvedFile {
                source: b.clone(),
                target: PathBuf::from("/home/user/.b"),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: None,
                permissions: None,
                patch: None,
            },
        ],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let actions = reconciler
        .plan_modules(&modules, "test", ReconcileContext::Apply)
        .0;
    // After the failing encryption check, planner does NOT emit DeployFiles —
    // single Skip is the only module action emitted.
    let kinds: Vec<&ModuleActionKind> = actions
        .iter()
        .filter_map(|(_, a)| match a {
            Action::Module(ma) => Some(&ma.kind),
            _ => None,
        })
        .collect();
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, ModuleActionKind::Skip { reason } if reason.contains("encryption check failed"))),
        "must emit Skip with check-failed reason"
    );
    assert!(
        !kinds
            .iter()
            .any(|k| matches!(k, ModuleActionKind::DeployFiles { .. })),
        "must NOT emit DeployFiles when encryption check errored"
    );
}

#[test]
fn plan_modules_encryption_file_not_encrypted_skips() {
    let dir = tempfile::tempdir().unwrap();
    // Create a plaintext file (not encrypted)
    let source = dir.path().join("plain.txt");
    std::fs::write(&source, "plain text content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);

    let modules = vec![ResolvedModule {
        name: "secrets-mod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source.clone(),
            target: PathBuf::from("/home/user/.secret"),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: Some(crate::config::EncryptionSpec {
                backend: "sops".to_string(),
                mode: crate::config::EncryptionMode::Always,
            }),
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let actions = reconciler
        .plan_modules(&modules, "test", ReconcileContext::Apply)
        .0;
    // Should skip because file requires encryption but isn't encrypted
    assert_eq!(actions.len(), 1);
    match &actions[0].1 {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::Skip { reason } => {
                assert!(
                    reason.contains("requires encryption") && reason.contains("not encrypted"),
                    "got: {reason}"
                );
            }
            other => panic!("Expected Skip, got {:?}", other),
        },
        other => panic!("Expected Module action, got {:?}", other),
    }
}

// --- apply_script_action via apply() ---

#[test]
#[cfg(unix)]
fn apply_script_action_executes_and_records_output() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("script-ran");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();

    // Post-apply script so it doesn't abort on failure
    resolved.merged.scripts.post_apply =
        vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // The script phase should have run
    let script_result = result
        .action_results
        .iter()
        .find(|r| r.description.contains("script:"));
    assert!(script_result.is_some(), "script action should be recorded");
    assert!(script_result.unwrap().success);
    assert!(marker.exists(), "script should have run and created marker");
}

// --- apply_module_action: RunScript ---

#[test]
#[cfg(unix)]
fn apply_module_run_script_executes_in_module_dir() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("module-script-ran");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "testmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        pre_apply_scripts: Vec::new(),
        post_apply_scripts: vec![ScriptEntry::Simple(format!("touch {}", marker.display()))],
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::PostScripts,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "testmod".to_string(),
                kind: ModuleActionKind::RunScript {
                    script: ScriptEntry::Simple(format!("touch {}", marker.display())),
                    phase: ScriptPhase::PostApply,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::PostScripts)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(marker.exists(), "module script should have created marker");
    assert!(
        result.action_results[0]
            .description
            .contains("module:testmod:script"),
        "desc: {}",
        result.action_results[0].description
    );
}

// --- plan_env: Fish and PowerShell content generation ---

#[test]
fn generate_fish_env_content_basic() {
    let env = vec![
        crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
            platforms: vec![],
        },
        crate::config::EnvVar {
            name: "CARGO_HOME".into(),
            value: "/home/user/.cargo".into(),
            platforms: vec![],
        },
    ];
    let aliases = vec![crate::config::ShellAlias {
        name: "g".into(),
        command: "git".into(),
        platforms: vec![],
    }];
    let content = super::generate_fish_env_content(&env, &aliases, None, &Default::default());
    assert!(content.starts_with("# managed by cfgd"));
    assert!(content.contains("set -gx EDITOR 'nvim'"));
    assert!(content.contains("set -gx CARGO_HOME '/home/user/.cargo'"));
    assert!(content.contains("abbr -a g 'git'"));
}

#[test]
fn generate_powershell_env_content_with_env_ref() {
    let env = vec![crate::config::EnvVar {
        name: "MY_PATH".into(),
        value: r"C:\tools;$env:PATH".into(),
        platforms: vec![],
    }];
    let content = super::generate_powershell_env_content(&env, &[], None, &Default::default());
    // Contains $env: so should be double-quoted
    assert!(
        content.contains(r#"$env:MY_PATH = "C:\tools;$env:PATH""#),
        "content: {}",
        content
    );
}

#[test]
fn generate_powershell_env_function_alias() {
    // When an alias command contains a space, PowerShell generates a function instead of Set-Alias
    let aliases = vec![crate::config::ShellAlias {
        name: "ll".into(),
        command: "Get-ChildItem -Force".into(),
        platforms: vec![],
    }];
    let content = super::generate_powershell_env_content(&[], &aliases, None, &Default::default());
    assert!(content.contains("function ll {"));
    assert!(content.contains("Get-ChildItem -Force @args"));
}

#[test]
fn generate_fish_env_path_splitting() {
    // Fish should split PATH values on :
    let env = vec![crate::config::EnvVar {
        name: "PATH".into(),
        value: "/usr/bin:/usr/local/bin:$PATH".into(),
        platforms: vec![],
    }];
    let content = super::generate_fish_env_content(
        &env,
        &[],
        fish_path_fold(&env).as_ref(),
        &Default::default(),
    );
    assert!(
        content.contains("set -gx PATH '/usr/bin' '/usr/local/bin' $PATH"),
        "content: {}",
        content
    );
}

// --- fish_in_use Unix-branch helper ---

#[test]
fn shell_var_indicates_fish_matches_explicit_paths() {
    use super::env_files::shell_var_indicates_fish;
    assert!(shell_var_indicates_fish(Some("/usr/bin/fish")));
    assert!(shell_var_indicates_fish(Some("/opt/homebrew/bin/fish")));
    assert!(shell_var_indicates_fish(Some("fish")));
}

#[test]
fn shell_var_indicates_fish_rejects_other_shells() {
    use super::env_files::shell_var_indicates_fish;
    assert!(!shell_var_indicates_fish(Some("/bin/bash")));
    assert!(!shell_var_indicates_fish(Some("/bin/zsh")));
    assert!(!shell_var_indicates_fish(Some("/usr/bin/sh")));
}

#[test]
fn shell_var_indicates_fish_handles_missing_or_empty() {
    use super::env_files::shell_var_indicates_fish;
    // SHELL unset → unwrap_or("") path, no match.
    assert!(!shell_var_indicates_fish(None));
    // SHELL set to empty string (some sandboxes do this).
    assert!(!shell_var_indicates_fish(Some("")));
}

#[test]
fn shell_var_indicates_fish_matches_substring_anywhere() {
    use super::env_files::shell_var_indicates_fish;
    // The implementation uses `.contains("fish")` rather than basename match —
    // pin that contract so a future regex-tightening refactor doesn't silently
    // break Cygwin/MSYS users whose `$SHELL` may include parent dirs.
    assert!(shell_var_indicates_fish(Some(
        "/cygdrive/c/Program Files/fish/bin/fish.exe"
    )));
    assert!(shell_var_indicates_fish(Some("/opt/fish-shell/fish")));
}

// --- build_script_env additional tests ---

#[test]
fn build_script_env_all_phases() {
    // Verify that each ScriptPhase variant produces the correct CFGD_PHASE value
    let phases_and_expected = [
        (ScriptPhase::PreApply, "preApply"),
        (ScriptPhase::PostApply, "postApply"),
        (ScriptPhase::PreReconcile, "preReconcile"),
        (ScriptPhase::PostReconcile, "postReconcile"),
        (ScriptPhase::OnDrift, "onDrift"),
        (ScriptPhase::OnChange, "onChange"),
    ];

    for (phase, expected_name) in &phases_and_expected {
        let env = super::build_script_env(&ScriptEnvContext {
            config_dir: std::path::Path::new("/etc/cfgd"),
            profile_name: "default",
            context: ReconcileContext::Apply,
            phase,
            module_name: None,
            module_dir: None,
            path_dirs: &[],
        });
        let map: HashMap<String, String> = env.into_iter().collect();
        assert_eq!(
            map.get("CFGD_PHASE").unwrap(),
            expected_name,
            "phase {:?} should produce CFGD_PHASE={}",
            phase,
            expected_name
        );
    }
}

#[test]
fn build_script_env_does_not_emit_dry_run() {
    // The CFGD_DRY_RUN env var was removed: it was hardcoded to "false"
    // because the CLI gates dry-run above `Reconciler::apply`, so
    // `execute_script` never ran in dry-run mode. Re-introduce the
    // variable only as part of a full wire-through that threads a real
    // `dry_run` down `Reconciler::apply`. This test guards against
    // accidental re-introduction of the un-wired variable.
    let env = super::build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/cfg"),
        profile_name: "laptop",
        context: ReconcileContext::Apply,
        phase: &ScriptPhase::PreApply,
        module_name: None,
        module_dir: None,
        path_dirs: &[],
    });
    let map: HashMap<String, String> = env.into_iter().collect();
    assert!(!map.contains_key("CFGD_DRY_RUN"));
}

#[test]
fn build_script_env_reconcile_context() {
    let env = super::build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/cfg"),
        profile_name: "server",
        context: ReconcileContext::Reconcile,
        phase: &ScriptPhase::PostReconcile,
        module_name: None,
        module_dir: None,
        path_dirs: &[],
    });
    let map: HashMap<String, String> = env.into_iter().collect();
    assert_eq!(map.get("CFGD_CONTEXT").unwrap(), "reconcile");
    assert_eq!(map.get("CFGD_PHASE").unwrap(), "postReconcile");
    assert_eq!(map.get("CFGD_PROFILE").unwrap(), "server");
}

#[test]
fn build_script_env_module_name_without_dir() {
    // module_name provided but module_dir is None
    let env = super::build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/cfg"),
        profile_name: "default",
        context: ReconcileContext::Apply,
        phase: &ScriptPhase::PreApply,
        module_name: Some("zsh"),
        module_dir: None,
        path_dirs: &[],
    });
    let map: HashMap<String, String> = env.into_iter().collect();
    assert_eq!(map.get("CFGD_MODULE_NAME").unwrap(), "zsh");
    assert!(
        !map.contains_key("CFGD_MODULE_DIR"),
        "CFGD_MODULE_DIR should not be set when module_dir is None"
    );
}

#[test]
fn build_script_env_count_base_vars() {
    // Without module info, should have exactly 4 base vars
    // (CFGD_CONFIG_DIR, CFGD_PROFILE, CFGD_CONTEXT, CFGD_PHASE)
    let env = super::build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/x"),
        profile_name: "p",
        context: ReconcileContext::Apply,
        phase: &ScriptPhase::PreApply,
        module_name: None,
        module_dir: None,
        path_dirs: &[],
    });
    assert_eq!(env.len(), 4, "base env should have 4 entries");

    // With both module name and dir, should have 6
    let env_with_module = super::build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/x"),
        profile_name: "p",
        context: ReconcileContext::Apply,
        phase: &ScriptPhase::PreApply,
        module_name: Some("m"),
        module_dir: Some(std::path::Path::new("/modules/m")),
        path_dirs: &[],
    });
    assert_eq!(
        env_with_module.len(),
        6,
        "env with module info should have 6 entries"
    );
}

// --- verify additional tests ---

#[test]
fn verify_empty_profile_returns_no_results() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let resolved = make_empty_resolved();
    let printer = test_printer();

    let results = verify(
        &resolved,
        &registry,
        &state,
        &[],
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;
    assert!(
        results.is_empty(),
        "empty profile with no modules should produce no verify results, got: {:?}",
        results
    );
}

// Profile-level managed-file verification moved to the binary crate
// (`cli::live_drift`), which is content-aware via CfgdFileManager. The reconciler
// no longer produces presence-only "file" results, so the former
// verify_file_target_exists / verify_file_target_missing tests live there now
// (file_verify_results_* in crates/cfgd/src/cli/live_drift.rs). MODULE-file
// verification is likewise content-aware and folded in by the binary crate
// (module_file_verify_results) — the reconciler is presence-blind across the
// crate boundary, so it emits NO module-file rows at all.

#[test]
fn verify_module_files_produce_no_reconciler_rows() {
    // A module with files (target missing OR present) yields no module-file row
    // from the reconciler: file content-awareness is the binary crate's job.
    let state = test_state();
    let registry = ProviderRegistry::new();
    let printer = test_printer();
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "test-mod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: PathBuf::from("/src/config"),
            target: PathBuf::from("/tmp/cfgd-test-nonexistent-module-file-29384"),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let results = verify(
        &resolved,
        &registry,
        &state,
        &modules,
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    // No module rows: the module has no packages (the only thing the reconciler
    // now checks for modules) and module files are not its responsibility.
    let module_rows: Vec<_> = results
        .iter()
        .filter(|r| r.resource_type == "module")
        .collect();
    assert!(
        module_rows.is_empty(),
        "reconciler must not emit module-file rows: {module_rows:?}"
    );
}

#[test]
fn verify_multiple_packages_mixed_status() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    // Only "git" installed, "tmux" missing
    registry.add_package_manager(Box::new(
        MockPackageManager::new("apt").with_installed(&["git"]),
    ));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.apt = Some(crate::config::AptSpec {
        file: None,
        packages: vec!["git".to_string(), "tmux".to_string()],
    });

    let printer = test_printer();
    let results = verify(
        &resolved,
        &registry,
        &state,
        &[],
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    let git_result = results
        .iter()
        .find(|r| r.resource_id == "apt:git")
        .expect("should have git result");
    assert!(git_result.matches);
    assert_eq!(git_result.expected, "installed");
    assert_eq!(git_result.actual, "installed");

    let tmux_result = results
        .iter()
        .find(|r| r.resource_id == "apt:tmux")
        .expect("should have tmux result");
    assert!(!tmux_result.matches);
    assert_eq!(tmux_result.expected, "installed");
    // The stored literal is the one live_drift stores for the same fact.
    assert_eq!(tmux_result.actual, crate::Absence::NotInstalled.as_str());
}

// --- format_action_description additional tests ---

#[test]
fn format_action_description_env_write_file() {
    let action = Action::Env(EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/user/.cfgd.env"),
        content: "export FOO=bar\n".to_string(),
        vars: 0,
        aliases: 0,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "env:write:/home/user/.cfgd.env");
}

#[test]
fn format_action_description_env_inject_source() {
    let action = Action::Env(EnvAction::InjectSourceLine {
        rc_path: PathBuf::from("/home/user/.zshrc"),
        line: ". ~/.cfgd.env".to_string(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "env:inject:/home/user/.zshrc");
}

#[test]
fn format_action_description_script_run_entry() {
    let action = Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple("echo hello".to_string()),
        phase: ScriptPhase::PreApply,
        origin: "local".to_string(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "script:preApply:echo hello");
}

#[test]
fn format_action_description_system_set_value_sysctl() {
    let action = Action::System(SystemAction::SetValue {
        configurator: "sysctl".to_string(),
        key: "net.ipv4.ip_forward".to_string(),
        desired: "1".to_string(),
        current: "0".to_string(),
        origin: "local".to_string(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "system:sysctl.net.ipv4.ip_forward");
}

#[test]
fn format_action_description_system_skip_sysctl() {
    let action = Action::System(SystemAction::Skip {
        configurator: "sysctl".to_string(),
        reason: "not available".to_string(),
        origin: "local".to_string(),
        unknown: false,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "system:sysctl:skip");
}

#[test]
fn format_action_description_module_install_multiple_packages() {
    let action = Action::Module(ModuleAction {
        module_name: "neovim".to_string(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![
                ResolvedPackage {
                    canonical_name: "neovim".to_string(),
                    resolved_name: "neovim".to_string(),
                    manager: "brew".to_string(),
                    manager_declared: false,
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
                    min_version: None,
                },
                ResolvedPackage {
                    canonical_name: "ripgrep".to_string(),
                    resolved_name: "ripgrep".to_string(),
                    manager: "brew".to_string(),
                    manager_declared: false,
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
                    min_version: None,
                },
            ],
        },
        origin: None,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "module:neovim:packages:neovim,ripgrep");
}

#[test]
fn format_action_description_module_deploy_two_files() {
    let action = Action::Module(ModuleAction {
        module_name: "nvim".to_string(),
        kind: {
            let files = vec![
                ResolvedFile {
                    source: PathBuf::from("/src/init.lua"),
                    target: PathBuf::from("/home/.config/nvim/init.lua"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                },
                ResolvedFile {
                    source: PathBuf::from("/src/plugins.lua"),
                    target: PathBuf::from("/home/.config/nvim/plugins.lua"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                },
            ];
            let declared_total = files.len();
            ModuleActionKind::DeployFiles {
                files,
                declared_total,
            }
        },
        origin: None,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "module:nvim:files:2");
}

#[test]
fn format_action_description_module_run_post_apply_script() {
    let action = Action::Module(ModuleAction {
        module_name: "rust".to_string(),
        kind: ModuleActionKind::RunScript {
            script: ScriptEntry::Simple("./setup.sh".to_string()),
            phase: ScriptPhase::PostApply,
        },
        origin: None,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "module:rust:script");
}

#[test]
fn format_action_description_module_skip_dependency() {
    let action = Action::Module(ModuleAction {
        module_name: "rust".to_string(),
        kind: ModuleActionKind::Skip {
            reason: "dependency not met".to_string(),
        },
        origin: None,
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "module:rust:skip");
}

#[test]
fn format_action_description_manager_provision() {
    let action = Action::Manager(ManagerAction::Provision {
        manager: "brew".to_string(),
        via: "homebrew installer".to_string(),
        declared: None,
        batched: vec![],
        depends_on: vec![],
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "manager:provision:brew");
}

#[test]
fn format_action_description_package_uninstall() {
    let action = Action::Package(PackageAction::Uninstall {
        manager: "apt".to_string(),
        packages: vec!["vim".to_string(), "nano".to_string()],
        origin: "local".to_string(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "package:apt:uninstall:vim,nano");
}

#[test]
fn format_action_description_file_set_permissions() {
    let action = Action::File(FileAction::SetPermissions {
        target: PathBuf::from("/etc/config.yaml"),
        mode: 0o600,
        origin: "local".to_string(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "file:chmod:0o600:/etc/config.yaml");
}

// --- PhaseName tests ---

#[test]
fn phase_name_all_variants_roundtrip() {
    let variants = [
        ("pre-scripts", PhaseName::PreScripts, "Pre-Scripts"),
        ("prerequisites", PhaseName::Prerequisites, "Prerequisites"),
        ("modules", PhaseName::Modules, "Modules"),
        ("packages", PhaseName::Packages, "Packages"),
        ("system", PhaseName::System, "System"),
        ("files", PhaseName::Files, "Files"),
        ("secrets", PhaseName::Secrets, "Secrets"),
        ("post-scripts", PhaseName::PostScripts, "Post-Scripts"),
    ];

    for (s, expected_variant, display) in &variants {
        let parsed = PhaseName::from_str(s).unwrap();
        assert_eq!(&parsed, expected_variant);
        assert_eq!(parsed.as_str(), *s);
        assert_eq!(parsed.display_name(), *display);
    }
}

#[test]
fn phase_name_unknown_returns_err() {
    let result = PhaseName::from_str("unknown-phase");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("unknown phase"),
        "error should mention unknown phase: {}",
        err
    );
}

// --- ScriptPhase display_name tests ---

#[test]
fn script_phase_display_names() {
    assert_eq!(ScriptPhase::PreApply.display_name(), "preApply");
    assert_eq!(ScriptPhase::PostApply.display_name(), "postApply");
    assert_eq!(ScriptPhase::PreReconcile.display_name(), "preReconcile");
    assert_eq!(ScriptPhase::PostReconcile.display_name(), "postReconcile");
    assert_eq!(ScriptPhase::OnDrift.display_name(), "onDrift");
    assert_eq!(ScriptPhase::OnChange.display_name(), "onChange");
}

// --- verify_env_file tests ---

#[test]
fn verify_env_file_matches_when_content_equal() {
    let tmp = tempfile::tempdir().unwrap();
    let env_path = tmp.path().join("test.env");
    let expected = "export FOO=\"bar\"\n";
    std::fs::write(&env_path, expected).unwrap();

    let mut results = Vec::new();
    super::verify_env_file(&env_path, expected, &mut results);

    assert_eq!(results.len(), 1);
    assert!(results[0].matches);
    assert_eq!(results[0].resource_type, "env");
    assert_eq!(results[0].expected, "current");
    assert_eq!(results[0].actual, "current");
}

#[test]
fn verify_env_file_stale_when_content_differs() {
    let tmp = tempfile::tempdir().unwrap();
    let env_path = tmp.path().join("test.env");
    std::fs::write(&env_path, "old content").unwrap();

    let mut results = Vec::new();
    super::verify_env_file(&env_path, "new content", &mut results);

    assert_eq!(results.len(), 1);
    assert!(!results[0].matches);
    assert_eq!(results[0].expected, "current");
    assert_eq!(results[0].actual, "stale");
}

#[test]
fn verify_env_file_missing_when_file_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let env_path = tmp.path().join("nonexistent.env");

    let mut results = Vec::new();
    super::verify_env_file(&env_path, "expected content", &mut results);

    assert_eq!(results.len(), 1);
    assert!(!results[0].matches);
    assert_eq!(results[0].expected, "present");
    assert_eq!(results[0].actual, "missing");
}

// --- env_verify_results / verify_env per-item alias & env-var tests ---

/// The primary managed env file `env_targets` would write for `env`/`aliases`
/// on THIS platform, enumerated through the very function `env_verify_results`
/// itself calls (`EnvHostProbe::detect` + `EnvPlatform::current()` +
/// `env_targets`) — so a fixture seeded from it can never drift from what the
/// verifier treats as the primary file: bash/zsh's `.cfgd.env` on Unix,
/// PowerShell's `.cfgd-env.ps1` on Windows.
fn primary_managed_env_target(
    home: &Path,
    env: &[EnvVar],
    aliases: &[ShellAlias],
) -> (PathBuf, String) {
    let probe = EnvHostProbe::detect(home);
    let platform = EnvPlatform::current();
    env_targets(
        EnvContent::new(env, aliases, &[], &Default::default()),
        EnvScope::All,
        home,
        &probe,
        platform,
    )
    .into_iter()
    .find_map(|t| match t {
        EnvTarget::ManagedFile { path, content, .. } => Some((path, content)),
        _ => None,
    })
    .expect("env_targets yields a primary managed file for non-empty env/aliases")
}

#[test]
#[serial_test::serial]
fn env_verify_results_reports_matching_alias_and_env_var_as_current() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let env = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "nvim".to_string(),
        platforms: vec![],
    }];
    let aliases = vec![ShellAlias {
        name: "ll".to_string(),
        command: "ls -la".to_string(),
        platforms: vec![],
    }];

    // Seed the primary managed file exactly as `apply` would generate it, so
    // the per-item check reads a real, matching baseline.
    let (path, content) = primary_managed_env_target(tmp_home.path(), &env, &aliases);
    std::fs::write(path, content).unwrap();

    let results = super::verify::env_verify_results(
        &env,
        &aliases,
        &Default::default(),
        EnvScope::All,
        &[],
        &[],
    );

    let alias_row = results
        .iter()
        .find(|r| r.resource_type == "alias" && r.resource_id == "ll")
        .expect("alias row present");
    assert!(alias_row.matches);
    assert_eq!(alias_row.actual, "current");

    let env_row = results
        .iter()
        .find(|r| r.resource_type == "env-var" && r.resource_id == "EDITOR")
        .expect("env-var row present");
    assert!(env_row.matches);
    assert_eq!(env_row.actual, "current");
}

#[test]
#[serial_test::serial]
fn env_verify_results_detects_hand_edited_alias_as_drift_without_flagging_untouched_env_var() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let env = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "nvim".to_string(),
        platforms: vec![],
    }];
    let aliases = vec![ShellAlias {
        name: "ll".to_string(),
        command: "ls -la".to_string(),
        platforms: vec![],
    }];

    // Generate the real baseline, then hand-edit only the alias's command —
    // the same shape a user editing the generated file out-of-band produces.
    // The needle and its replacement are both derived from
    // `primary_alias_line` (the real declared line vs. the line a hand-edited
    // command would render), never a hardcoded POSIX literal — so the
    // mutation is meaningful on whichever dialect this platform writes.
    let (path, content) = primary_managed_env_target(tmp_home.path(), &env, &aliases);
    let platform = EnvPlatform::current();
    let declared_line =
        super::env_files::primary_alias_line(&aliases[0], platform, &Default::default())
            .expect("alias renders a declared line");
    let hand_edited = ShellAlias {
        name: "ll".to_string(),
        command: "ls -lah".to_string(),
        platforms: vec![],
    };
    let hand_edited_line =
        super::env_files::primary_alias_line(&hand_edited, platform, &Default::default())
            .expect("hand-edited alias renders a line");
    let mutated = content.replace(&declared_line, &hand_edited_line);
    assert_ne!(
        content, mutated,
        "fixture must actually mutate the alias line"
    );
    std::fs::write(path, mutated).unwrap();

    let results = super::verify::env_verify_results(
        &env,
        &aliases,
        &Default::default(),
        EnvScope::All,
        &[],
        &[],
    );

    let alias_row = results
        .iter()
        .find(|r| r.resource_type == "alias" && r.resource_id == "ll")
        .expect("alias row present");
    assert!(!alias_row.matches);
    // Opaque markers only: `expected`/`actual` must never carry the alias's
    // real declared command, which flows unmodified into `drift_events` and
    // the device gateway and can be sensitive.
    assert_eq!(alias_row.actual, "missing or changed");
    assert_eq!(alias_row.expected, "current");

    // The env var's own line is untouched, so it must not be swept up in the
    // alias's drift — per-item attribution, not a whole-file verdict.
    let env_row = results
        .iter()
        .find(|r| r.resource_type == "env-var" && r.resource_id == "EDITOR")
        .expect("env-var row present");
    assert!(env_row.matches);
}

/// WARN regression: an env/alias verify row must never carry the declared
/// value — only the opaque `current`/`missing or changed` markers. A
/// declared value can be sensitive (a secret-shaped env var, a command
/// embedding a token) and the row flows unmodified into `drift_events` (the
/// CLI recording seam stores each producer's own literals) and from there to
/// `cfgd status`/`cfgd diff` *and* the device gateway, so the real content
/// has to be recomputed from config at render time
/// (`env_item_declared_line`) rather than carried. Uses a deliberately
/// secret-shaped env value and alias command so a regression that words the
/// row with the raw line fails on the sensitive substring, not just on a
/// generic marker string.
#[test]
#[serial_test::serial]
fn env_verify_results_carry_only_the_opaque_markers_never_the_declared_value() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let env = vec![EnvVar {
        name: "API_TOKEN".to_string(),
        value: "sk-super-secret-value".to_string(),
        platforms: vec![],
    }];
    let aliases = vec![ShellAlias {
        name: "deploy".to_string(),
        command: "curl -H 'Authorization: Bearer sk-super-secret-value' https://example.com"
            .to_string(),
        platforms: vec![],
    }];
    // The primary managed file (whichever dialect this platform writes)
    // exists but carries neither declared line — an absent file is left to
    // the whole-file check instead (per `verify_env_items`'s doc comment), so
    // the per-item rows below need a present-but-non-matching file to
    // exercise the "missing or changed" arm. `ENV_FILE_HEADER` is the one
    // line every dialect's generator opens with, so the header alone is a
    // legal (if incomplete) managed file on any platform.
    let (path, _) = primary_managed_env_target(tmp_home.path(), &env, &aliases);
    std::fs::write(path, format!("{ENV_FILE_HEADER}\n")).unwrap();

    let results = super::verify::env_verify_results(
        &env,
        &aliases,
        &Default::default(),
        EnvScope::All,
        &[],
        &[],
    );

    for (rtype, rid) in [("env-var", "API_TOKEN"), ("alias", "deploy")] {
        let row = results
            .iter()
            .find(|r| r.resource_type == rtype && r.resource_id == rid)
            .unwrap_or_else(|| panic!("{rtype} row present"));
        assert!(!row.matches);
        assert!(
            !row.expected.contains("sk-super-secret-value")
                && !row.actual.contains("sk-super-secret-value"),
            "declared value must never be a row's operand: {row:?}"
        );
        assert_eq!(row.expected, "current");
        assert_eq!(row.actual, "missing or changed");
    }
}

/// The counterpart of the rule above: the opaque markers stay OUT of the
/// stored row, and the display recompute puts real values back at render
/// time — the declared line against the line the managed file actually
/// holds. A row rendering `have: missing or changed` names no value at all,
/// so the reader cannot tell a hand-edited value from a deleted one without
/// opening the file themselves.
#[test]
#[serial_test::serial]
fn a_drifted_env_row_shows_the_line_the_file_holds_against_the_declared_one() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let declared = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "nvim".to_string(),
        platforms: vec![],
    }];
    let edited = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "emacs".to_string(),
        platforms: vec![],
    }];
    // Both lines come from production's own renderer rather than a POSIX
    // literal, so the fixture holds whatever dialect this platform writes.
    let edited_line =
        super::verify::MergedEnvItems::new(&edited, &[], &Default::default(), &[], &[])
            .declared_line("env-var", "EDITOR")
            .expect("the edited var renders a line");
    let (path, _) = primary_managed_env_target(tmp_home.path(), &declared, &[]);
    std::fs::write(path, format!("{ENV_FILE_HEADER}\n{edited_line}\n")).unwrap();

    let (want, have) =
        super::verify::MergedEnvItems::new(&declared, &[], &Default::default(), &[], &[])
            .display_values("env-var", "EDITOR")
            .expect("a declared env var recomputes both operands");
    assert_eq!(
        want,
        super::verify::MergedEnvItems::new(&declared, &[], &Default::default(), &[], &[])
            .declared_line("env-var", "EDITOR")
            .unwrap(),
        "want is the line the declaration renders as"
    );
    assert_eq!(
        have, edited_line,
        "have is the line the file actually holds, not a marker"
    );
}

/// The merge is built ONCE and asked per row: a command rendering a drift
/// report holds one view and answers every finding from it, rather than
/// cloning the profile's env, its aliases and both origin maps per row.
#[test]
#[serial_test::serial]
fn one_merged_env_view_answers_every_row_of_a_report() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let env = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "nvim".to_string(),
        platforms: vec![],
    }];
    let aliases = vec![ShellAlias {
        name: "ll".to_string(),
        command: "ls -lah".to_string(),
        platforms: vec![],
    }];
    let view = super::verify::MergedEnvItems::new(&env, &aliases, &Default::default(), &[], &[]);

    let editor = view
        .declared_line("env-var", "EDITOR")
        .expect("the env var renders a line");
    let ll = view
        .declared_line("alias", "ll")
        .expect("the alias renders a line from the same view");
    assert!(
        editor.contains("EDITOR") && ll.contains("ll"),
        "{editor} / {ll}"
    );
    assert_eq!(
        view.declared_line("file", "/etc/hosts"),
        None,
        "a kind with no managed line answers None from the same view"
    );
}

/// A declared item no deployed line claims reads as the shared absence word,
/// never as a second spelling of it — and a kind that has no managed line at
/// all recomputes nothing, leaving the caller's own operands standing.
#[test]
#[serial_test::serial]
fn an_env_item_the_file_does_not_hold_reads_as_the_shared_absence_word() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let declared = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "nvim".to_string(),
        platforms: vec![],
    }];
    let (path, _) = primary_managed_env_target(tmp_home.path(), &declared, &[]);
    std::fs::write(path, format!("{ENV_FILE_HEADER}\n")).unwrap();

    let (_, have) =
        super::verify::MergedEnvItems::new(&declared, &[], &Default::default(), &[], &[])
            .display_values("env-var", "EDITOR")
            .expect("a declared env var recomputes both operands");
    assert_eq!(have, crate::Absence::Missing.as_str());

    assert!(
        super::verify::MergedEnvItems::new(&declared, &[], &Default::default(), &[], &[])
            .display_values("file", "~/.zshrc")
            .is_none(),
        "a kind with no managed env line recomputes nothing"
    );
    assert!(
        super::verify::MergedEnvItems::new(&declared, &[], &Default::default(), &[], &[])
            .display_values("env-var", "PAGER")
            .is_none(),
        "an item no longer declared recomputes nothing"
    );
}

/// A managed file that EXISTS and cannot be read says nothing about whether
/// the entry is deployed, and reporting `have: missing` there claims an
/// absence the machine never confirmed. Only `NotFound` may read as absent;
/// every other error leaves the caller holding its own operands.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn an_unreadable_managed_env_file_recomputes_nothing_rather_than_claiming_absence() {
    use std::os::unix::fs::PermissionsExt;

    // Root ignores the mode bits entirely, so the unreadable file is readable
    // and the branch under test is not the one that runs.
    if crate::is_root() {
        return;
    }

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let declared = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "nvim".to_string(),
        platforms: vec![],
    }];
    let (path, _) = primary_managed_env_target(tmp_home.path(), &declared, &[]);
    std::fs::write(&path, format!("{ENV_FILE_HEADER}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let recomputed =
        super::verify::MergedEnvItems::new(&declared, &[], &Default::default(), &[], &[])
            .display_values("env-var", "EDITOR");

    // Restore before asserting so a failure does not leave the tempdir
    // undeletable.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        recomputed.is_none(),
        "a file that could not be read must not be reported as an absent entry: {recomputed:?}"
    );
}

/// A successful write of the primary managed env file converges every entry
/// inside it, so the per-item `env-var`/`alias` rows heal with the file.
///
/// Before this, only the file's own `("env", <path>)` row resolved — the item
/// rows are keyed by the entry's NAME and no action ever names one, so they
/// stayed open forever and a converged machine kept reporting drift about
/// entries the file already holds. The resolution never touches an operand:
/// the stored `current` / `missing or changed` markers are a wire contract,
/// and a row that is still open must still read back exactly as it was
/// written.
#[test]
#[serial_test::serial]
fn a_successful_env_apply_resolves_the_per_item_rows_it_converged() {
    let home = tempfile::tempdir().unwrap();
    let _home_guard = crate::with_test_home_guard(home.path());
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::with_home(&registry, &state, home.path());

    let mut resolved = make_empty_resolved();
    resolved.merged.env = vec![EnvVar {
        name: "EDITOR".to_string(),
        value: "nvim".to_string(),
        platforms: vec![],
    }];
    resolved.merged.aliases = vec![ShellAlias {
        name: "ll".to_string(),
        command: "ls -la".to_string(),
        platforms: vec![],
    }];
    resolved.merged.env_scope = EnvScope::Interactive;

    for (rtype, rid) in [
        ("env-var", "EDITOR"),
        ("alias", "ll"),
        // Declared by nobody: the resolution is scoped to what the write
        // covered, so this row must survive with its operands untouched.
        ("env-var", "PAGER"),
    ] {
        state
            .record_drift(
                rtype,
                rid,
                Some("current"),
                Some("missing or changed"),
                crate::config::LOCAL_LAYER,
            )
            .unwrap();
    }

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();
    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let open = state.unresolved_drift().unwrap();
    for (rtype, rid) in [("env-var", "EDITOR"), ("alias", "ll")] {
        assert!(
            !open
                .iter()
                .any(|d| d.resource_type == rtype && d.resource_id == rid),
            "the apply wrote {rid} into the managed file, so its row must resolve: {open:?}"
        );
    }
    let pager = open
        .iter()
        .find(|d| d.resource_type == "env-var" && d.resource_id == "PAGER")
        .unwrap_or_else(|| panic!("an undeclared item's row must survive the write: {open:?}"));
    assert_eq!(
        (pager.expected.as_deref(), pager.actual.as_deref()),
        (Some("current"), Some("missing or changed")),
        "resolution never rewrites an operand: {pager:?}"
    );
}

// --- merge_module_env_aliases tests ---

#[test]
fn merge_module_env_aliases_empty() {
    let (env, aliases, _origins) =
        super::merge_module_env_aliases(&[], &[], &Default::default(), &[]);
    assert!(env.is_empty());
    assert!(aliases.is_empty());
}

#[test]
fn merge_module_env_aliases_combines_profile_and_modules() {
    let profile_env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "vim".into(),
        platforms: vec![],
    }];
    let profile_aliases = vec![crate::config::ShellAlias {
        name: "g".into(),
        command: "git".into(),
        platforms: vec![],
    }];
    let modules = vec![ResolvedModule {
        name: "test".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![crate::config::EnvVar {
            name: "PAGER".into(),
            value: "less".into(),
            platforms: vec![],
        }],
        aliases: vec![crate::config::ShellAlias {
            name: "ll".into(),
            command: "ls -la".into(),
            platforms: vec![],
        }],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let (env, aliases, _origins) = super::merge_module_env_aliases(
        &profile_env,
        &profile_aliases,
        &Default::default(),
        &modules,
    );
    assert_eq!(env.len(), 2);
    assert_eq!(aliases.len(), 2);

    // Check that both profile and module values are present
    assert!(env.iter().any(|e| e.name == "EDITOR"));
    assert!(env.iter().any(|e| e.name == "PAGER"));
    assert!(aliases.iter().any(|a| a.name == "g"));
    assert!(aliases.iter().any(|a| a.name == "ll"));
}

#[test]
fn merge_module_env_aliases_module_overrides_profile() {
    let profile_env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "vim".into(),
        platforms: vec![],
    }];
    let modules = vec![ResolvedModule {
        name: "test".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
            platforms: vec![],
        }],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let (env, _, _) =
        super::merge_module_env_aliases(&profile_env, &[], &Default::default(), &modules);
    // merge_env deduplicates by name, last wins
    let editor = env.iter().find(|e| e.name == "EDITOR").unwrap();
    assert_eq!(
        editor.value, "nvim",
        "module should override profile env var"
    );
}

// --- Module deploy files: hardlink strategy ---

#[test]
fn apply_module_deploy_files_hardlink_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("source.txt");
    let target_file = dir.path().join("hardlink-target.txt");
    std::fs::write(&source_file, "hardlinked content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Hardlink;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "hardmod".to_string(),
                kind: {
                    let files = vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Hardlink),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let modules = vec![ResolvedModule {
        name: "hardmod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source_file.clone(),
            target: target_file.clone(),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Hardlink),
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        !target_file.is_symlink(),
        "hardlink should not be a symlink"
    );
    assert_eq!(
        std::fs::read_to_string(&target_file).unwrap(),
        "hardlinked content"
    );
    // Verify it's a hardlink by checking inode (Unix)
    #[cfg(unix)]
    {
        assert!(
            crate::is_same_inode(&source_file, &target_file),
            "source and target should share the same inode"
        );
    }
}

// --- Module deploy files: copy strategy ---

#[test]
fn apply_module_deploy_files_copy_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("source.txt");
    let target_file = dir.path().join("copy-target.txt");
    std::fs::write(&source_file, "copied content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "copymod".to_string(),
                kind: {
                    let files = vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let modules = vec![ResolvedModule {
        name: "copymod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source_file.clone(),
            target: target_file.clone(),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(!target_file.is_symlink(), "copy should not be a symlink");
    assert_eq!(
        std::fs::read_to_string(&target_file).unwrap(),
        "copied content"
    );
    // Verify it's NOT a hardlink (independent copy)
    #[cfg(unix)]
    {
        assert!(
            !crate::is_same_inode(&source_file, &target_file),
            "copy should have a different inode"
        );
    }
}

// --- Module deploy files: permissions applied after deploy ---

#[cfg(unix)]
#[test]
fn apply_module_deploy_files_applies_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("source.sh");
    let target_file = dir.path().join("bin").join("tool");
    std::fs::write(&source_file, "#!/bin/sh\necho hi\n").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let file = ResolvedFile {
        source: source_file.clone(),
        target: target_file.clone(),
        is_git_source: false,
        strategy: Some(crate::config::FileStrategy::Copy),
        encryption: None,
        permissions: Some("750".to_string()),
        patch: None,
    };

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "permmod".to_string(),
                kind: {
                    let files = vec![file.clone()];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let modules = vec![ResolvedModule {
        name: "permmod".to_string(),
        packages: vec![],
        files: vec![file],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    let mode = std::fs::metadata(&target_file)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o750, "deployed module file should be mode 0o750");
}

// --- Module deploy files: directory with symlink vs copy ---

#[test]
fn apply_module_deploy_files_directory_copy_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("src-dir");
    std::fs::create_dir(&source_dir).unwrap();
    std::fs::write(source_dir.join("a.txt"), "aaa").unwrap();
    std::fs::write(source_dir.join("b.txt"), "bbb").unwrap();

    let target_dir = dir.path().join("target-dir");

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "dirmod".to_string(),
                kind: {
                    let files = vec![ResolvedFile {
                        source: source_dir.clone(),
                        target: target_dir.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let modules = vec![ResolvedModule {
        name: "dirmod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: source_dir.clone(),
            target: target_dir.clone(),
            is_git_source: false,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(target_dir.is_dir(), "target should be a directory");
    assert!(!target_dir.is_symlink(), "copy should not be a symlink");
    assert_eq!(
        std::fs::read_to_string(target_dir.join("a.txt")).unwrap(),
        "aaa"
    );
    assert_eq!(
        std::fs::read_to_string(target_dir.join("b.txt")).unwrap(),
        "bbb"
    );
}

// --- Module deploy files: overwrites existing target ---

#[test]
fn apply_module_deploy_files_overwrites_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("source.txt");
    let target_file = dir.path().join("target.txt");
    std::fs::write(&source_file, "new content").unwrap();
    std::fs::write(&target_file, "old content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "overmod".to_string(),
                kind: {
                    let files = vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let modules = vec![ResolvedModule {
        name: "overmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(
        std::fs::read_to_string(&target_file).unwrap(),
        "new content",
        "existing file should be overwritten"
    );
}

// --- Module-level onChange script runs when module changes ---

#[test]
#[cfg(unix)]
fn apply_module_on_change_script_runs_when_module_has_changes() {
    let dir = tempfile::tempdir().unwrap();
    let source_file = dir.path().join("source.txt");
    let target_file = dir.path().join("target.txt");
    std::fs::write(&source_file, "content").unwrap();
    let marker = dir.path().join("onchange-ran");

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "changemod".to_string(),
                kind: {
                    let files = vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let modules = vec![ResolvedModule {
        name: "changemod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![crate::config::ScriptEntry::Simple(format!(
            "touch {}",
            marker.display()
        ))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None, // no phase filter — run everything including onChange
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        marker.exists(),
        "module onChange script should have created marker file"
    );
}

#[test]
#[cfg(unix)]
fn apply_module_on_change_script_does_not_run_when_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("onchange-ran");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // Empty plan — no actions, so no module changes
    let plan = Plan {
        phases: vec![],
        warnings: vec![],
    };

    let modules = vec![ResolvedModule {
        name: "nochangemod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![crate::config::ScriptEntry::Simple(format!(
            "touch {}",
            marker.display()
        ))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        !marker.exists(),
        "module onChange should NOT run when module had no changes"
    );
}

// --- Rollback restores file to correct content ---

#[test]
fn rollback_restores_file_with_correct_content() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("managed.txt");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    // Record a first apply with a file backup
    let file_state = crate::FileState {
        content: b"original content".to_vec(),
        content_hash: crate::sha256_hex(b"original content"),
        permissions: Some(0o644),
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };
    let apply_id_1 = state
        .record_apply("default", "plan-hash-1", ApplyStatus::InProgress, None)
        .unwrap();
    state
        .store_file_backup(apply_id_1, &file_path.display().to_string(), &file_state)
        .unwrap();
    state
        .update_apply_status(apply_id_1, ApplyStatus::Success, Some("{}"))
        .unwrap();

    // Record a second apply that changed the file
    let new_state = crate::FileState {
        content: b"modified content".to_vec(),
        content_hash: crate::sha256_hex(b"modified content"),
        permissions: Some(0o644),
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };
    let apply_id_2 = state
        .record_apply("default", "plan-hash-2", ApplyStatus::InProgress, None)
        .unwrap();
    state
        .store_file_backup(apply_id_2, &file_path.display().to_string(), &new_state)
        .unwrap();
    state
        .update_apply_status(apply_id_2, ApplyStatus::Success, Some("{}"))
        .unwrap();

    // Write the current file with apply-2 content
    std::fs::write(&file_path, "modified content").unwrap();

    let printer = test_printer();
    let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    assert!(
        result.files_restored > 0,
        "should restore at least one file"
    );
    assert_eq!(
        std::fs::read_to_string(&file_path).unwrap(),
        "original content",
        "file should be restored to apply-1 state"
    );
}

// --- Rollback: removes files created after target ---

#[test]
fn rollback_removes_file_created_after_target_apply() {
    let dir = tempfile::tempdir().unwrap();
    let created_file = dir.path().join("new-file.txt");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    // Apply 1: a simple apply that didn't touch new-file.txt
    let apply_id_1 = state
        .record_apply("default", "hash-1", ApplyStatus::InProgress, None)
        .unwrap();
    state
        .update_apply_status(apply_id_1, ApplyStatus::Success, None)
        .unwrap();

    // Apply 2: creates new-file.txt (file didn't exist before). Apply records
    // an absent marker as the pre-action backup of the CREATE — the durable
    // fact rollback uses to remove the file.
    let apply_id_2 = state
        .record_apply("default", "hash-2", ApplyStatus::InProgress, None)
        .unwrap();
    state
        .store_absent_backup(apply_id_2, &created_file.display().to_string())
        .unwrap();
    let j_id = state
        .journal_begin(
            apply_id_2,
            0,
            "files",
            "file",
            &format!("file:create:{}", created_file.display()),
            None,
        )
        .unwrap();
    state.journal_complete(j_id, 0, None, None).unwrap();
    state
        .update_apply_status(apply_id_2, ApplyStatus::Success, None)
        .unwrap();

    // Write the file to disk (simulating what apply 2 did)
    std::fs::write(&created_file, "new content").unwrap();
    assert!(created_file.exists());

    // Rollback to apply 1 — file didn't exist then, should be removed
    let printer = test_printer();
    let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    assert!(
        !created_file.exists(),
        "file created after target apply should be removed"
    );
    assert!(
        result.files_removed > 0,
        "files_removed should reflect the deletion"
    );
}

#[test]
fn rollback_keeps_file_that_existed_at_target_apply() {
    let dir = tempfile::tempdir().unwrap();
    let existing_file = dir.path().join("existing.txt");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    // Apply 1: creates existing.txt (journal records file:create:...)
    let apply_id_1 = state
        .record_apply("default", "hash-1", ApplyStatus::InProgress, None)
        .unwrap();
    let j_id = state
        .journal_begin(
            apply_id_1,
            0,
            "files",
            "file",
            &format!("file:create:{}", existing_file.display()),
            None,
        )
        .unwrap();
    state.journal_complete(j_id, 0, None, None).unwrap();
    // Store backup so phase 1 handles it
    let file_state = crate::FileState {
        content: b"original".to_vec(),
        content_hash: crate::sha256_hex(b"original"),
        permissions: Some(0o644),
        is_symlink: false,
        symlink_target: None,
        oversized: false,
    };
    state
        .store_file_backup(
            apply_id_1,
            &existing_file.display().to_string(),
            &file_state,
        )
        .unwrap();
    state
        .update_apply_status(apply_id_1, ApplyStatus::Success, None)
        .unwrap();

    // Apply 2: updates existing.txt
    let apply_id_2 = state
        .record_apply("default", "hash-2", ApplyStatus::InProgress, None)
        .unwrap();
    let j_id = state
        .journal_begin(
            apply_id_2,
            0,
            "files",
            "file",
            &format!("file:create:{}", existing_file.display()),
            None,
        )
        .unwrap();
    state.journal_complete(j_id, 0, None, None).unwrap();
    state
        .update_apply_status(apply_id_2, ApplyStatus::Success, None)
        .unwrap();

    // Write current state
    std::fs::write(&existing_file, "modified").unwrap();

    // Rollback to apply 1 — file existed at apply 1, should be restored not removed
    let printer = test_printer();
    let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    assert!(
        existing_file.exists(),
        "file that existed at target apply should NOT be removed"
    );
    assert_eq!(
        std::fs::read_to_string(&existing_file).unwrap(),
        "original",
        "file should be restored to target apply state"
    );
    assert!(result.files_restored > 0);
}

#[test]
fn rollback_collects_non_file_actions_from_subsequent_applies() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    // Apply 1: base state
    let apply_id_1 = state
        .record_apply("default", "hash-1", ApplyStatus::InProgress, None)
        .unwrap();
    state
        .update_apply_status(apply_id_1, ApplyStatus::Success, None)
        .unwrap();

    // Apply 2: installs a package and runs a script
    let apply_id_2 = state
        .record_apply("default", "hash-2", ApplyStatus::InProgress, None)
        .unwrap();
    let j1 = state
        .journal_begin(apply_id_2, 0, "Packages", "install", "brew:ripgrep", None)
        .unwrap();
    state.journal_complete(j1, 0, None, None).unwrap();
    let j2 = state
        .journal_begin(
            apply_id_2,
            1,
            "PostScripts",
            "script",
            "script:post:setup.sh",
            None,
        )
        .unwrap();
    // The completion counter is monotonic within a run, so two rows of one
    // apply can never share an index.
    state.journal_complete(j2, 1, None, None).unwrap();
    state
        .update_apply_status(apply_id_2, ApplyStatus::Success, None)
        .unwrap();

    // Rollback to apply 1
    let printer = test_printer();
    let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    // Non-file actions from subsequent applies should be listed for manual review
    assert!(
        result
            .non_file_actions
            .contains(&("install".to_string(), "brew:ripgrep".to_string())),
        "should list package action for manual review: {:?}",
        result.non_file_actions
    );
    assert!(
        result
            .non_file_actions
            .contains(&("script".to_string(), "script:post:setup.sh".to_string())),
        "should list script action for manual review: {:?}",
        result.non_file_actions
    );
}

// --- Verify: system configurator drift detection ---

#[test]
fn verify_system_configurator_reports_drift() {
    struct DriftingConfigurator;

    impl crate::providers::SystemConfigurator for DriftingConfigurator {
        fn name(&self) -> &str {
            "sysctl"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn current_state(&self) -> crate::errors::Result<serde_yaml::Value> {
            Ok(serde_yaml::Value::Null)
        }
        fn diff(
            &self,
            _: &serde_yaml::Value,
        ) -> crate::errors::Result<Vec<crate::providers::SystemDrift>> {
            Ok(vec![
                crate::providers::SystemDrift {
                    key: "vm.swappiness".to_string(),
                    expected: "10".to_string(),
                    actual: "60".to_string(),
                },
                crate::providers::SystemDrift {
                    key: "net.ipv4.ip_forward".to_string(),
                    expected: "1".to_string(),
                    actual: "0".to_string(),
                },
            ])
        }
        fn apply(
            &self,
            _: &serde_yaml::Value,
            _: &crate::providers::SystemContext<'_>,
        ) -> crate::errors::Result<()> {
            Ok(())
        }
    }

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(DriftingConfigurator));

    let mut system = BTreeMap::new();
    system.insert(
        "sysctl".to_string(),
        serde_yaml::to_value(serde_yaml::Mapping::new()).unwrap(),
    );
    let merged = crate::config::MergedProfile {
        system,
        ..Default::default()
    };
    let resolved = crate::config::ResolvedProfile {
        layers: vec![crate::config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "default".to_string(),
            priority: 0,
            policy: crate::config::LayerPolicy::Local,
            spec: Default::default(),
        }],
        merged,
    };

    let printer = test_printer();
    let results = verify(
        &resolved,
        &registry,
        &state,
        &[],
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    // Should have per-key drift entries with resource_type "system"
    let drift_results: Vec<_> = results
        .iter()
        .filter(|r| r.resource_type == "system" && !r.matches)
        .collect();
    assert_eq!(
        drift_results.len(),
        2,
        "should report drift for each sysctl key, got: {:?}",
        drift_results
    );
    assert!(
        drift_results
            .iter()
            .any(|r| r.resource_id == "sysctl.vm.swappiness"),
        "should report sysctl.vm.swappiness drift"
    );
    assert!(
        drift_results
            .iter()
            .any(|r| r.resource_id == "sysctl.net.ipv4.ip_forward"),
        "should report sysctl.net.ipv4.ip_forward drift"
    );
    // Verify the expected/actual values are correct
    let swap = drift_results
        .iter()
        .find(|r| r.resource_id == "sysctl.vm.swappiness")
        .unwrap();
    assert_eq!(swap.expected, "10");
    assert_eq!(swap.actual, "60");
}

#[test]
fn verify_system_configurator_reports_healthy_when_no_drift() {
    struct HealthyConfigurator;

    impl crate::providers::SystemConfigurator for HealthyConfigurator {
        fn name(&self) -> &str {
            "sysctl"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn current_state(&self) -> crate::errors::Result<serde_yaml::Value> {
            Ok(serde_yaml::Value::Null)
        }
        fn diff(
            &self,
            _: &serde_yaml::Value,
        ) -> crate::errors::Result<Vec<crate::providers::SystemDrift>> {
            Ok(vec![])
        }
        fn apply(
            &self,
            _: &serde_yaml::Value,
            _: &crate::providers::SystemContext<'_>,
        ) -> crate::errors::Result<()> {
            Ok(())
        }
    }

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(HealthyConfigurator));

    let mut system = BTreeMap::new();
    system.insert(
        "sysctl".to_string(),
        serde_yaml::to_value(serde_yaml::Mapping::new()).unwrap(),
    );
    let merged = crate::config::MergedProfile {
        system,
        ..Default::default()
    };
    let resolved = crate::config::ResolvedProfile {
        layers: vec![crate::config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "default".to_string(),
            priority: 0,
            policy: crate::config::LayerPolicy::Local,
            spec: Default::default(),
        }],
        merged,
    };

    let printer = test_printer();
    let results = verify(
        &resolved,
        &registry,
        &state,
        &[],
        &crate::providers::PackageContext::new(&printer, &state),
        true,
    )
    .unwrap()
    .results;

    let sysctl_results: Vec<_> = results
        .iter()
        .filter(|r| r.resource_type == "system")
        .collect();
    assert_eq!(
        sysctl_results.len(),
        1,
        "should have one healthy result for sysctl"
    );
    assert!(
        sysctl_results[0].matches,
        "sysctl should report as matching (no drift)"
    );
    assert_eq!(sysctl_results[0].resource_id, "sysctl");
}

// `Reconciler::apply` streams one status line per action inside its phase/owner
// tree. Callers that want a buffered summary (e.g. `cmd_apply`'s `ApplyOutput`)
// emit a `Doc` on the same `Printer` right after.
// The renderer's blank-line accounting must produce exactly one blank line
// between the last streaming line and the buffered Doc's first visible line —
// zero blanks would let the spinner's tail bleed into the summary; two would
// leave a visual gap.

mod bridge {
    use super::super::Reconciler;
    use super::*;
    use crate::output::test_capture::{assert_snapshot_at, strip_ansi};
    // Only the mixed-apply fixture strips a wall-clock duration, and that
    // fixture is Unix-only.
    #[cfg(unix)]
    use crate::output::test_capture::strip_spinner_duration;
    use crate::output::{Doc, Printer, Role};

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/reconciler/snapshots")
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    /// Build `ApplyOutput`-shaped payload locally (cfgd-core can't depend on
    /// `cfgd`'s `cli::apply::ApplyOutput`). The payload exists only to give
    /// the buffered Doc a JSON shape; the bridge invariant cares about the
    /// human render, which `with_data` does not contribute to.
    #[derive(serde::Serialize)]
    struct ApplySummary {
        total: usize,
        succeeded: usize,
        failed: usize,
    }

    /// Minimal seam fixture: one streaming status line + one buffered Doc.
    /// Does NOT drive the reconciler — keeps `bridge.txt` distinct from
    /// (and far smaller than) the cycle goldens so a regression in the seam
    /// itself tips a fixture that has nothing else moving.
    #[cfg(unix)]
    fn run_minimal_bridge() -> String {
        let (printer, cap) = Printer::for_test_doc();
        printer.status_simple(Role::Ok, "write /etc/hosts");
        let doc = Doc::new().status(Role::Ok, "Apply complete");
        printer.emit(doc);
        drop(printer);
        strip_ansi(&cap.human())
    }

    /// Drive a mixed-result apply (1 ok package + 1 continueOnError-warning
    /// post-script + 1 hard package failure), then emit a buffered Doc
    /// carrying the summary. Returns the captured human surface
    /// (ANSI-stripped).
    #[cfg(unix)]
    fn run_mixed_apply_then_emit_summary() -> String {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));
        registry.add_package_manager(Box::new(FailingPackageManager::new("apt")));

        let reconciler = Reconciler::new(&registry, &state);
        let mut resolved = make_empty_resolved();

        // Post-apply script that fails with continueOnError=true → exercises
        // the Role::Warn branch in apply.rs (continueOnError-warning).
        resolved.merged.scripts.post_apply = vec![ScriptEntry::Full(ScriptCommand {
            workdir: None,
            run: "exit 42".to_string(),
            timeout: Some("5s".to_string()),
            idle_timeout: None,
            continue_on_error: Some(true),
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        })];

        let pkg_actions = vec![
            PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["jq".to_string()],
                origin: "local".to_string(),
            },
            PackageAction::Install {
                manager: "apt".to_string(),
                packages: vec!["curl".to_string()],
                origin: "local".to_string(),
            },
        ];
        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let (printer, cap) = Printer::for_test_doc();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                std::path::Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
                None,
                &crate::AbortFlag::new(),
            )
            .unwrap();

        // Buffered summary on the SAME printer — this is the seam under test.
        let summary = ApplySummary {
            total: result.action_results.len(),
            succeeded: result.succeeded(),
            failed: result.failed(),
        };
        let doc = Doc::new()
            .status(Role::Warn, "Apply partial")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        // The failed script's finish line carries a real wall-clock duration
        // (`window.finish_fail(...).duration(start.elapsed())`), which varies
        // run to run — strip it so the golden is host- and timing-stable.
        strip_spinner_duration(strip_ansi(&cap.human()))
    }

    /// Drive a clean apply with an EMPTY plan — no actions, no per-action
    /// streaming lines, just the buffered "0 changes" summary Doc. Captures
    /// the "no preceding content" branch of the renderer's blank-line
    /// accounting.
    fn run_clean_apply_then_emit_summary() -> String {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        // Empty plan — no actions in any phase.
        let plan = crate::reconciler::Plan {
            phases: Vec::new(),
            warnings: Vec::new(),
        };

        let (printer, cap) = Printer::for_test_doc();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                std::path::Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
                None,
                &crate::AbortFlag::new(),
            )
            .unwrap();

        let summary = ApplySummary {
            total: result.action_results.len(),
            succeeded: result.succeeded(),
            failed: result.failed(),
        };
        let doc = Doc::new()
            .status(Role::Ok, "Apply complete — 0 changes")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        strip_ansi(&cap.human())
    }

    #[test]
    #[cfg(unix)]
    fn bridge_invariant_apply_cycle() {
        let captured = run_minimal_bridge();

        // Exactly one blank line at the streaming → buffered seam.
        assert!(
            captured.contains("\n\n"),
            "bridge missing blank line:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "bridge has duplicate blank line:\n{captured}"
        );

        // Lock the human shape so a renderer regression in blank-line
        // accounting tips both the structural assertion AND the golden.
        assert_snapshot("bridge.txt", &captured);

        // Mixed-apply seam — same invariant: exactly one blank line between
        // the last streaming line and the buffered Doc's first visible line,
        // even when the preceding streaming sequence includes a spinner
        // finish_fail → continueOnError Warn pair.
        let mixed = run_mixed_apply_then_emit_summary();
        assert!(
            mixed.contains("\n\n"),
            "mixed-apply-cycle missing blank line at seam:\n{mixed}"
        );
        assert!(
            !mixed.contains("\n\n\n"),
            "mixed-apply-cycle has duplicate blank line:\n{mixed}"
        );

        // Clean-cycle seam — no preceding streaming lines, only the buffered
        // Doc. Guards against the renderer emitting a leading blank when no
        // prior emission has happened.
        let clean = run_clean_apply_then_emit_summary();
        assert!(
            !clean.contains("\n\n\n"),
            "clean-apply-cycle has duplicate blank line:\n{clean}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_mixed_apply_cycle() {
        let captured = run_mixed_apply_then_emit_summary();
        assert_snapshot("mixed_apply_cycle.txt", &captured);
    }

    #[test]
    fn snapshot_clean_apply_cycle() {
        let captured = run_clean_apply_then_emit_summary();
        assert_snapshot("clean_apply_cycle.txt", &captured);
    }
}

// ---------------------------------------------------------------------------
// format_plan_items — uncovered branches
// ---------------------------------------------------------------------------

#[test]
fn format_plan_items_file_skip() {
    let phase = Phase::from_actions(
        PhaseName::Files,
        &Owner::profile("test"),
        vec![Action::File(FileAction::Skip {
            target: PathBuf::from("/home/user/.config/skipped"),
            reason: "unchanged".into(),
            origin: "corp".into(),
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("skip"), "got: {}", items[0]);
    assert!(items[0].contains("unchanged"), "got: {}", items[0]);
    assert!(items[0].contains("<- corp"), "got: {}", items[0]);
}

#[test]
fn format_plan_items_file_set_permissions() {
    let phase = Phase::from_actions(
        PhaseName::Files,
        &Owner::profile("test"),
        vec![Action::File(FileAction::SetPermissions {
            target: PathBuf::from("/home/user/.ssh/id_rsa"),
            mode: 0o600,
            origin: "local".into(),
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("chmod"), "got: {}", items[0]);
    assert!(items[0].contains("0o600"), "got: {}", items[0]);
    assert!(items[0].contains("id_rsa"), "got: {}", items[0]);
}

#[test]
fn format_plan_items_manager_provision() {
    let phase = Phase::from_actions(
        PhaseName::Prerequisites,
        &Owner::profile("test"),
        vec![Action::Manager(ManagerAction::Provision {
            manager: "brew".into(),
            via: "curl | bash".into(),
            declared: None,
            batched: vec![],
            depends_on: vec![],
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("provision brew"), "got: {}", items[0]);
    assert!(items[0].contains("curl | bash"), "got: {}", items[0]);
}

#[test]
fn format_plan_items_package_uninstall() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("test"),
        vec![Action::Package(PackageAction::Uninstall {
            manager: "apt".into(),
            packages: vec!["vim".into(), "nano".into()],
            origin: "local".into(),
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("uninstall"), "got: {}", items[0]);
    assert!(items[0].contains("vim"), "got: {}", items[0]);
    assert!(items[0].contains("nano"), "got: {}", items[0]);
}

#[test]
fn format_module_action_item_run_script() {
    let phase = Phase::from_actions(
        PhaseName::PostScripts,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction {
            module_name: "nvim".into(),
            kind: ModuleActionKind::RunScript {
                script: ScriptEntry::Simple("make install".into()),
                phase: ScriptPhase::PostApply,
            },
            origin: None,
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].starts_with("postApply:"), "got: {}", items[0]);
    assert!(items[0].contains("make install"), "got: {}", items[0]);
}

#[test]
fn format_module_action_item_source_delivered_shows_origin_suffix() {
    // A source-delivered module (origin = Some) gets the same ` <- <source>`
    // provenance suffix as source-delivered files/packages.
    let phase = Phase::from_actions(
        PhaseName::Files,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction::with_origin(
            "nvim",
            {
                let files = vec![ResolvedFile {
                    source: PathBuf::from("/cache/nvim/config"),
                    target: PathBuf::from("/home/user/.config/nvim"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                }];
                let declared_total = files.len();
                ModuleActionKind::DeployFiles {
                    files,
                    declared_total,
                }
            },
            Some("acme".to_string()),
        ))],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].starts_with("deploy "), "got: {}", items[0]);
    assert!(items[0].ends_with(" <- acme"), "got: {}", items[0]);
}

#[test]
fn format_module_action_item_local_has_no_origin_suffix() {
    // A consumer-local module (origin = None) renders with no provenance suffix,
    // exactly as before — regression guard for local modules.
    let phase = Phase::from_actions(
        PhaseName::Files,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction::local("nvim", {
            let files = vec![ResolvedFile {
                source: PathBuf::from("/cache/nvim/config"),
                target: PathBuf::from("/home/user/.config/nvim"),
                is_git_source: false,
                strategy: None,
                encryption: None,
                permissions: None,
                patch: None,
            }];
            let declared_total = files.len();
            ModuleActionKind::DeployFiles {
                files,
                declared_total,
            }
        }))],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(!items[0].contains(" <- "), "got: {}", items[0]);
}

#[test]
fn format_module_action_item_deploy_many_files_names_them_all() {
    let files: Vec<ResolvedFile> = (0..5)
        .map(|i| ResolvedFile {
            source: PathBuf::from(format!("/cache/mod/f{i}")),
            target: PathBuf::from(format!("/home/user/.f{i}")),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        })
        .collect();
    let phase = Phase::from_actions(
        PhaseName::Files,
        &Owner::profile("test"),
        vec![Action::Module(ModuleAction {
            module_name: "big".into(),
            kind: ModuleActionKind::DeployFiles {
                declared_total: files.len(),
                files,
            },
            origin: None,
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(
        (0..5).all(|i| items[0].contains(&format!("/home/user/.f{i}")))
            && !items[0].contains("files"),
        "every target named, and the count left to the detail, got: {}",
        items[0]
    );
    let details: Vec<Option<String>> = phase
        .actions()
        .map(|a| super::action_produced_detail(a, None, 0, &[]))
        .collect();
    assert_eq!(
        details,
        vec![None],
        "a full deploy names every target; no detail restates the total"
    );
}

#[test]
fn format_action_description_module_alias_canonical_mismatch() {
    let action = Action::Module(ModuleAction {
        module_name: "tools".into(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![ResolvedPackage {
                canonical_name: "fd".into(),
                resolved_name: "fd-find".into(),
                manager: "apt".into(),
                manager_declared: false,
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            }],
        },
        origin: None,
    });
    let desc = format_action_description(&action);
    assert!(
        desc.contains("module:tools:packages:fd-find"),
        "got: {desc}"
    );
}

#[test]
fn format_action_description_file_skip_action() {
    let action = Action::File(FileAction::Skip {
        target: PathBuf::from("/home/user/.old"),
        reason: "unchanged".into(),
        origin: "local".into(),
    });
    let desc = format_action_description(&action);
    assert!(desc.starts_with("file:skip:"), "got: {desc}");
}

// ---------------------------------------------------------------------------
// FileAction::clone_action
// ---------------------------------------------------------------------------

#[test]
fn clone_action_create_preserves_all_fields() {
    let action = FileAction::Create {
        source: PathBuf::from("/src/file"),
        target: PathBuf::from("/dst/file"),
        origin: "remote".into(),
        strategy: crate::config::FileStrategy::Symlink,
        source_hash: Some("abc123".into()),
        patch: Some(crate::config::PatchSpec {
            format: Some(crate::config::PatchFormat::Ini),
            ensure: None,
            script: Some("rewrite.sh".into()),
            blocked_by: None,
        }),
    };
    let cloned = action.clone_action();
    match cloned {
        FileAction::Create {
            source,
            target,
            origin,
            strategy,
            source_hash,
            patch,
        } => {
            assert_eq!(source, PathBuf::from("/src/file"));
            assert_eq!(target, PathBuf::from("/dst/file"));
            assert_eq!(origin, "remote");
            assert_eq!(strategy, crate::config::FileStrategy::Symlink);
            assert_eq!(source_hash.as_deref(), Some("abc123"));
            let patch = patch.expect("patch block must survive the clone");
            assert_eq!(patch.format, Some(crate::config::PatchFormat::Ini));
            assert_eq!(patch.script.as_deref(), Some("rewrite.sh"));
        }
        other => panic!("expected Create, got: {other:?}"),
    }
}

#[test]
fn clone_action_update_preserves_all_fields() {
    let action = FileAction::Update {
        source: PathBuf::from("/src/updated"),
        target: PathBuf::from("/dst/updated"),
        diff: "- old\n+ new".into(),
        origin: "corp".into(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    };
    let cloned = action.clone_action();
    match cloned {
        FileAction::Update {
            source,
            target,
            diff,
            origin,
            strategy,
            source_hash,
            patch,
        } => {
            assert_eq!(patch, None);
            assert_eq!(source, PathBuf::from("/src/updated"));
            assert_eq!(target, PathBuf::from("/dst/updated"));
            assert_eq!(diff, "- old\n+ new");
            assert_eq!(origin, "corp");
            assert_eq!(strategy, crate::config::FileStrategy::Copy);
            assert!(source_hash.is_none());
        }
        other => panic!("expected Update, got: {other:?}"),
    }
}

#[test]
fn clone_action_delete_preserves_all_fields() {
    let action = FileAction::Delete {
        target: PathBuf::from("/home/user/.old"),
        origin: "local".into(),
    };
    let cloned = action.clone_action();
    match cloned {
        FileAction::Delete { target, origin } => {
            assert_eq!(target, PathBuf::from("/home/user/.old"));
            assert_eq!(origin, "local");
        }
        other => panic!("expected Delete, got: {other:?}"),
    }
}

#[test]
fn clone_action_set_permissions_preserves_all_fields() {
    let action = FileAction::SetPermissions {
        target: PathBuf::from("/home/user/.ssh/key"),
        mode: 0o600,
        origin: "local".into(),
    };
    let cloned = action.clone_action();
    match cloned {
        FileAction::SetPermissions {
            target,
            mode,
            origin,
        } => {
            assert_eq!(target, PathBuf::from("/home/user/.ssh/key"));
            assert_eq!(mode, 0o600);
            assert_eq!(origin, "local");
        }
        other => panic!("expected SetPermissions, got: {other:?}"),
    }
}

#[test]
fn clone_action_skip_preserves_all_fields() {
    let action = FileAction::Skip {
        target: PathBuf::from("/home/user/.config/skipped"),
        reason: "unchanged".into(),
        origin: "corp".into(),
    };
    let cloned = action.clone_action();
    match cloned {
        FileAction::Skip {
            target,
            reason,
            origin,
        } => {
            assert_eq!(target, PathBuf::from("/home/user/.config/skipped"));
            assert_eq!(reason, "unchanged");
            assert_eq!(origin, "corp");
        }
        other => panic!("expected Skip, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// apply_file_action_direct — filesystem operations with tempdir
// ---------------------------------------------------------------------------

#[test]
fn apply_file_action_direct_creates_file_with_copy() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.txt");
    std::fs::write(&src, "hello").unwrap();
    let dst = dir.path().join("sub/target.txt");

    let action = FileAction::Create {
        source: src,
        target: dst.clone(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "hello");
}

#[test]
fn apply_file_action_direct_patch_merges_into_existing_target() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("settings.json");
    std::fs::write(&dst, "{\n  \"keep\": 1\n}\n").unwrap();

    let action = FileAction::Update {
        source: PathBuf::new(),
        target: dst.clone(),
        diff: String::new(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Patch,
        source_hash: None,
        patch: Some(crate::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str("added: true").unwrap()),
            script: None,
            blocked_by: None,
        }),
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();

    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dst).unwrap()).unwrap();
    assert_eq!(written["keep"], 1, "unmentioned keys must survive");
    assert_eq!(written["added"], true);
}

#[test]
#[cfg(unix)]
fn apply_file_action_direct_patch_preserves_the_targets_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("settings.json");
    std::fs::write(&dst, "{\n  \"keep\": 1\n}\n").unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o644)).unwrap();

    let action = FileAction::Update {
        source: PathBuf::new(),
        target: dst.clone(),
        diff: String::new(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Patch,
        source_hash: None,
        patch: Some(crate::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str("added: true").unwrap()),
            script: None,
            blocked_by: None,
        }),
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();

    assert_eq!(
        std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
        0o644,
        "the target's mode must survive the merge"
    );
}

#[test]
#[cfg(unix)]
fn apply_file_action_direct_patch_writes_through_a_symlinked_target() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("repo").join("settings.json");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "{\n  \"keep\": 1\n}\n").unwrap();
    let dst = dir.path().join("settings.json");
    crate::create_symlink(&real, &dst).unwrap();

    let action = FileAction::Update {
        source: PathBuf::new(),
        target: dst.clone(),
        diff: String::new(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Patch,
        source_hash: None,
        patch: Some(crate::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str("added: true").unwrap()),
            script: None,
            blocked_by: None,
        }),
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();

    assert!(dst.is_symlink(), "the symlink must survive the merge");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
    assert_eq!(written["keep"], 1);
    assert_eq!(
        written["added"], true,
        "the merge must land in the file the link points at"
    );
}

#[test]
fn apply_file_action_direct_patch_without_block_errors_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("target.json");

    let action = FileAction::Create {
        source: PathBuf::new(),
        target: dst.clone(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Patch,
        source_hash: None,
        patch: None,
    };
    let err = super::file_action::apply_file_action_direct(&action, dir.path(), "test")
        .expect_err("a Patch action without a patch block must not be applied");
    assert!(
        matches!(
            err,
            crate::errors::CfgdError::File(crate::errors::FileError::PatchBlockMissing { .. })
        ),
        "expected FileError::PatchBlockMissing, got: {err:?}"
    );
    assert!(!dst.exists(), "target must not be created");
}

#[test]
fn apply_file_action_direct_creates_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.txt");
    std::fs::write(&src, "link-target").unwrap();
    let dst = dir.path().join("link.txt");

    let action = FileAction::Create {
        source: src,
        target: dst.clone(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Symlink,
        source_hash: None,
        patch: None,
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();
    assert!(dst.is_symlink());
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "link-target");
}

#[test]
fn apply_file_action_direct_creates_hardlink() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.txt");
    std::fs::write(&src, "hard-data").unwrap();
    let dst = dir.path().join("hard.txt");

    let action = FileAction::Create {
        source: src,
        target: dst.clone(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Hardlink,
        source_hash: None,
        patch: None,
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "hard-data");
}

#[test]
fn apply_file_action_direct_deletes_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("doomed.txt");
    std::fs::write(&target, "bye").unwrap();
    assert!(target.exists());

    let action = FileAction::Delete {
        target: target.clone(),
        origin: "local".into(),
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();
    assert!(!target.exists());
}

#[test]
fn apply_file_action_direct_delete_nonexistent_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nonexistent.txt");

    let action = FileAction::Delete {
        target,
        origin: "local".into(),
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();
}

#[test]
fn apply_file_action_direct_skip_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let action = FileAction::Skip {
        target: dir.path().join("whatever.txt"),
        reason: "unchanged".into(),
        origin: "local".into(),
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();
}

#[test]
fn apply_file_action_direct_update_replaces_existing() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new-version.txt");
    std::fs::write(&src, "v2").unwrap();
    let dst = dir.path().join("target.txt");
    std::fs::write(&dst, "v1").unwrap();

    let action = FileAction::Update {
        source: src,
        target: dst.clone(),
        diff: String::new(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    };
    super::file_action::apply_file_action_direct(&action, dir.path(), "test").unwrap();
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "v2");
}

// -----------------------------------------------------------------------
// apply_module_action: additional uncovered branches
// (script-based package install, bootstrap, manager-missing,
// DeployFiles with no parent, RunScript with no module dir)
// -----------------------------------------------------------------------

/// A package manager that reports unavailable, carries a bootstrap plan,
/// and emits path_dirs so the planner's PATH-entry branch fires.
struct BootstrappingPackageManager {
    name: String,
    available: std::sync::Mutex<bool>,
    bootstrap_called: std::sync::Mutex<bool>,
    install_calls: std::sync::Mutex<Vec<Vec<String>>>,
    path_dirs_after: Vec<String>,
    bootstrap_creates: Vec<String>,
    install_creates: Vec<String>,
    path_dirs_per_method: bool,
    install_fails: bool,
}

impl BootstrappingPackageManager {
    fn new(name: &str, path_dirs: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            available: std::sync::Mutex::new(false),
            bootstrap_called: std::sync::Mutex::new(false),
            install_calls: std::sync::Mutex::new(Vec::new()),
            path_dirs_after: path_dirs.iter().map(|s| s.to_string()).collect(),
            bootstrap_creates: Vec::new(),
            install_creates: Vec::new(),
            path_dirs_per_method: false,
            install_fails: false,
        }
    }

    /// The user-installed shape: available before this run does anything, so
    /// the planner has no reason to provision it and nothing records its
    /// directories at bootstrap time.
    fn already_available(self) -> Self {
        *self.available.lock().unwrap() = true;
        self
    }

    /// Report `dirs` as directories this manager's `install()` created — npm's
    /// `~/.npm-global` shape, where the prefix is cfgd's own doing rather than
    /// the installer's.
    fn creating_on_install(mut self, dirs: &[&str]) -> Self {
        self.install_creates = dirs.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Fail every `install()`. The prefix a real manager makes is created
    /// before the packages are fetched, so the failure arrives with the
    /// directory already on disk.
    fn failing_install(mut self) -> Self {
        self.install_fails = true;
        self
    }

    /// Answer `path_dirs` from the method the plan chose — pipx's shape, where
    /// the directories depend on which mediator installed the manager. With no
    /// planned method in the context it answers `PROBED_PATH_DIR` instead,
    /// standing in for the live re-probe whose answer has already moved by the
    /// time the record is written.
    fn path_dirs_per_method(mut self) -> Self {
        self.path_dirs_per_method = true;
        self
    }

    /// Also declare `dirs` on the `BootstrapPlan` itself — the population
    /// `fold_provision_path_dirs` reads at plan time, before any bootstrap
    /// has run. Kept separate from `path_dirs_after` (what a real bootstrap
    /// would later record) so a test can exercise the two independently.
    fn declaring_path_dirs(mut self, dirs: &[&str]) -> Self {
        self.bootstrap_creates = dirs.iter().map(|s| s.to_string()).collect();
        self
    }
}

impl PackageManager for BootstrappingPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        *self.available.lock().unwrap()
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        Some(crate::providers::BootstrapPlan::new("stub").creating(self.bootstrap_creates.clone()))
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        *self.bootstrap_called.lock().unwrap() = true;
        *self.available.lock().unwrap() = true;
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        self.install_calls.lock().unwrap().push(packages.to_vec());
        if self.install_fails {
            return Err(crate::errors::PackageError::InstallFailed {
                manager: self.name.clone(),
                message: format!("stub failure installing {}", packages.join(",")),
            }
            .into());
        }
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn path_dirs(&self, cx: &PackageContext<'_>) -> Vec<String> {
        if self.path_dirs_per_method {
            return match cx.planned_method() {
                Some(via) => vec![format!("/opt/{via}/bin")],
                None => vec![PROBED_PATH_DIR.to_string()],
            };
        }
        self.path_dirs_after.clone()
    }
    fn created_path_dirs(&self, _: &PackageContext<'_>) -> Vec<String> {
        self.install_creates.clone()
    }
}

/// What a live re-probe would answer at record time, once the bootstrap it is
/// supposed to describe has already changed the machine.
const PROBED_PATH_DIR: &str = "/opt/probed-after-the-fact/bin";

/// A one-provision plan for `manager`, naming `via` as the method the planner
/// chose — the string the action line the user read says.
fn provision_only_plan(manager: &str, via: &str) -> Plan {
    Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("work"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: manager.to_string(),
                via: via.to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    }
}

/// A one-install plan for `manager`, with no `Provision` node anywhere in it.
fn install_only_plan(manager: &str, package: &str) -> Plan {
    Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("work"),
            vec![Action::Package(PackageAction::Install {
                manager: manager.to_string(),
                packages: vec![package.to_string()],
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    }
}

/// A directory cfgd created earns an env entry however the manager got onto
/// the machine: this manager is available from the start, so nothing
/// provisions it and no bootstrap records anything — yet the prefix its
/// `install()` made still has to reach the recorded directories the generated
/// env file is built from.
#[test]
#[serial_test::serial]
fn an_install_records_the_directories_it_created_with_no_provision_in_the_run() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());
    // The apply registers the created directory into the process-global
    // registry, which production never clears.
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("npm", &[])
            .already_available()
            .creating_on_install(&["/home/u/.npm-global/bin"]),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    reconciler
        .apply(
            &install_only_plan("npm", "typescript"),
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(
        state.bootstrapped_managers().unwrap(),
        vec![(
            "npm".to_string(),
            vec!["/home/u/.npm-global/bin".to_string()]
        )],
    );
}

/// The install-time PATH gap this closes: a manager already available before
/// this run does anything (baked into an image, or bootstrapped on a prior
/// run) never bootstraps, so `path_dirs()` never reaches
/// `record_bootstrap`. Its `created_path_dirs()` also answers empty (this
/// manager creates nothing of its own — the brew shape), so nothing about it
/// is cfgd's to persist either. Yet the very next action in this same run
/// needs to resolve a binary this install just landed in that directory, so
/// `register_install_path_dirs` registers it into the PROCESS-level registry
/// regardless — proven by reading `bootstrapped_path_dirs()` directly rather
/// than the state store, and proven NOT persisted by asserting the state
/// store recorded nothing for this manager at all.
#[test]
#[serial_test::serial]
fn an_install_with_no_bootstrap_and_nothing_created_still_registers_its_path_dirs_in_process() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("brew-like", &["/opt/brew-like/bin"]).already_available(),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    reconciler
        .apply(
            &install_only_plan("brew-like", "some-formula"),
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert!(
        crate::bootstrapped_path_dirs()
            .iter()
            .any(|d| d.to_string_lossy() == "/opt/brew-like/bin"),
        "the install-time directory must be resolvable to the rest of this process"
    );
    assert!(
        state.bootstrapped_managers().unwrap().is_empty(),
        "a manager that created nothing must earn no persisted row — its \
         directory is not cfgd's to publish into the generated env file"
    );
}

/// The record a provision writes comes from the method the plan named, so a
/// manager whose directories depend on its mediator records what the plan
/// promised. The bootstrap itself changes what a live probe sees, so a record
/// derived by re-probing would disagree with the plan-time answer and the env
/// file would never converge.
#[test]
#[serial_test::serial]
fn a_provision_records_the_directories_the_planned_method_names() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("pipx", &[]).path_dirs_per_method(),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    reconciler
        .apply(
            &provision_only_plan("pipx", "pip"),
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(
        state.bootstrapped_managers().unwrap(),
        vec![("pipx".to_string(), vec!["/opt/pip/bin".to_string()])],
        "the recorded directories must come from the plan's method, not from a \
         re-probe at record time"
    );
}

/// The directory is on disk whether or not the install that followed it
/// succeeded, so it is recorded either way. Leaving a created directory
/// unrecorded is the state where a binary a later run installs lands somewhere
/// no login shell reads; a record for a directory holding nothing yet costs a
/// PATH entry and nothing else.
#[test]
#[serial_test::serial]
fn a_failed_install_still_records_the_directory_it_had_already_created() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("npm", &[])
            .already_available()
            .creating_on_install(&["/home/u/.npm-global/bin"])
            .failing_install(),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    let result = reconciler
        .apply(
            &install_only_plan("npm", "typescript"),
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(
        state.bootstrapped_managers().unwrap(),
        vec![(
            "npm".to_string(),
            vec!["/home/u/.npm-global/bin".to_string()]
        )],
    );
}

/// A manager whose install creates one prefix while its bootstrap declared
/// several keeps them all: the created directory is ADDED to the record, so a
/// narrower answer cannot cost a provision's other directories their place in
/// the generated env file.
#[test]
#[serial_test::serial]
fn an_install_that_created_one_directory_keeps_the_rest_of_the_recorded_row() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
    let state = test_state();
    record_brew_bootstrap(&state);
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("brew", &BREW_PATH_DIRS)
            .already_available()
            .creating_on_install(&[BREW_PATH_DIRS[0]]),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    reconciler
        .apply(
            &install_only_plan("brew", "ripgrep"),
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(
        state.bootstrapped_managers().unwrap(),
        vec![(
            "brew".to_string(),
            BREW_PATH_DIRS.iter().map(|d| d.to_string()).collect()
        )],
        "a created directory adds to the row and never replaces it"
    );
}

/// A manager that created nothing queues no record at all, so an ordinary
/// install costs no write and an earlier provision's directories are read back
/// exactly as recorded.
#[test]
#[serial_test::serial]
fn an_install_that_creates_nothing_leaves_an_earlier_provisions_record_intact() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());
    let state = test_state();
    record_brew_bootstrap(&state);
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("brew", &BREW_PATH_DIRS).already_available(),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();

    reconciler
        .apply(
            &install_only_plan("brew", "ripgrep"),
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(
        state.bootstrapped_managers().unwrap(),
        vec![(
            "brew".to_string(),
            BREW_PATH_DIRS.iter().map(|d| d.to_string()).collect()
        )],
    );
}

/// Build the single-module fixture both out-of-band-write tests drive:
/// one `brew` package, and the `InstallPackages` action the Modules phase
/// would run for it.
fn brew_install_fixture() -> (Vec<ResolvedModule>, ModuleAction) {
    let package = ResolvedPackage {
        canonical_name: "ripgrep".to_string(),
        resolved_name: "ripgrep".to_string(),
        manager: "brew".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![package.clone()],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];
    let action = ModuleAction {
        module_name: "tools".to_string(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![package],
        },
        origin: None,
    };
    (modules, action)
}

/// Run one Modules-phase action against a registry holding a bootstrappable
/// `brew` contributing `path_dirs`, and return the state store it recorded to.
fn run_brew_module_action(path_dirs: &[&str]) -> crate::state::StateStore {
    // The bootstrap below registers `path_dirs` into the process-global
    // resolution registry, which production never clears. Without this the real
    // host directories named here stay resolvable for every later test in the
    // binary.
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(BootstrappingPackageManager::new(
        "brew", path_dirs,
    )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let (modules, action) = brew_install_fixture();
    let printer = test_printer();

    let run = reconciler
        .apply_module_action(
            &action,
            Path::new("."),
            &printer,
            1,
            ReconcileContext::Apply,
            &resolved,
            &modules,
            None,
            &crate::AbortFlag::new(),
            &crate::providers::NoteSink::default(),
            &mut Vec::new(),
        )
        .expect("module action must succeed");
    assert!(
        run.changed,
        "a manager-backed install counts as changed: {}",
        run.description
    );
    state
}

#[test]
#[serial_test::serial]
fn apply_module_install_packages_bootstraps_without_writing_env_out_of_band() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());
    // The provision below registers `path_dirs` into the process-global
    // resolution registry, which production never clears. Without this the
    // real host directories named here stay resolvable for every later test
    // in the binary.
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(BootstrappingPackageManager::new(
        "brew",
        &["/opt/homebrew/bin", "/opt/homebrew/sbin"],
    )));

    let plan = prerequisites_phase(vec![provision_node("brew", "stub", &[])]);
    let (result, _text) = apply_manager_plan(&registry, &state, &plan);
    assert_eq!(result.status, ApplyStatus::Success);

    // The generated env file has exactly one writer — the Env phase. A
    // Prerequisites-phase provision that writes it out of band would be
    // erased by the next plan's wholesale rewrite, so the bootstrapped PATH
    // would vanish on the second apply.
    let env_path = tmp_home.path().join(".cfgd.env");
    assert!(
        !env_path.exists(),
        "the Prerequisites phase must not write {}",
        env_path.posix()
    );

    // The directories went to the state store instead, where the Env phase —
    // this run's post-phase regeneration and every later plan — reads them.
    assert_eq!(
        state.bootstrapped_managers().unwrap(),
        vec![(
            "brew".to_string(),
            vec![
                "/opt/homebrew/bin".to_string(),
                "/opt/homebrew/sbin".to_string()
            ]
        )],
        "a successful provision must record the manager's PATH directories in order"
    );
}

#[test]
#[serial_test::serial]
fn apply_module_install_packages_leaves_existing_env_file_untouched() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    // Pre-create .cfgd.env with one path entry already present.
    std::fs::write(
        tmp_home.path().join(".cfgd.env"),
        "export PATH=\"/usr/local/bin:$PATH\"\n",
    )
    .unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    // One dir already exists in env; one is new.
    run_brew_module_action(&["/usr/local/bin", "/opt/homebrew/bin"]);

    let contents = std::fs::read_to_string(tmp_home.path().join(".cfgd.env")).unwrap();
    assert_eq!(
        contents, "export PATH=\"/usr/local/bin:$PATH\"\n",
        "the Modules phase must leave the Env phase's file byte-identical: {contents}"
    );
}

/// Registry holding one bootstrappable manager contributing two PATH entries.
///
/// The registry alone contributes nothing to the planned env file: planning
/// reads the state store's bootstrap record, never the manager's live
/// `path_dirs()` probe. Pair with `record_brew_bootstrap` to give the planner
/// something to work from.
fn registry_with_bootstrappable_brew() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(BootstrappingPackageManager::new(
        "brew",
        &[
            "/home/linuxbrew/.linuxbrew/bin",
            "/home/linuxbrew/.linuxbrew/sbin",
        ],
    )));
    registry
}

/// The PATH directories a linuxbrew bootstrap contributes, in the order the
/// generated env file must export them.
const BREW_PATH_DIRS: [&str; 2] = [
    "/home/linuxbrew/.linuxbrew/bin",
    "/home/linuxbrew/.linuxbrew/sbin",
];

/// Seed the state store as if cfgd had bootstrapped brew on this machine.
fn record_brew_bootstrap(state: &crate::state::StateStore) {
    let dirs: Vec<String> = BREW_PATH_DIRS.iter().map(|d| d.to_string()).collect();
    state
        .record_bootstrapped_path_dirs("brew", &dirs)
        .expect("record bootstrap path dirs");
}

/// Body of the `.cfgd.env` write the Env phase planned, if any.
fn planned_env_file_content(plan: &Plan) -> Option<String> {
    plan.phases
        .iter()
        .find(|p| p.name == PhaseName::Prerequisites)?
        .actions()
        .find_map(|a| match a {
            Action::Env(EnvAction::WriteEnvFile { path, content, .. })
                if path.file_name() == Some(std::ffi::OsStr::new(".cfgd.env")) =>
            {
                Some(content.clone())
            }
            _ => None,
        })
}

#[test]
#[serial_test::serial]
fn plan_env_carries_bootstrap_path_dirs_on_every_plan() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    let state = test_state();
    record_brew_bootstrap(&state);
    let registry = registry_with_bootstrappable_brew();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let modules = vec![make_resolved_module("tools")];

    let plan_content = |m: Vec<ResolvedModule>| {
        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                m,
                ReconcileContext::Apply,
            )
            .unwrap();
        planned_env_file_content(&plan).expect("bootstrap path dirs must plan a .cfgd.env write")
    };

    let first = plan_content(modules.clone());
    let second = plan_content(modules);

    assert!(
        first.contains(
            "export PATH=\"/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:$PATH\""
        ),
        "planned env file must export the manager's PATH entries in order: {first}"
    );
    // The file's content is hashed and compared on every reconcile tick, so a
    // non-deterministic ordering would surface as drift on a random subset of
    // ticks forever.
    assert_eq!(
        first, second,
        "consecutive plans must produce byte-identical env file content"
    );
}

#[test]
#[serial_test::serial]
fn plan_env_injects_source_line_for_bootstrap_only_profile() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    let state = test_state();
    record_brew_bootstrap(&state);
    let registry = registry_with_bootstrappable_brew();
    let reconciler = Reconciler::new(&registry, &state);
    // No env vars, no aliases — the manager's PATH entries are the only reason
    // this profile has an env surface at all.
    let resolved = make_empty_resolved();
    let modules = vec![make_resolved_module("tools")];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let env_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Prerequisites)
        .expect("env phase");
    assert!(
        env_phase
            .actions()
            .any(|a| matches!(a, Action::Env(EnvAction::InjectSourceLine { .. }))),
        "a written env file no shell sources is inert: {:?}",
        env_phase.actions().collect::<Vec<_>>()
    );
}

#[test]
#[serial_test::serial]
fn plan_env_writes_nothing_for_a_manager_cfgd_never_bootstrapped() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    // Registry knows brew and the profile names brew packages, but the state
    // store holds no bootstrap record — the machine's brew is the user's own.
    let state = test_state();
    let registry = registry_with_bootstrappable_brew();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let modules = vec![make_resolved_module("tools")];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    // Rewriting a user's `.bashrc` because a profile happens to name a manager
    // the user installed themselves claims ownership of a machine change cfgd
    // never made. No env actions ⇒ the phase is dropped entirely.
    let env_actions = plan
        .phases
        .iter()
        .flat_map(|phase| phase.actions())
        .filter(|action| matches!(action, Action::Env(_)))
        .count();
    assert_eq!(
        env_actions, 0,
        "an unbootstrapped manager must earn no env file and no rc source line: {:?}",
        plan.phases
    );
    assert!(
        !tmp_home.path().join(".cfgd.env").exists(),
        "planning must not write the env file"
    );
}

#[test]
#[serial_test::serial]
fn plan_env_folds_in_a_to_be_provisioned_managers_declared_path_dirs() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    // No bootstrap has ever run — the state store holds no record — but the
    // manager this run is about to provision names, on its own
    // `BootstrapPlan`, where its binaries will land. `plan()` must fold that
    // declaration in itself, without waiting for the bootstrap to actually
    // run and record it.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("brew", &[]).declaring_path_dirs(&[
            "/home/linuxbrew/.linuxbrew/bin",
            "/home/linuxbrew/.linuxbrew/sbin",
        ]),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let modules = vec![make_resolved_module("tools")];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();

    let content = planned_env_file_content(&plan)
        .expect("a to-be-provisioned manager's declared dirs must plan a .cfgd.env write");
    assert!(
        content.contains(
            "export PATH=\"/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:$PATH\""
        ),
        "the planner must fold the Provision node's declared dirs in at plan time: {content}"
    );
}

#[test]
fn env_targets_folded_path_dirs_render_into_the_fish_managed_file() {
    // The same folded PATH-dir set `plan_env_folds_in_a_to_be_provisioned_managers_declared_path_dirs`
    // pins through the bash `.cfgd.env` render — proven here through fish's
    // dialect too, so a divergence in `generate_fish_env_content`'s PATH
    // folding (a different join char, a missing per-entry quote) cannot hide
    // behind bash-only coverage.
    let home = Path::new("/h");
    let mut probe = env_probe("/bin/bash");
    probe.fish_present = true;
    let dirs: Vec<ManagerPathDir> = BREW_PATH_DIRS
        .iter()
        .map(|d| ManagerPathDir::new("brew", *d))
        .collect();
    let t = env_targets(
        EnvContent::new(&[], &[], &dirs, &Default::default()),
        EnvScope::Interactive,
        home,
        &probe,
        EnvPlatform::Linux,
    );
    let fish_content = t
        .iter()
        .find_map(|target| match target {
            EnvTarget::ManagedFile { path, content, .. } if path.ends_with("cfgd-env.fish") => {
                Some(content.clone())
            }
            _ => None,
        })
        .expect("fish_present must plan the fish managed file");
    assert!(
        fish_content.contains(
            "set -gx PATH '/home/linuxbrew/.linuxbrew/bin' '/home/linuxbrew/.linuxbrew/sbin' $PATH"
        ),
        "fish must fold the same PATH dirs bash renders, single-quoted per entry: {fish_content}"
    );
}

#[test]
fn env_targets_folded_path_dirs_render_into_the_powershell_managed_file() {
    // PowerShell counterpart of the fish assertion above: same folded PATH-dir
    // set, `;`-joined and double-quoted for `$env:PATH` interpolation.
    let home = Path::new("/h");
    let dirs: Vec<ManagerPathDir> = BREW_PATH_DIRS
        .iter()
        .map(|d| ManagerPathDir::new("brew", *d))
        .collect();
    let t = env_targets(
        EnvContent::new(&[], &[], &dirs, &Default::default()),
        EnvScope::Interactive,
        home,
        &env_probe(""),
        EnvPlatform::Windows,
    );
    let ps_content = t
        .iter()
        .find_map(|target| match target {
            EnvTarget::ManagedFile { path, content, .. } if path.ends_with(".cfgd-env.ps1") => {
                Some(content.clone())
            }
            _ => None,
        })
        .expect("Windows must plan the PowerShell managed file");
    assert!(
        ps_content.contains(
            "$env:PATH = \"/home/linuxbrew/.linuxbrew/bin;/home/linuxbrew/.linuxbrew/sbin;$env:PATH\""
        ),
        "PowerShell must fold the same PATH dirs bash renders, `;`-joined: {ps_content}"
    );
}

/// Every managed file reports what IT holds. At `envScope: All` a Linux host
/// also writes `environment.d`, which has no alias syntax at all — a count
/// taken from the run's merged totals would have that write claim aliases its
/// file does not contain. An entry whose name the generator refuses is not
/// counted either: it renders no line.
#[test]
fn every_managed_env_file_counts_its_own_lines() {
    let env = vec![
        EnvVar {
            name: "EDITOR".to_string(),
            value: "nvim".to_string(),
            platforms: vec![],
        },
        EnvVar {
            name: "PAGER".to_string(),
            value: "less".to_string(),
            platforms: vec![],
        },
        EnvVar {
            name: "BAD NAME".to_string(),
            value: "x".to_string(),
            platforms: vec![],
        },
    ];
    let aliases = vec![
        ShellAlias {
            name: "v".to_string(),
            command: "nvim".to_string(),
            platforms: vec![],
        },
        ShellAlias {
            name: "bad name".to_string(),
            command: "true".to_string(),
            platforms: vec![],
        },
    ];
    let counts: std::collections::HashMap<String, (usize, usize)> = env_targets(
        EnvContent::new(&env, &aliases, &[], &Default::default()),
        EnvScope::All,
        Path::new("/h"),
        &env_probe(""),
        EnvPlatform::Linux,
    )
    .into_iter()
    .filter_map(|t| match t {
        EnvTarget::ManagedFile { path, rendered, .. } => Some((
            path.file_name()?.to_string_lossy().into_owned(),
            (rendered.vars, rendered.aliases),
        )),
        _ => None,
    })
    .collect();

    assert_eq!(
        counts.get(".cfgd.env"),
        Some(&(2, 1)),
        "the shell file holds the entries whose names the generator accepted: {counts:?}"
    );
    assert_eq!(
        counts.get("cfgd.conf"),
        Some(&(2, 0)),
        "environment.d renders env vars only: {counts:?}"
    );
}

#[test]
#[serial_test::serial]
fn apply_converges_env_file_in_the_same_run_that_bootstraps() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    // First apply on a bare machine: no record exists when the Env phase is
    // planned, so its PATH entries cannot be known that early.
    let state = test_state();
    let registry = registry_with_bootstrappable_brew();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let modules = vec![make_resolved_module("tools")];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules.clone(),
            ReconcileContext::Apply,
        )
        .unwrap();
    assert!(
        planned_env_file_content(&plan).is_none(),
        "nothing is recorded yet, so the Env phase has nothing to write"
    );

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);

    // The Modules phase bootstrapped brew and recorded its directories, and the
    // post-phase regeneration folded them in — so the file is right by the end
    // of THIS apply, not only from the next one on.
    let contents = std::fs::read_to_string(tmp_home.path().join(".cfgd.env"))
        .expect("the bootstrapping apply must leave a .cfgd.env behind");
    assert!(
        contents.contains(
            "export PATH=\"/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:$PATH\""
        ),
        "the bootstrapped manager's directories must reach the env file: {contents}"
    );

    // The record is what a later plan reads back: re-derived byte-identical,
    // the write is elided at plan time, so a second apply plans nothing. A
    // derivation that drifted from the record would surface here as a planned
    // write carrying the drifted content.
    let replan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();
    assert!(
        planned_env_file_content(&replan).is_none(),
        "a converged env file must plan no write: {:?}",
        planned_env_file_content(&replan)
    );
}

// -----------------------------------------------------------------------
// path_dirs_changed: order-insensitive convergence-net comparison
// -----------------------------------------------------------------------

#[test]
fn path_dirs_changed_is_false_when_only_order_differs() {
    let now = vec![
        ManagerPathDir::new("brew", "/home/linuxbrew/.linuxbrew/bin"),
        ManagerPathDir::new("npm", "/home/u/.npm-global/bin"),
    ];
    let at_plan = vec![
        ManagerPathDir::new("npm", "/home/u/.npm-global/bin"),
        ManagerPathDir::new("brew", "/home/linuxbrew/.linuxbrew/bin"),
    ];
    assert!(
        !super::apply::path_dirs_changed(&now, &at_plan),
        "the same set of directories in a different order must not read as drift"
    );
}

#[test]
fn path_dirs_changed_is_true_when_the_set_actually_differs() {
    // Models npm: its resolved global prefix is only knowable once the
    // install finishes, so the plan-time fold cannot have named it.
    let now = vec![
        ManagerPathDir::new("npm", "/home/u/.npm-global/bin"),
        ManagerPathDir::new("npm", "/usr/local/lib/node_modules/.bin"),
    ];
    let at_plan = vec![ManagerPathDir::new("npm", "/home/u/.npm-global/bin")];
    assert!(
        super::apply::path_dirs_changed(&now, &at_plan),
        "a genuinely new directory must still trigger regeneration"
    );
}

#[test]
fn path_dirs_changed_is_false_for_identical_input() {
    let dirs = vec![ManagerPathDir::new("brew", "/opt/homebrew/bin")];
    assert!(!super::apply::path_dirs_changed(&dirs, &dirs));
}

/// Two package managers: `brew`, unavailable so this run provisions it, and
/// `npm`, already satisfied so it earns no action of its own — only its
/// state-store record, seeded by the caller before planning.
fn brew_and_npm_module_fixture() -> Vec<ResolvedModule> {
    let brew_package = ResolvedPackage {
        canonical_name: "ripgrep".to_string(),
        resolved_name: "ripgrep".to_string(),
        manager: "brew".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    let npm_package = ResolvedPackage {
        canonical_name: "prettier".to_string(),
        resolved_name: "prettier".to_string(),
        manager: "npm".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    };
    vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![brew_package, npm_package],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }]
}

#[test]
#[serial_test::serial]
fn apply_does_not_reorder_the_env_file_when_a_new_manager_joins_an_already_recorded_one() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    // npm is already recorded from a prior run. `state::bootstrapped_managers`
    // reads managers back `ORDER BY manager` — "brew" sorts ahead of "npm" —
    // while the plan-time fold appends a newly-provisioned manager's dirs
    // AFTER whatever was already recorded. The two orders disagree on
    // purpose: this is what the convergence-net comparison must tolerate
    // without rewriting the file.
    let state = test_state();
    state
        .record_bootstrapped_path_dirs("npm", &["/home/u/.npm-global/bin".to_string()])
        .expect("record npm bootstrap path dirs");
    // Unlike `registry_with_bootstrappable_brew` (which leaves the plan-time
    // declaration empty on purpose, to model npm's late-known prefix), this
    // manager declares the SAME dirs it will later record — the ordinary,
    // reconciled shape every real `PackageManager` now has (Task 10). Only
    // that shape can prove "an ordinary provision does not spuriously
    // regenerate": if the declared and recorded sets differed, this test
    // could not tell a real divergence apart from an ordering artifact.
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        BootstrappingPackageManager::new("brew", &BREW_PATH_DIRS)
            .declaring_path_dirs(&BREW_PATH_DIRS),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let modules = brew_and_npm_module_fixture();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules.clone(),
            ReconcileContext::Apply,
        )
        .unwrap();
    let planned_content = planned_env_file_content(&plan)
        .expect("the pre-recorded npm dir alone must already plan an env write");
    assert!(
        planned_content.contains(
            "export PATH=\"/home/u/.npm-global/bin:/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:$PATH\""
        ),
        "npm (already recorded) must lead, brew (declared by this run's Provision) must \
         follow, in fold order: {planned_content}"
    );

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);

    // brew is now ALSO recorded, so a post-run read of `bootstrapped_managers`
    // comes back alphabetically ("brew" then "npm") — the opposite of the
    // fold order above. A convergence net that compared those two orderings
    // directly would rewrite the file into the alphabetical order; the file
    // on disk must stay byte-identical to what the Env phase already wrote.
    let contents = std::fs::read_to_string(tmp_home.path().join(".cfgd.env"))
        .expect("apply must leave a .cfgd.env behind");
    assert_eq!(
        contents, planned_content,
        "an ordinary provision beside an already-recorded manager must not reorder PATH \
         between the plan-time write and the end of this same apply: {contents}"
    );
}

#[test]
#[serial_test::serial]
fn apply_reports_one_result_per_env_surface_when_env_and_bootstrap_coincide() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    // `spec.env` makes the Env phase write the file early; the bootstrap makes
    // the post-phase regeneration rewrite the same file. One surface, one row.
    let state = test_state();
    let registry = registry_with_bootstrappable_brew();
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    resolved.merged.env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    // The default `EnvScope::All` also plans a live-session refresh, and this
    // test applies unfiltered — that action shells out to the developer's real
    // login session (`systemctl --user set-environment`, `launchctl setenv`,
    // `setx` into HKCU) which no test home can sandbox. File merging is the
    // subject here, so keep the scope to the surfaces that stay on disk.
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;
    let modules = vec![make_resolved_module("tools")];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules.clone(),
            ReconcileContext::Apply,
        )
        .unwrap();
    let planned_total: usize = plan.phases.iter().map(|p| p.action_count()).sum();
    assert!(
        !plan
            .phases
            .iter()
            .flat_map(|p| p.actions())
            .any(|a| matches!(a, Action::Env(EnvAction::RefreshLiveSession { .. }))),
        "no live-session refresh may be planned before this test applies"
    );

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);
    // The bootstrap makes this apply a candidate for the post-phase env
    // regeneration, which re-plans env work from scratch — so a plan-level
    // check alone cannot prove the live session was left alone. The applied
    // results are where a regenerated refresh would surface.
    assert!(
        !result.action_results.iter().any(|r| r
            .description
            .contains(super::format::LIVE_SESSION_RESOURCE_ID)),
        "the host's live session must not be touched: {:?}",
        result
            .action_results
            .iter()
            .map(|r| r.description.as_str())
            .collect::<Vec<_>>()
    );

    let env_file = crate::to_posix_string(tmp_home.path().join(".cfgd.env"));
    let rows: Vec<&ActionResult> = result
        .action_results
        .iter()
        .filter(|r| r.description.trim_end_matches(":skipped") == format!("env:write:{env_file}"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "one env surface must yield one result: {:?}",
        result.action_results
    );
    assert!(rows[0].changed, "the surface was written, so it changed");
    assert!(
        result.action_results.len() <= planned_total,
        "results ({}) must not outgrow the {planned_total} planned actions.\nplanned: {:?}\nresults: {:?}",
        result.action_results.len(),
        plan.phases
            .iter()
            .flat_map(|p| p
                .actions()
                .map(crate::reconciler::format_action_description))
            .collect::<Vec<_>>(),
        result
            .action_results
            .iter()
            .map(|r| r.description.as_str())
            .collect::<Vec<_>>()
    );

    // The merge must not swallow the regeneration's content.
    let contents = std::fs::read_to_string(tmp_home.path().join(".cfgd.env")).unwrap();
    assert!(
        contents.contains("export EDITOR=\"nvim\"")
            && contents.contains("/home/linuxbrew/.linuxbrew/bin"),
        "both inputs must survive into the file: {contents}"
    );
}

/// Planning is pure, so the widest scope can be covered here without the
/// session shell-out that makes applying it untestable: the refresh action is
/// asserted as a planned value and the plan is deliberately never applied.
#[test]
#[serial_test::serial]
fn plan_env_all_scope_appends_a_live_session_refresh_after_the_file_surfaces() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    resolved.merged.env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    resolved.merged.env_scope = crate::config::EnvScope::All;

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let env_actions: Vec<&Action> = plan
        .phases
        .iter()
        .flat_map(|p| p.actions())
        .filter(|a| matches!(a, Action::Env(_)))
        .collect();
    let refresh_at = env_actions
        .iter()
        .position(|a| matches!(a, Action::Env(EnvAction::RefreshLiveSession { .. })))
        .expect("EnvScope::All must plan a live-session refresh");
    assert_eq!(
        refresh_at,
        env_actions.len() - 1,
        "the refresh must run after the durable files are written: {:?}",
        env_actions
            .iter()
            .map(|a| crate::reconciler::format_action_description(a))
            .collect::<Vec<_>>()
    );
    let Action::Env(EnvAction::RefreshLiveSession { vars }) = env_actions[refresh_at] else {
        unreachable!("the position above matched this variant")
    };
    assert_eq!(
        vars.as_slice(),
        &[("EDITOR".to_string(), "nvim".to_string())],
        "the refresh must carry the declared variables"
    );
    assert_eq!(
        crate::reconciler::format_action_description(env_actions[refresh_at]),
        super::format::LIVE_SESSION_RESOURCE_ID,
        "the refresh resource-id is what the apply-side guards match on"
    );
}

#[test]
#[serial_test::serial]
fn apply_phase_modules_bootstraps_without_touching_any_env_surface() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    // A pre-existing rc file gives the source-line injection something real to
    // land in, so its absence afterwards is evidence rather than a vacuous pass.
    let bashrc = tmp_home.path().join(".bashrc");
    std::fs::write(&bashrc, "# user's own line\n").unwrap();

    let state = test_state();
    let registry = registry_with_bootstrappable_brew();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let modules = vec![make_resolved_module("tools")];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules.clone(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Modules)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);

    // `--phase modules` must stay inside the Modules phase. Rewriting the env
    // file or injecting a source line into an rc file reaches surfaces the
    // caller deliberately excluded.
    assert!(
        !tmp_home.path().join(".cfgd.env").exists(),
        "a phase-scoped apply must not write the env file"
    );
    assert_eq!(
        std::fs::read_to_string(&bashrc).unwrap(),
        "# user's own line\n",
        "a phase-scoped apply must not inject a source line"
    );
    assert!(
        !result
            .action_results
            .iter()
            .any(|r| r.description.starts_with("env:")),
        "no env result may be reported: {:?}",
        result.action_results
    );

    // The bootstrap record IS durable, so the next unfiltered apply converges
    // the file rather than losing the directories forever. It re-plans first,
    // exactly as `cfgd apply` does — and that fresh plan reads the record the
    // phase-scoped run left behind.
    let replan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules.clone(),
            ReconcileContext::Apply,
        )
        .unwrap();
    let unfiltered = reconciler
        .apply(
            &replan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(unfiltered.status, ApplyStatus::Success);
    let contents = std::fs::read_to_string(tmp_home.path().join(".cfgd.env"))
        .expect("the following unfiltered apply must converge the env file");
    assert!(
        contents.contains(
            "export PATH=\"/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:$PATH\""
        ),
        "the recorded directories must survive the phase-scoped run: {contents}"
    );
}

#[test]
fn apply_module_install_packages_no_op_when_manager_not_in_registry() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "ghost".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "anything".to_string(),
            resolved_name: "anything".to_string(),
            manager: "no-such-manager".to_string(),
            manager_declared: false,
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
            min_version: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "ghost".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "anything".to_string(),
                        resolved_name: "anything".to_string(),
                        manager: "no-such-manager".to_string(),
                        manager_declared: false,
                        version: None,
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: None,
                    }],
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Missing manager — no error (silent skip after the if-let-None branch).
    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        result.action_results[0]
            .description
            .contains("module:ghost:packages"),
        "desc: {}",
        result.action_results[0].description
    );
}

#[test]
#[cfg(unix)]
fn apply_module_install_packages_script_manager_runs_per_package_script() {
    let dir = tempfile::tempdir().unwrap();
    let marker_a = dir.path().join("script-a-ran");
    let marker_b = dir.path().join("script-b-ran");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "scripted".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "scripted".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![
                        ResolvedPackage {
                            canonical_name: "pkg-a".to_string(),
                            resolved_name: "pkg-a".to_string(),
                            manager: "script".to_string(),
                            manager_declared: false,
                            version: None,
                            script: Some(format!("touch {}", marker_a.display())),
                            creates: None,
                            only_if: None,
                            unless: None,
                            min_version: None,
                        },
                        ResolvedPackage {
                            canonical_name: "pkg-b".to_string(),
                            resolved_name: "pkg-b".to_string(),
                            manager: "script".to_string(),
                            manager_declared: false,
                            version: None,
                            script: Some(format!("touch {}", marker_b.display())),
                            creates: None,
                            only_if: None,
                            unless: None,
                            min_version: None,
                        },
                    ],
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(marker_a.exists(), "script for pkg-a must have run");
    assert!(marker_b.exists(), "script for pkg-b must have run");
}

#[test]
#[cfg(unix)]
fn apply_module_install_packages_script_manager_failure_returns_err() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let dir = tempfile::tempdir().unwrap();
    let modules = vec![ResolvedModule {
        name: "bad-script".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "bad-script".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "broken".to_string(),
                        resolved_name: "broken".to_string(),
                        manager: "script".to_string(),
                        manager_declared: false,
                        version: None,
                        script: Some("exit 3".to_string()),
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: None,
                    }],
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Apply records the failure inside action_results[0].success=false.
    assert!(
        !result.action_results[0].success,
        "script failure must surface as action failure"
    );
}

/// Build a single-package script-install plan with the given guards, run apply,
/// and return the resulting `ApplyResult` plus whether the marker was created.
#[cfg(unix)]
fn run_guarded_script_install(
    dir: &std::path::Path,
    marker: &std::path::Path,
    creates: Option<String>,
    only_if: Option<String>,
    unless: Option<String>,
) -> crate::reconciler::ApplyResult {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "guarded".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "guarded".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "guarded-pkg".to_string(),
                        resolved_name: "guarded-pkg".to_string(),
                        manager: "script".to_string(),
                        manager_declared: false,
                        version: None,
                        script: Some(format!("touch {}", marker.display())),
                        creates,
                        only_if,
                        unless,
                        min_version: None,
                    }],
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            dir,
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Packages)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap()
}

#[test]
#[cfg(unix)]
fn script_install_creates_existing_path_skips_and_reports_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    // The `creates` path already exists, so the install must be skipped.
    let creates_path = dir.path().join("already-there");
    std::fs::write(&creates_path, "x").unwrap();

    let result = run_guarded_script_install(
        dir.path(),
        &marker,
        Some(creates_path.display().to_string()),
        None,
        None,
    );

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        !marker.exists(),
        "creates guard satisfied: install script must NOT run"
    );
    assert!(
        !result.action_results[0].changed,
        "skipped install must report changed=false"
    );
}

#[test]
#[cfg(unix)]
fn script_install_creates_missing_path_runs_and_reports_changed() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let creates_path = dir.path().join("not-there-yet");

    let result = run_guarded_script_install(
        dir.path(),
        &marker,
        Some(creates_path.display().to_string()),
        None,
        None,
    );

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        marker.exists(),
        "creates path missing: install script must run"
    );
    assert!(
        result.action_results[0].changed,
        "executed install must report changed=true"
    );
}

#[test]
#[cfg(unix)]
fn script_install_unless_holds_skips_and_reports_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");

    // `unless: true` exits zero → the guarded state already holds → skip.
    let result =
        run_guarded_script_install(dir.path(), &marker, None, None, Some("true".to_string()));

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(!marker.exists(), "unless holds: install must NOT run");
    assert!(
        !result.action_results[0].changed,
        "skipped install must report changed=false"
    );
}

#[test]
#[cfg(unix)]
fn script_install_only_if_fails_skips_and_reports_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");

    // `onlyIf: false` exits non-zero → condition not met → skip.
    let result =
        run_guarded_script_install(dir.path(), &marker, None, Some("false".to_string()), None);

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(!marker.exists(), "onlyIf failed: install must NOT run");
    assert!(
        !result.action_results[0].changed,
        "skipped install must report changed=false"
    );
}

#[test]
#[cfg(unix)]
fn script_install_no_guards_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");

    let result = run_guarded_script_install(dir.path(), &marker, None, None, None);

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(marker.exists(), "no guards: install must run every apply");
    assert!(
        result.action_results[0].changed,
        "ungated install must report changed=true"
    );
}

// -----------------------------------------------------------------------
// apply: module-level onChange scripts
// -----------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn apply_module_on_change_script_runs_when_module_changed() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("mod-onchange-ran");
    let source = dir.path().join("source.txt");
    let target = dir.path().join("target.txt");
    std::fs::write(&source, "v1").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module_actions = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Simple(format!("touch {}", marker.display()))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    // Plan includes a file action that affects this module → records a
    // `module:mymod:files:1` change entry → the module-level on_change runs.
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: {
                    let files = vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &module_actions,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        marker.exists(),
        "module-level on_change script should have run after the file deploy"
    );
}

#[test]
#[cfg(unix)]
fn apply_module_on_change_script_does_not_run_when_module_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("mod-onchange-noop");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module_actions = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Simple(format!("touch {}", marker.display()))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    // Empty plan: no changes, on_change must NOT fire.
    let plan = Plan {
        phases: vec![],
        warnings: vec![],
    };

    let printer = test_printer();
    let _result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Modules)),
            &module_actions,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert!(
        !marker.exists(),
        "module-level on_change script must NOT run when nothing changed"
    );
}

#[test]
#[cfg(unix)]
fn apply_module_on_change_skip_scripts_flag_bypasses_module_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("mod-onchange-skipped");
    let source = dir.path().join("source.txt");
    let target = dir.path().join("target.txt");
    std::fs::write(&source, "v1").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module_actions = vec![ResolvedModule {
        name: "skipmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Simple(format!("touch {}", marker.display()))],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "skipmod".to_string(),
                kind: {
                    let files = vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    // skip_scripts=true → module on_change must NOT run.
    let _result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &module_actions,
            ReconcileContext::Apply,
            true,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert!(
        !marker.exists(),
        "skip_scripts=true must suppress module on_change execution"
    );
}

// ---------------------------------------------------------------------------
// secret_env_collector: ResolveEnv action with a registered provider drives
// the secret-env injection branch of apply.rs. After every
// per-action loop pass with a non-empty collector, `Self::plan_env` re-runs
// to produce env actions that include the resolved secret values.
// ---------------------------------------------------------------------------

#[test]
fn apply_resolve_env_action_collects_secret_into_env_actions() {
    use crate::providers::SecretAction;
    use crate::test_helpers::MockSecretProvider;
    use std::path::PathBuf;

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("super-secret-value"),
    ));

    let mut resolved = make_empty_resolved();
    // Plan-side adds an env file action so plan_env has env work to do after
    // the secret_env_collector flushes the collected values.
    resolved.merged.env.push(crate::config::EnvVar {
        name: "API_TOKEN".to_string(),
        value: String::new(),
        platforms: vec![],
    });
    // Under the default `EnvScope::All` the secret-env regeneration also plans a
    // live-session refresh, which would publish the resolved secret into the
    // developer's real login session via `systemctl --user set-environment` /
    // `launchctl setenv` / `setx`. A test home cannot sandbox a session
    // shell-out, so keep the scope to on-disk surfaces.
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;

    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());

    // Construct a plan containing a Secret::ResolveEnv that delivers the
    // resolved provider value into the API_TOKEN env var.
    let secret_action = Action::Secret(SecretAction::ResolveEnv {
        provider: "vault".to_string(),
        reference: "kv/data/token".to_string(),
        envs: vec!["API_TOKEN".to_string()],
        template: None,
        origin: "local".to_string(),
    });
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![secret_action],
        )],
        warnings: Vec::new(),
    };

    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            true, // skip_scripts to keep the test deterministic
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply must succeed");

    // The first action result is the secret resolve itself; the secret-env
    // injection branch then appends one or more env action results to push
    // the secret into the env file.
    assert!(
        result.action_results.len() >= 2,
        "expected secret + env-injection actions, got: {:?}",
        result.action_results
    );
    let descriptions: Vec<&str> = result
        .action_results
        .iter()
        .map(|r| r.description.as_str())
        .collect();
    assert!(
        descriptions
            .iter()
            .any(|d| d.contains("secret:resolve-env")),
        "expected secret:resolve-env action result, got: {descriptions:?}"
    );
    assert!(
        !descriptions
            .iter()
            .any(|d| d.contains(super::format::LIVE_SESSION_RESOURCE_ID)),
        "the resolved secret must not be pushed into the host's live session: {descriptions:?}"
    );
    // PathBuf usage to anchor the import even when run on platforms where
    // home expansion differs.
    let _: PathBuf = tmp.path().to_path_buf();
}

// ---------------------------------------------------------------------------
// plan_modules: manager-priority sort exercises plan.rs's bootstrap-plan arm
// when a manager is registered but not currently available.
// ---------------------------------------------------------------------------

#[test]
fn plan_modules_sorts_bootstrappable_managers_after_native_ones() {
    // brew = bootstrappable (not available now, but plans a bootstrap) → 1
    // unknown-mgr (not in registry) → 2
    // apt = available → 0
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(MockPackageManager::new("apt")));
    registry.add_package_manager(Box::new(BootstrappingPackageManager::new("brew", &[])));
    let reconciler = Reconciler::new(&registry, &state);

    let module = ResolvedModule {
        name: "multimgr".to_string(),
        packages: vec![
            crate::modules::ResolvedPackage {
                canonical_name: "p1".to_string(),
                resolved_name: "p1".to_string(),
                manager: "unknown-mgr".to_string(),
                manager_declared: false,
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            },
            crate::modules::ResolvedPackage {
                canonical_name: "p2".to_string(),
                resolved_name: "p2".to_string(),
                manager: "brew".to_string(),
                manager_declared: false,
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            },
            crate::modules::ResolvedPackage {
                canonical_name: "p3".to_string(),
                resolved_name: "p3".to_string(),
                manager: "apt".to_string(),
                manager_declared: false,
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
                min_version: None,
            },
        ],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
        origin: None,
        platform_skip_reason: None,
    };

    let actions = reconciler
        .plan_modules(&[module], "test", ReconcileContext::Apply)
        .0;
    // Order in actions reflects the sorted manager order: apt (0), brew (1), unknown (2).
    let install_managers: Vec<String> = actions
        .iter()
        .filter_map(|(_, a)| match a {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::InstallPackages { resolved } => {
                    resolved.first().map(|p| p.manager.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        install_managers,
        vec![
            "apt".to_string(),
            "brew".to_string(),
            "unknown-mgr".to_string()
        ],
        "available manager comes first; bootstrappable second; unknown last"
    );
}

// ---------------------------------------------------------------------------
// update_module_state: covers the git_sources_json branch by applying a plan
// that includes a module with `is_git_source=true` files.
// ---------------------------------------------------------------------------

#[test]
fn apply_module_with_git_source_file_serializes_into_module_state() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("from-git.txt");
    let target = dir.path().join("target.txt");
    std::fs::write(&source, "git content").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module_actions = vec![ResolvedModule {
        name: "gitmod".to_string(),
        packages: vec![],
        files: vec![crate::modules::ResolvedFile {
            source: source.clone(),
            target: target.clone(),
            is_git_source: true,
            strategy: Some(crate::config::FileStrategy::Copy),
            encryption: None,
            permissions: None,
            patch: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "gitmod".to_string(),
                kind: {
                    let files = vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: true,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &module_actions,
            ReconcileContext::Apply,
            true,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply must succeed");

    let row = state
        .module_state_by_name("gitmod")
        .expect("module_state_by_name must succeed")
        .expect("gitmod row must exist");
    let js = row
        .git_sources
        .as_ref()
        .expect("git_sources must be Some when is_git_source=true");
    assert!(
        js.contains("from-git.txt") && js.contains("target.txt"),
        "git_sources_json must include source/target paths: {js}"
    );
}

// ---------------------------------------------------------------------------
// Module on_change error handling (apply.rs): script failure with
// default continueOnError=true records an error result but lets apply succeed.
// ---------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn apply_module_on_change_failure_continues_with_default_continue_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src.txt");
    let target = dir.path().join("tgt.txt");
    std::fs::write(&source, "v1").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // ScriptEntry::Simple defaults continueOnError=true for OnChange phase
    let module_actions = vec![ResolvedModule {
        name: "failmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Simple("exit 7".to_string())],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "failmod".to_string(),
                kind: {
                    let files = vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &module_actions,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply must succeed when continueOnError defaults true");

    let err_result = result
        .action_results
        .iter()
        .find(|r| !r.success && r.description.starts_with("module:failmod:onChange"))
        .expect("module onChange failure result must be recorded");
    assert!(err_result.error.is_some());
}

#[test]
#[cfg(unix)]
fn apply_module_on_change_failure_aborts_when_continue_on_error_false() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src.txt");
    let target = dir.path().join("tgt.txt");
    std::fs::write(&source, "v1").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module_actions = vec![ResolvedModule {
        name: "abortmod".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: vec![ScriptEntry::Full(ScriptCommand {
            workdir: None,
            run: "exit 5".to_string(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: Some(false),
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        })],
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "abortmod".to_string(),
                kind: {
                    let files = vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }];
                    let declared_total = files.len();
                    ModuleActionKind::DeployFiles {
                        files,
                        declared_total,
                    }
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let err = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &module_actions,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect_err("explicit continueOnError=false must return Err");
    let _ = err.to_string();
}

// ---------------------------------------------------------------------------
// Profile on_change error handling (apply.rs): identical pattern but
// driven from resolved.merged.scripts.on_change instead of module scripts.
// ---------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn apply_profile_on_change_failure_continues_with_default_continue_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("p_src.txt");
    let target = dir.path().join("p_tgt.txt");
    std::fs::write(&source, "v1").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.on_change = vec![ScriptEntry::Simple("exit 9".to_string())];

    let file_actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            file_actions,
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply must succeed when default continueOnError is true");

    let err_result = result
        .action_results
        .iter()
        .find(|r| !r.success && r.description.starts_with("onChange"))
        .expect("profile onChange failure must surface in results");
    assert!(err_result.error.is_some());
}

#[test]
#[cfg(unix)]
fn apply_profile_on_change_failure_aborts_when_continue_on_error_false() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("ap_src.txt");
    let target = dir.path().join("ap_tgt.txt");
    std::fs::write(&source, "v1").unwrap();

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.on_change = vec![ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "exit 11".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: Some(false),
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    })];

    let file_actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
        patch: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            file_actions,
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    let printer = test_printer();
    let err = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect_err("profile onChange continueOnError=false must propagate err");
    let _ = err.to_string();
}

// --- action_matches_phase_filter helper + --phase {pre,post}-scripts module-level inclusion ---

#[test]
fn action_matches_phase_filter_table() {
    // Helper inputs: (phase_name, action, filter) -> expected
    let pre_script_action = Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple("echo pre".to_string()),
        phase: ScriptPhase::PreApply,
        origin: "local".to_string(),
    });
    let post_script_action = Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple("echo post".to_string()),
        phase: ScriptPhase::PostApply,
        origin: "local".to_string(),
    });
    let module_pre_script = Action::Module(ModuleAction {
        module_name: "m".to_string(),
        kind: ModuleActionKind::RunScript {
            script: ScriptEntry::Simple("echo pre".to_string()),
            phase: ScriptPhase::PreApply,
        },
        origin: None,
    });
    let module_post_script = Action::Module(ModuleAction {
        module_name: "m".to_string(),
        kind: ModuleActionKind::RunScript {
            script: ScriptEntry::Simple("echo post".to_string()),
            phase: ScriptPhase::PostApply,
        },
        origin: None,
    });
    let module_install = Action::Module(ModuleAction {
        module_name: "m".to_string(),
        kind: ModuleActionKind::InstallPackages { resolved: vec![] },
        origin: None,
    });
    let pkg_install = Action::Package(PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["jq".to_string()],
        origin: "local".to_string(),
    });

    let module_owner = Owner::module("m");
    let profile_owner = Owner::profile("work");
    let managers_owner = Owner::cfgd(MANAGERS_GROUP);
    let env_owner = Owner::cfgd("env");
    let brew_provision = Action::Manager(ManagerAction::Provision {
        manager: "brew".to_string(),
        via: "curl".to_string(),
        declared: None,
        batched: vec![],
        depends_on: vec![],
    });
    let npm_refresh = Action::Manager(ManagerAction::RefreshIndex {
        manager: "npm".to_string(),
    });
    // A Prerequisite node's tool and installer deliberately differ, so a case
    // pinned to only one of them would pass even if the matcher regressed
    // onto `manager()` (the installer) instead of `filter_subject()` (the
    // tool) — the exact drift finding 3 fixed.
    let curl_prereq = Action::Manager(ManagerAction::Prerequisite {
        tool: "curl".to_string(),
        installer: "brew".to_string(),
        required_by: vec!["brew".to_string()],
        depends_on: vec![],
    });
    let cases: Vec<(&str, bool, &PhaseName, &Owner, &Action, PhaseFilter)> = vec![
        // Strict phase-equality cases.
        (
            "post script under its own phase",
            true,
            &PhaseName::PostScripts,
            &profile_owner,
            &post_script_action,
            PhaseFilter::Phase(PhaseName::PostScripts),
        ),
        (
            "pre script under its own phase",
            true,
            &PhaseName::PreScripts,
            &profile_owner,
            &pre_script_action,
            PhaseFilter::Phase(PhaseName::PreScripts),
        ),
        (
            "package install under Packages",
            true,
            &PhaseName::Packages,
            &profile_owner,
            &pkg_install,
            PhaseFilter::Phase(PhaseName::Packages),
        ),
        // Module lifecycle scripts are swept in by their script-phase filter
        // wherever they landed.
        (
            "module post script under PostScripts",
            true,
            &PhaseName::PostScripts,
            &module_owner,
            &module_post_script,
            PhaseFilter::Phase(PhaseName::PostScripts),
        ),
        (
            "module pre script under PreScripts",
            true,
            &PhaseName::PreScripts,
            &module_owner,
            &module_pre_script,
            PhaseFilter::Phase(PhaseName::PreScripts),
        ),
        // Non-script module actions are NOT swept in by script filters.
        (
            "module install under PostScripts",
            false,
            &PhaseName::Packages,
            &module_owner,
            &module_install,
            PhaseFilter::Phase(PhaseName::PostScripts),
        ),
        (
            "module install under PreScripts",
            false,
            &PhaseName::Packages,
            &module_owner,
            &module_install,
            PhaseFilter::Phase(PhaseName::PreScripts),
        ),
        // Cross-phase mismatch.
        (
            "module pre script under PostScripts",
            false,
            &PhaseName::PreScripts,
            &module_owner,
            &module_pre_script,
            PhaseFilter::Phase(PhaseName::PostScripts),
        ),
        (
            "module post script under PreScripts",
            false,
            &PhaseName::PostScripts,
            &module_owner,
            &module_post_script,
            PhaseFilter::Phase(PhaseName::PreScripts),
        ),
        // Unrelated filter only matches phase-equal actions.
        (
            "module post script under Packages",
            false,
            &PhaseName::PostScripts,
            &module_owner,
            &module_post_script,
            PhaseFilter::Phase(PhaseName::Packages),
        ),
        (
            "package install under a foreign phase filter",
            false,
            &PhaseName::Files,
            &profile_owner,
            &pkg_install,
            PhaseFilter::Phase(PhaseName::Packages),
        ),
        // `--phase modules` is an owner filter: it reaches module work in every
        // phase its kind routed to, and never reaches profile work.
        (
            "module install under ModuleOwners",
            true,
            &PhaseName::Packages,
            &module_owner,
            &module_install,
            PhaseFilter::ModuleOwners,
        ),
        (
            "module post script under ModuleOwners",
            true,
            &PhaseName::PostScripts,
            &module_owner,
            &module_post_script,
            PhaseFilter::ModuleOwners,
        ),
        (
            "profile package under ModuleOwners",
            false,
            &PhaseName::Packages,
            &profile_owner,
            &pkg_install,
            PhaseFilter::ModuleOwners,
        ),
        // `--phase prerequisites.managers` / `.env` — the dotted group-selector
        // grammar reaches the cfgd owner group by name, regardless of action kind.
        (
            "cfgd managers-group provision under prerequisites.managers",
            true,
            &PhaseName::Prerequisites,
            &managers_owner,
            &brew_provision,
            PhaseFilter::Selector(PhaseName::Prerequisites, "managers".to_string()),
        ),
        (
            "cfgd env-group action under prerequisites.env",
            true,
            &PhaseName::Prerequisites,
            &env_owner,
            &pkg_install,
            PhaseFilter::Selector(PhaseName::Prerequisites, "env".to_string()),
        ),
        (
            "cfgd managers-group provision under prerequisites.env misses",
            false,
            &PhaseName::Prerequisites,
            &managers_owner,
            &brew_provision,
            PhaseFilter::Selector(PhaseName::Prerequisites, "env".to_string()),
        ),
        (
            "cfgd managers-group under a foreign phase misses",
            false,
            &PhaseName::Packages,
            &managers_owner,
            &brew_provision,
            PhaseFilter::Selector(PhaseName::Prerequisites, "managers".to_string()),
        ),
        // `--phase prerequisites.brew` — a literal manager name selects that
        // manager's own DAG nodes, already family-collapsed at plan time.
        (
            "brew provision under prerequisites.brew",
            true,
            &PhaseName::Prerequisites,
            &managers_owner,
            &brew_provision,
            PhaseFilter::Selector(PhaseName::Prerequisites, "brew".to_string()),
        ),
        (
            "npm refresh under prerequisites.brew misses",
            false,
            &PhaseName::Prerequisites,
            &managers_owner,
            &npm_refresh,
            PhaseFilter::Selector(PhaseName::Prerequisites, "brew".to_string()),
        ),
        (
            "brew provision under prerequisites.npm misses",
            false,
            &PhaseName::Prerequisites,
            &managers_owner,
            &brew_provision,
            PhaseFilter::Selector(PhaseName::Prerequisites, "npm".to_string()),
        ),
        // A Prerequisite node is keyed on its TOOL (`curl`), not its
        // installer (`brew`) — `prerequisites.curl` reaches it and
        // `prerequisites.brew` does not, even though `brew` is the command
        // that actually runs it.
        (
            "curl prerequisite under prerequisites.curl (its tool)",
            true,
            &PhaseName::Prerequisites,
            &managers_owner,
            &curl_prereq,
            PhaseFilter::Selector(PhaseName::Prerequisites, "curl".to_string()),
        ),
        (
            "curl prerequisite under prerequisites.brew (its installer) misses",
            false,
            &PhaseName::Prerequisites,
            &managers_owner,
            &curl_prereq,
            PhaseFilter::Selector(PhaseName::Prerequisites, "brew".to_string()),
        ),
    ];

    for (label, expected, phase_name, owner, action, filter) in cases {
        assert_eq!(
            action_matches_phase_filter(phase_name, owner, action, &filter),
            expected,
            "{label}"
        );
    }
}

#[test]
fn apply_post_scripts_filter_runs_module_post_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("post_marker");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module = crate::modules::ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    };

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::InstallPackages { resolved: vec![] },
                    origin: None,
                })],
            ),
            Phase::from_actions(
                PhaseName::PostScripts,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::RunScript {
                        script: ScriptEntry::Simple(format!("touch {}", marker.display())),
                        phase: ScriptPhase::PostApply,
                    },
                    origin: None,
                })],
            ),
        ],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::PostScripts)),
            std::slice::from_ref(&module),
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // The InstallPackages action must NOT have been executed; only the
    // module post-script should have run.
    let descriptions: Vec<&str> = result
        .action_results
        .iter()
        .map(|r| r.description.as_str())
        .collect();
    assert_eq!(
        result.action_results.len(),
        1,
        "expected exactly one executed action under --phase post-scripts, got {:?}",
        descriptions,
    );
    assert!(
        result.action_results[0]
            .description
            .starts_with("module:nvim:script"),
        "executed action should be the module post-script, got: {}",
        result.action_results[0].description,
    );
    assert!(
        marker.exists(),
        "module postApply script should have run and created marker file"
    );
}

#[test]
fn apply_pre_scripts_filter_runs_module_pre_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pre_marker");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module = crate::modules::ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    };

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::PreScripts,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::RunScript {
                        script: ScriptEntry::Simple(format!("touch {}", marker.display())),
                        phase: ScriptPhase::PreApply,
                    },
                    origin: None,
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::InstallPackages { resolved: vec![] },
                    origin: None,
                })],
            ),
        ],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::PreScripts)),
            std::slice::from_ref(&module),
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(
        result.action_results.len(),
        1,
        "expected exactly one executed action under --phase pre-scripts, got {:?}",
        result
            .action_results
            .iter()
            .map(|r| &r.description)
            .collect::<Vec<_>>(),
    );
    assert!(
        result.action_results[0]
            .description
            .starts_with("module:nvim:script"),
        "executed action should be the module pre-script, got: {}",
        result.action_results[0].description,
    );
    assert!(
        marker.exists(),
        "module preApply script should have run and created marker file"
    );
}

#[test]
fn apply_modules_phase_filter_runs_all_module_actions() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("modules_marker");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module = crate::modules::ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    };

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Modules,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::Skip {
                        reason: "exercised by test".to_string(),
                    },
                    origin: None,
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::InstallPackages { resolved: vec![] },
                    origin: None,
                })],
            ),
            Phase::from_actions(
                PhaseName::PostScripts,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::RunScript {
                        script: ScriptEntry::Simple(format!("touch {}", marker.display())),
                        phase: ScriptPhase::PostApply,
                    },
                    origin: None,
                })],
            ),
        ],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::ModuleOwners),
            std::slice::from_ref(&module),
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // All three module actions should have run: the owner filter selects
    // module-owned work in every phase it landed in, not scripts only.
    assert_eq!(result.action_results.len(), 3);
    let descs: Vec<&str> = result
        .action_results
        .iter()
        .map(|r| r.description.as_str())
        .collect();
    assert!(descs.iter().any(|d| d.starts_with("module:nvim:script")));
    assert!(descs.iter().any(|d| d.starts_with("module:nvim:packages")));
    assert!(descs.iter().any(|d| d.starts_with("module:nvim:skip")));
}

#[test]
fn apply_post_scripts_filter_skips_other_phases() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("post_only_marker");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let module = crate::modules::ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    };

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Files,
                &Owner::profile("test"),
                vec![Action::File(FileAction::Skip {
                    target: PathBuf::from("/tmp/should_not_run"),
                    reason: "blocked".to_string(),
                    origin: "local".to_string(),
                })],
            ),
            Phase::from_actions(
                PhaseName::System,
                &Owner::profile("test"),
                vec![Action::System(SystemAction::Skip {
                    configurator: "shell".to_string(),
                    reason: "blocked".to_string(),
                    origin: "local".to_string(),
                    unknown: false,
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("test"),
                vec![Action::Package(PackageAction::Skip {
                    manager: "apt".to_string(),
                    reason: "blocked".to_string(),
                    origin: "local".to_string(),
                })],
            ),
            Phase::from_actions(
                PhaseName::PostScripts,
                &Owner::profile("test"),
                vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::RunScript {
                        script: ScriptEntry::Simple(format!("touch {}", marker.display())),
                        phase: ScriptPhase::PostApply,
                    },
                    origin: None,
                })],
            ),
        ],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::PostScripts)),
            std::slice::from_ref(&module),
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    // Only the module post-script should run; Files / System / Packages
    // actions are filtered out.
    assert_eq!(
        result.action_results.len(),
        1,
        "expected only the module post-script to run, got {:?}",
        result
            .action_results
            .iter()
            .map(|r| &r.description)
            .collect::<Vec<_>>(),
    );
    assert!(
        result.action_results[0]
            .description
            .starts_with("module:nvim:script")
    );
    assert!(marker.exists());
}

// ─────────────────────────────────────────────────────
// spec.env reach: EnvScope target matrix, gotchas, parity
// ─────────────────────────────────────────────────────

fn env_probe(shell: &str) -> EnvHostProbe {
    EnvHostProbe {
        shell: shell.to_string(),
        fish_present: false,
        bash_profile_exists: false,
        bash_login_exists: false,
        git_bash_present: false,
        zsh_present: shell.contains("zsh"),
    }
}

fn target_keys(targets: &[EnvTarget]) -> Vec<String> {
    targets
        .iter()
        .map(|t| match t {
            EnvTarget::ManagedFile { path, .. } => format!("file:{}", path.posix()),
            EnvTarget::SourceLine { rc_path, .. } => format!("src:{}", rc_path.posix()),
            EnvTarget::LiveSession { .. } => "session".to_string(),
        })
        .collect()
}

fn one_env() -> Vec<EnvVar> {
    vec![EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }]
}

#[test]
fn env_targets_empty_yields_nothing() {
    let home = Path::new("/h");
    let t = env_targets(
        EnvContent::new(&[], &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &env_probe("/bin/bash"),
        EnvPlatform::Linux,
    );
    assert!(t.is_empty());
}

#[test]
fn env_targets_interactive_is_env_file_plus_interactive_rc() {
    let home = Path::new("/h");
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::Interactive,
        home,
        &env_probe("/bin/bash"),
        EnvPlatform::Linux,
    );
    assert_eq!(target_keys(&t), vec!["file:/h/.cfgd.env", "src:/h/.bashrc"]);
}

#[test]
fn env_targets_interactive_zsh_uses_zshrc() {
    let home = Path::new("/h");
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::Interactive,
        home,
        &env_probe("/usr/bin/zsh"),
        EnvPlatform::Linux,
    );
    assert_eq!(target_keys(&t), vec!["file:/h/.cfgd.env", "src:/h/.zshrc"]);
}

#[test]
fn env_targets_login_adds_zshenv_only_when_zsh_present() {
    let home = Path::new("/h");
    // zsh in use ⇒ ~/.zshenv is written into the login chain.
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::Login,
        home,
        &env_probe("/bin/zsh"),
        EnvPlatform::Linux,
    );
    let keys = target_keys(&t);
    assert_eq!(
        keys,
        vec![
            "file:/h/.cfgd.env",
            "src:/h/.zshrc",
            "src:/h/.zshenv",
            "src:/h/.profile",
        ]
    );

    // bash-only host ⇒ no inert ~/.zshenv for a shell it never runs.
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::Login,
        home,
        &env_probe("/bin/bash"),
        EnvPlatform::Linux,
    );
    let keys = target_keys(&t);
    assert_eq!(
        keys,
        vec!["file:/h/.cfgd.env", "src:/h/.bashrc", "src:/h/.profile"]
    );
    // The bash first-match gotcha: never create ~/.bash_profile from nothing.
    assert!(!keys.iter().any(|k| k.ends_with(".bash_profile")));
    assert!(!keys.iter().any(|k| k.ends_with(".bash_login")));
    // Login excludes the session surfaces.
    assert!(!keys.iter().any(|k| k.contains("environment.d")));
    assert!(!keys.contains(&"session".to_string()));
}

#[test]
fn env_targets_login_injects_existing_bash_profile() {
    let home = Path::new("/h");
    let mut probe = env_probe("/bin/bash");
    probe.bash_profile_exists = true;
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::Login,
        home,
        &probe,
        EnvPlatform::Linux,
    );
    let keys = target_keys(&t);
    assert!(keys.contains(&"src:/h/.bash_profile".to_string()));
    assert!(!keys.iter().any(|k| k.ends_with(".bash_login")));
}

#[test]
fn env_targets_login_falls_back_to_bash_login_when_only_it_exists() {
    let home = Path::new("/h");
    let mut probe = env_probe("/bin/bash");
    probe.bash_login_exists = true;
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::Login,
        home,
        &probe,
        EnvPlatform::Linux,
    );
    let keys = target_keys(&t);
    assert!(keys.contains(&"src:/h/.bash_login".to_string()));
    assert!(!keys.iter().any(|k| k.ends_with(".bash_profile")));
}

#[test]
fn env_targets_all_linux_adds_environment_d_and_session() {
    let home = Path::new("/h");
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &env_probe("/bin/bash"),
        EnvPlatform::Linux,
    );
    let keys = target_keys(&t);
    assert!(keys.contains(&"file:/h/.config/environment.d/cfgd.conf".to_string()));
    assert_eq!(keys.last().map(String::as_str), Some("session"));
    // No macOS plist on Linux.
    assert!(!keys.iter().any(|k| k.contains("LaunchAgents")));
}

#[test]
fn env_targets_all_macos_adds_launchagent_not_environment_d() {
    let home = Path::new("/h");
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &env_probe("/bin/zsh"),
        EnvPlatform::MacOs,
    );
    let keys = target_keys(&t);
    assert!(
        keys.iter()
            .any(|k| k.contains("Library/LaunchAgents/com.cfgd.user-environment.plist"))
    );
    assert!(!keys.iter().any(|k| k.contains("environment.d")));
    assert_eq!(keys.last().map(String::as_str), Some("session"));
}

#[test]
fn env_targets_all_freebsd_omits_environment_d_and_launchagent() {
    // FreeBSD has neither systemd nor launchd: the .cfgd.env + rc source lines
    // are its whole env surface. It must NOT get a systemd environment.d file
    // (inert clutter no consumer reads) nor a macOS LaunchAgent plist.
    let home = Path::new("/h");
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &env_probe("/bin/sh"),
        EnvPlatform::FreeBsd,
    );
    let keys = target_keys(&t);
    assert!(keys.contains(&"file:/h/.cfgd.env".to_string()));
    assert!(!keys.iter().any(|k| k.contains("environment.d")));
    assert!(!keys.iter().any(|k| k.contains("LaunchAgents")));
    // Live-session refresh still runs last under scope=All (a guarded no-op
    // on a FreeBSD host, but the target is emitted the same way).
    assert_eq!(keys.last().map(String::as_str), Some("session"));
}

#[test]
fn env_targets_windows_is_ps_profiles_plus_session_on_all() {
    let home = Path::new("/h");
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &env_probe(""),
        EnvPlatform::Windows,
    );
    let keys = target_keys(&t);
    assert!(keys.contains(&"file:/h/.cfgd-env.ps1".to_string()));
    assert_eq!(
        keys.iter()
            .filter(|k| k.contains("Microsoft.PowerShell_profile.ps1"))
            .count(),
        2
    );
    assert_eq!(keys.last().map(String::as_str), Some("session"));
}

#[test]
fn env_targets_match_what_verify_rederives() {
    // Parity: the planner and verifier both call env_targets, so identical
    // inputs must yield an identical target set. Guards against divergence.
    let home = Path::new("/h");
    let probe = env_probe("/bin/bash");
    let a = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &probe,
        EnvPlatform::Linux,
    );
    let b = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &probe,
        EnvPlatform::Linux,
    );
    assert_eq!(target_keys(&a), target_keys(&b));
}

#[test]
fn environment_d_content_is_key_value_not_shell() {
    let env = vec![
        EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
            platforms: vec![],
        },
        EnvVar {
            name: "PATH".into(),
            value: "/usr/bin:/bin".into(),
            platforms: vec![],
        },
    ];
    let content = generate_environment_d_content(&env);
    assert!(content.contains("EDITOR='nvim'"));
    assert!(content.contains("PATH='/usr/bin:/bin'"));
    // environment.d is not shell: the parser reads assignments, so `export`
    // would be part of the name rather than a keyword.
    assert!(!content.contains("export "));
}

#[test]
fn environment_d_content_quotes_a_value_carrying_a_newline() {
    let env = vec![EnvVar {
        name: "RAW".into(),
        // Unquoted, the newline ends the assignment and the tail stands as a
        // second one — systemd would put LD_PRELOAD in the user's session.
        value: "a\nLD_PRELOAD=/evil.so".into(),
        platforms: vec![],
    }];
    let content = generate_environment_d_content(&env);
    assert!(
        content.contains("RAW='a\nLD_PRELOAD=/evil.so'"),
        "unexpected content: {content}"
    );
    assert!(!content.contains("\nLD_PRELOAD=/evil.so\n"));
}

#[test]
fn environment_d_content_re_supplies_an_embedded_quote() {
    let env = vec![EnvVar {
        name: "Q".into(),
        value: "it's".into(),
        platforms: vec![],
    }];
    let content = generate_environment_d_content(&env);
    assert!(
        content.contains("Q='it'\\''s'"),
        "unexpected content: {content}"
    );
}

#[test]
fn environment_d_content_skips_unsafe_names() {
    let env = vec![EnvVar {
        name: "BAD NAME".into(),
        value: "x".into(),
        platforms: vec![],
    }];
    let content = generate_environment_d_content(&env);
    assert!(!content.contains("BAD NAME"));
}

#[test]
fn launchd_plist_carries_label_and_vars() {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("EDITOR".to_string(), "nvim".to_string());
    let plist = launchd_env_plist("com.cfgd.user-environment", &vars);
    assert!(plist.contains("<string>com.cfgd.user-environment</string>"));
    // The agent publishes the var via `launchctl setenv` at load, not via an inert
    // `EnvironmentVariables` dict that only scopes to the job's own process.
    assert!(plist.contains("/bin/launchctl setenv EDITOR"));
    assert!(plist.contains("nvim"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(!plist.contains("/usr/bin/true"));
    assert!(!plist.contains("<key>EnvironmentVariables</key>"));
}

#[test]
fn launchd_plist_shell_escapes_values_with_spaces() {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("ACC_EDITOR".to_string(), "nvim --acc".to_string());
    let plist = launchd_env_plist("lbl", &vars);
    // A `/bin/sh -c` chain runs one `launchctl setenv` per var; a value with a space
    // must be shell-quoted so it reaches launchctl as a single argument.
    assert!(plist.contains("<string>/bin/sh</string>"));
    assert!(plist.contains("/bin/launchctl setenv ACC_EDITOR &quot;nvim --acc&quot;"));
}

#[test]
fn launchd_plist_chains_multiple_setenv() {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("A".to_string(), "1".to_string());
    vars.insert("B".to_string(), "2".to_string());
    let plist = launchd_env_plist("lbl", &vars);
    // BTreeMap iteration is sorted; the per-var setenv calls are joined with "; ".
    assert!(
        plist.contains(
            "/bin/launchctl setenv A &quot;1&quot;; /bin/launchctl setenv B &quot;2&quot;"
        )
    );
}

#[test]
fn launchd_plist_skips_unsafe_names() {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("GOOD".to_string(), "ok".to_string());
    vars.insert("BAD; rm -rf /".to_string(), "x".to_string());
    let plist = launchd_env_plist("lbl", &vars);
    assert!(plist.contains("/bin/launchctl setenv GOOD"));
    // An unsafe name must never reach the shell command.
    assert!(!plist.contains("rm -rf"));
    assert!(!plist.contains("BAD"));
}

#[test]
fn launchd_plist_xml_escapes_values() {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("X".to_string(), "a<b&c".to_string());
    let plist = launchd_env_plist("lbl", &vars);
    assert!(plist.contains("a&lt;b&amp;c"));
    assert!(!plist.contains("a<b&c"));
}

#[test]
fn plan_env_all_scope_emits_live_session_action() {
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &one_env(),
        &[],
        &Default::default(),
        EnvScope::All,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    )
    .actions;
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Env(EnvAction::RefreshLiveSession { .. }))),
        "All scope must emit a live-session refresh action"
    );
}

#[test]
fn plan_env_interactive_scope_has_no_live_session_action() {
    let tmp = tempfile::tempdir().unwrap();
    let actions = Reconciler::plan_env_with_home(
        &one_env(),
        &[],
        &Default::default(),
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    )
    .actions;
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Env(EnvAction::RefreshLiveSession { .. }))),
        "Interactive scope must not touch the live session"
    );
}

// --- Cross-scope package dedup tests ---

fn dedup_rp(name: &str, manager: &str) -> ResolvedPackage {
    ResolvedPackage {
        canonical_name: name.to_string(),
        resolved_name: name.to_string(),
        manager: manager.to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    }
}

fn install_packages_action(module: &str, resolved: Vec<ResolvedPackage>) -> (PhaseName, Action) {
    (
        PhaseName::Packages,
        Action::Module(ModuleAction {
            module_name: module.to_string(),
            kind: ModuleActionKind::InstallPackages { resolved },
            origin: None,
        }),
    )
}

fn resolved_names_of(action: &Action) -> Vec<String> {
    match action {
        Action::Module(ModuleAction {
            kind: ModuleActionKind::InstallPackages { resolved },
            ..
        }) => resolved.iter().map(|r| r.resolved_name.clone()).collect(),
        _ => Vec::new(),
    }
}

#[test]
fn dedup_profile_loses_to_module_same_manager_name() {
    let mut module_phase = vec![install_packages_action(
        "gh-auth",
        vec![dedup_rp("gh", "brew")],
    )];
    let claimed = Reconciler::dedup_module_packages(&mut module_phase);

    assert!(claimed.is_claimed("brew", "gh"));

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["gh".to_string()],
        origin: "profile".to_string(),
    }];
    let filtered = Reconciler::filter_profile_packages(pkg_actions, &claimed);

    assert!(
        filtered.is_empty(),
        "profile Install emptied by dedup must be dropped, got {filtered:?}"
    );
    // module keeps gh
    assert_eq!(
        resolved_names_of(&module_phase[0].1),
        vec!["gh".to_string()]
    );
}

#[test]
fn dedup_different_managers_keep_both() {
    let mut module_phase = vec![install_packages_action(
        "rg-mod",
        vec![dedup_rp("ripgrep", "cargo")],
    )];
    let claimed = Reconciler::dedup_module_packages(&mut module_phase);

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["ripgrep".to_string()],
        origin: "profile".to_string(),
    }];
    let filtered = Reconciler::filter_profile_packages(pkg_actions, &claimed);

    assert_eq!(filtered.len(), 1, "different managers must both survive");
    match &filtered[0] {
        PackageAction::Install { packages, .. } => {
            assert_eq!(packages, &vec!["ripgrep".to_string()]);
        }
        other => panic!("expected Install, got {other:?}"),
    }
}

#[test]
fn dedup_earlier_module_wins_over_later() {
    let mut module_phase = vec![
        install_packages_action("a", vec![dedup_rp("fd", "brew")]),
        install_packages_action("b", vec![dedup_rp("fd", "brew")]),
    ];
    Reconciler::dedup_module_packages(&mut module_phase);

    // module a keeps fd; module b's InstallPackages emptied -> action dropped
    assert_eq!(
        module_phase.len(),
        1,
        "later duplicate action must be dropped"
    );
    match &module_phase[0].1 {
        Action::Module(ModuleAction { module_name, .. }) => assert_eq!(module_name, "a"),
        other => panic!("expected Module action, got {other:?}"),
    }
    assert_eq!(
        resolved_names_of(&module_phase[0].1),
        vec!["fd".to_string()]
    );
}

#[test]
fn dedup_script_manager_never_dropped() {
    let mut module_phase = vec![
        install_packages_action("a", vec![dedup_rp("setup", "script")]),
        install_packages_action("b", vec![dedup_rp("setup", "script")]),
    ];
    let claimed = Reconciler::dedup_module_packages(&mut module_phase);

    assert!(
        !claimed.is_claimed("script", "setup"),
        "script keys must not be claimed"
    );
    assert_eq!(module_phase.len(), 2, "both script installs must survive");
    assert_eq!(
        resolved_names_of(&module_phase[0].1),
        vec!["setup".to_string()]
    );
    assert_eq!(
        resolved_names_of(&module_phase[1].1),
        vec!["setup".to_string()]
    );
}

#[test]
fn dedup_mixed_kept_and_dropped_in_one_action() {
    let mut module_phase = vec![
        install_packages_action("a", vec![dedup_rp("fd", "brew")]),
        install_packages_action("b", vec![dedup_rp("fd", "brew"), dedup_rp("bat", "brew")]),
    ];
    Reconciler::dedup_module_packages(&mut module_phase);

    assert_eq!(module_phase.len(), 2);
    assert_eq!(
        resolved_names_of(&module_phase[0].1),
        vec!["fd".to_string()]
    );
    // module b's fd dropped, bat retained
    assert_eq!(
        resolved_names_of(&module_phase[1].1),
        vec!["bat".to_string()]
    );
}

#[test]
fn dedup_profile_install_partial_retains_unclaimed() {
    let mut module_phase = vec![install_packages_action("a", vec![dedup_rp("gh", "brew")])];
    let claimed = Reconciler::dedup_module_packages(&mut module_phase);

    let pkg_actions = vec![PackageAction::Install {
        manager: "brew".to_string(),
        packages: vec!["gh".to_string(), "jq".to_string()],
        origin: "profile".to_string(),
    }];
    let filtered = Reconciler::filter_profile_packages(pkg_actions, &claimed);

    assert_eq!(filtered.len(), 1);
    match &filtered[0] {
        PackageAction::Install { packages, .. } => {
            assert_eq!(packages, &vec!["jq".to_string()], "gh deduped, jq kept");
        }
        other => panic!("expected Install, got {other:?}"),
    }
}

#[test]
fn dedup_passes_through_non_install_package_actions() {
    let claimed = crate::config::PackageClaim::from_claimed(
        [("brew".to_string(), "gh".to_string())]
            .into_iter()
            .collect(),
    );
    let pkg_actions = vec![
        PackageAction::Uninstall {
            manager: "brew".to_string(),
            packages: vec!["gh".to_string()],
            origin: "profile".to_string(),
        },
        PackageAction::Skip {
            manager: "brew".to_string(),
            reason: "available".to_string(),
            origin: "profile".to_string(),
        },
    ];
    let filtered = Reconciler::filter_profile_packages(pkg_actions, &claimed);
    assert_eq!(
        filtered.len(),
        2,
        "Uninstall/Skip must pass through untouched"
    );
}

#[test]
fn execute_script_working_dir_is_a_file_errors() {
    // A `working_dir` that resolves to a regular file (not a directory) must be
    // rejected up front with a message that names the path and the kind, rather
    // than surfacing a cryptic spawn ENOENT later.
    let printer = test_printer();
    let entry = ScriptEntry::Simple("echo hi".to_string());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, "x").unwrap();

    let result = super::execute_script(
        &entry,
        dir.path(),
        &file,
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("working directory is not a directory"),
        "error must name the not-a-directory condition, got: {err}"
    );
}

#[test]
fn execute_script_working_dir_missing_errors() {
    // A non-existent `working_dir` (e.g. a cleaned-up tempdir) is caught by the
    // pre-spawn metadata probe and reported as "does not exist".
    let printer = test_printer();
    let entry = ScriptEntry::Simple("echo hi".to_string());
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("gone");

    let result = super::execute_script(
        &entry,
        dir.path(),
        &missing,
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("working directory does not exist"),
        "error must name the missing-directory condition, got: {err}"
    );
}

#[test]
fn env_targets_windows_with_git_bash_adds_unix_env_file_and_bashrc() {
    // When a POSIX sh (Git Bash) is present, the Windows arm additionally emits
    // the shared Unix bash env file and a ~/.bashrc source line.
    let home = Path::new("/h");
    let probe = EnvHostProbe {
        shell: "/bin/bash".to_string(),
        fish_present: false,
        bash_profile_exists: false,
        bash_login_exists: false,
        git_bash_present: true,
        zsh_present: false,
    };
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::All,
        home,
        &probe,
        EnvPlatform::Windows,
    );
    let keys = target_keys(&t);
    assert!(
        keys.contains(&"file:/h/.cfgd.env".to_string()),
        "Git Bash ⇒ unix env file expected, got: {keys:?}"
    );
    assert!(
        keys.contains(&"src:/h/.bashrc".to_string()),
        "Git Bash ⇒ .bashrc source line expected, got: {keys:?}"
    );
}

#[test]
fn env_targets_fish_present_adds_managed_fish_file() {
    // When fish is in use (probe.fish_present), the interactive surface gains a
    // managed conf.d/cfgd-env.fish file alongside the bash env file + rc line.
    let home = Path::new("/h");
    let probe = EnvHostProbe {
        shell: "/bin/bash".to_string(),
        fish_present: true,
        bash_profile_exists: false,
        bash_login_exists: false,
        git_bash_present: false,
        zsh_present: false,
    };
    let t = env_targets(
        EnvContent::new(&one_env(), &[], &[], &Default::default()),
        EnvScope::Interactive,
        home,
        &probe,
        EnvPlatform::Linux,
    );
    let keys = target_keys(&t);
    assert!(
        keys.contains(&"file:/h/.config/fish/conf.d/cfgd-env.fish".to_string()),
        "fish env file expected when fish is present, got: {keys:?}"
    );
}

// --- execute_script idempotency guards: creates / unless / onlyIf short-circuit ---

fn full_guarded_script(
    run: &str,
    creates: Option<String>,
    only_if: Option<String>,
    unless: Option<String>,
) -> ScriptEntry {
    ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: run.to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if,
        unless,
        creates,
        interactive: false,
    })
}

#[test]
fn execute_script_creates_guard_skips_when_path_exists() {
    // `creates:` names an artifact the script produces; if it already exists the
    // script is a no-op (changed=false) and its body never runs.
    let printer = test_printer();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("already-there");
    std::fs::write(&marker, "x").unwrap();
    // Body would create a DIFFERENT file if it ran — its absence proves the skip.
    let entry = full_guarded_script(
        "touch should-not-exist",
        Some(marker.to_string_lossy().into_owned()),
        None,
        None,
    );
    let (_label, changed, _out) = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .unwrap();
    assert!(!changed, "creates guard must mark the run as a no-op");
    assert!(
        !dir.path().join("should-not-exist").exists(),
        "guarded body must not have executed"
    );
}

#[test]
fn execute_script_unless_guard_skips_when_condition_holds() {
    // `unless:` skips when its command succeeds (the guarded state already holds).
    let printer = test_printer();
    let dir = tempfile::tempdir().unwrap();
    let entry = full_guarded_script(
        "touch should-not-exist",
        None,
        None,
        Some("true".to_string()),
    );
    let (_label, changed, _out) = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .unwrap();
    assert!(!changed, "unless-holds must skip the body");
    assert!(!dir.path().join("should-not-exist").exists());
}

#[test]
fn execute_script_only_if_guard_skips_when_condition_unmet() {
    // `onlyIf:` runs the body only when its command succeeds; a failing guard
    // (`false`) skips the body as a clean no-op.
    let printer = test_printer();
    let dir = tempfile::tempdir().unwrap();
    let entry = full_guarded_script(
        "touch should-not-exist",
        None,
        Some("false".to_string()),
        None,
    );
    let (_label, changed, _out) = super::execute_script(
        &entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(10),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .unwrap();
    assert!(!changed, "onlyIf-unmet must skip the body");
    assert!(!dir.path().join("should-not-exist").exists());
}

#[test]
fn apply_env_inject_refuses_a_non_utf8_rc_and_leaves_it_byte_identical() {
    // A latin-1 rc file is a read failure, not an empty file. Degrading to an
    // empty baseline would rewrite the user's whole rc down to cfgd's one line.
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");
    let original: &[u8] = b"# caf\xe9\nexport FOO=bar\n";
    std::fs::write(&rc_path, original).unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };
    let printer = test_printer();
    let err =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap_err();

    assert!(
        err.to_string().contains("not valid UTF-8"),
        "error must name the cause: {err}"
    );
    assert_eq!(
        std::fs::read(&rc_path).unwrap(),
        original,
        "a refused inject must leave the rc byte-identical"
    );
}

#[test]
fn apply_env_write_regenerates_a_corrupt_managed_file() {
    // The target of a managed write is cfgd's own generated file, so unreadable
    // bytes are damage to regenerate, not user content to protect. Refusing
    // instead would wedge every future apply on one stray byte, and the only
    // recovery would be deleting a file the user never wrote.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".cfgd.env");
    std::fs::write(&path, b"\xff\xfe# managed by cfgd\n").unwrap();

    let content = "# managed by cfgd\nexport FOO=\"bar\"\n";
    let action = EnvAction::WriteEnvFile {
        path: path.clone(),
        content: content.to_string(),
        vars: 0,
        aliases: 0,
    };
    let printer = test_printer();
    let desc =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap();

    assert!(
        !desc.ends_with(super::apply::ENV_SKIPPED_SUFFIX),
        "a regenerated file is a change, not a skip: {desc}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn apply_env_inject_propagates_a_non_notfound_read_error() {
    // Reading a directory as a file fails with something other than NotFound —
    // the class of failure (EACCES after an elevated run, EIO) that must abort
    // the write instead of producing an empty baseline.
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join("rc-as-a-directory");
    std::fs::create_dir(&rc_path).unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };
    let printer = test_printer();
    assert!(
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .is_err()
    );
    assert!(rc_path.is_dir(), "the target must be left untouched");
}

#[cfg(unix)]
#[test]
fn apply_env_inject_refuses_an_unreadable_rc() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");
    let original = "export FOO=bar\n";
    std::fs::write(&rc_path, original).unwrap();
    std::fs::set_permissions(&rc_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };
    let printer = test_printer();
    let outcome =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded());

    // The kernel's read check does not apply to uid 0, so what stops an
    // elevated run from rewriting this file is cfgd's own mode-based guard, and
    // that is what the root arm pins. Both arms assert, so neither runner
    // reports green having verified nothing.
    let err = outcome.unwrap_err();
    let expected = if crate::is_root() {
        "read-only"
    } else {
        "permission"
    };
    assert!(
        err.to_string().to_lowercase().contains(expected),
        "error must name the failure ({expected}): {err}"
    );

    std::fs::set_permissions(&rc_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read_to_string(&rc_path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn apply_env_inject_refuses_a_read_only_rc() {
    use std::os::unix::fs::PermissionsExt;

    // A rename lands regardless of the write bit, so a read-only rc would
    // otherwise be replaced silently despite the user marking it untouchable.
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");
    let original = "export FOO=bar\n";
    std::fs::write(&rc_path, original).unwrap();
    std::fs::set_permissions(&rc_path, std::fs::Permissions::from_mode(0o444)).unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };
    let printer = test_printer();
    let err =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
    assert_eq!(std::fs::read_to_string(&rc_path).unwrap(), original);

    std::fs::set_permissions(&rc_path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn guard_rc_write_refuses_an_empty_baseline_over_existing_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");
    std::fs::write(&rc_path, "export FOO=bar\n").unwrap();

    let err = super::env_files::guard_rc_write(&rc_path, "").unwrap_err();
    assert!(err.to_string().contains("refusing to replace"), "{err}");

    // A truthful empty baseline over an absent or empty file still passes.
    assert!(super::env_files::guard_rc_write(&rc_path, "export FOO=bar\n").is_ok());
    let missing = dir.path().join(".zshrc");
    assert!(super::env_files::guard_rc_write(&missing, "").is_ok());
    let empty = dir.path().join(".profile");
    std::fs::write(&empty, "").unwrap();
    assert!(super::env_files::guard_rc_write(&empty, "").is_ok());
}

#[test]
fn merge_source_line_keeps_a_commented_loader_and_a_user_note() {
    // A deliberately disabled loader must not be deleted and re-enabled, and a
    // user's prose mentioning the file is not a loader at all.
    let line = "[ -f ~/.cfgd.env ] && . ~/.cfgd.env";
    let existing = "# ~/.cfgd.env is generated by cfgd\n# [ -f ~/.cfgd.env ] && . ~/.cfgd.env\nexport FOO=bar\n";

    let merged = super::env_files::merge_source_line(existing, line).unwrap();
    assert!(merged.contains("# ~/.cfgd.env is generated by cfgd\n"));
    assert!(merged.contains("# [ -f ~/.cfgd.env ] && . ~/.cfgd.env\n"));
    assert!(merged.contains("export FOO=bar\n"));
    assert!(merged.ends_with(&format!("{line}\n")));
    assert_eq!(
        merged.matches(line).count(),
        2,
        "only the commented form and the appended live one may be present"
    );
}

#[test]
fn merge_source_line_replaces_only_the_live_loader() {
    let line = "[ -f ~/.cfgd.env ] && . ~/.cfgd.env";
    let existing = "# [ -f ~/.cfgd.env ] && source ~/.cfgd.env\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\nexport FOO=bar\n";

    let merged = super::env_files::merge_source_line(existing, line).unwrap();
    assert_eq!(
        merged,
        "# [ -f ~/.cfgd.env ] && source ~/.cfgd.env\n[ -f ~/.cfgd.env ] && . ~/.cfgd.env\nexport FOO=bar\n",
        "the stale live loader is upgraded in place; the commented one is untouched"
    );
}

#[test]
fn merge_source_line_preserves_crlf_and_trailing_blank_lines() {
    let line = "[ -f ~/.cfgd.env ] && . ~/.cfgd.env";
    let existing = "# my config\r\nexport FOO=bar\r\n\r\n\r\n";

    let merged = super::env_files::merge_source_line(existing, line).unwrap();
    assert_eq!(merged, format!("{existing}{line}\r\n"));
    assert!(
        !merged.contains("bar\n\n"),
        "existing CRLF terminators must survive: {merged:?}"
    );
}

#[test]
fn merge_source_line_keeps_the_terminator_of_the_line_it_replaces() {
    let line = "[ -f ~/.cfgd.env ] && . ~/.cfgd.env";
    let existing = "[ -f ~/.cfgd.env ] && source ~/.cfgd.env\r\nexport FOO=bar\n";

    let merged = super::env_files::merge_source_line(existing, line).unwrap();
    assert_eq!(merged, format!("{line}\r\nexport FOO=bar\n"));
}

#[test]
fn apply_env_inject_stores_a_file_backup_for_the_rc() {
    // The injection rewrites a user-owned dotfile, so rollback needs a row
    // holding the pre-write bytes.
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(dir.path());
    let rc_path = dir.path().join(".bashrc");
    let original = "# my config\nexport FOO=bar\n";
    std::fs::write(&rc_path, original).unwrap();

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Env(EnvAction::InjectSourceLine {
                rc_path: rc_path.clone(),
                line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);

    let key = crate::to_posix_fs_key(&rc_path);
    let backups = state.file_backups_after_apply(0).unwrap();
    let row = backups
        .iter()
        .find(|b| b.file_path == key)
        .expect("inject must leave a backup row for the rc file");
    assert!(row.existed);
    assert_eq!(String::from_utf8(row.content.clone()).unwrap(), original);
    assert!(
        std::fs::read_to_string(&rc_path)
            .unwrap()
            .contains(". ~/.cfgd.env")
    );
}

#[test]
fn apply_env_records_one_managed_resource_across_a_converged_second_run() {
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(dir.path());
    let rc_path = dir.path().join(".bashrc");
    std::fs::write(&rc_path, "export FOO=bar\n").unwrap();

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Env(EnvAction::InjectSourceLine {
                rc_path: rc_path.clone(),
                line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    for _ in 0..2 {
        reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
                None,
                &crate::AbortFlag::new(),
            )
            .unwrap();
    }

    let env_rows: Vec<_> = state
        .managed_resources()
        .unwrap()
        .into_iter()
        .filter(|r| r.resource_type == "env")
        .collect();
    assert_eq!(
        env_rows.len(),
        1,
        "a converged second run must not mint a second row: {env_rows:?}"
    );
    assert_eq!(env_rows[0].resource_id, crate::to_posix_string(&rc_path));
}

#[cfg(unix)]
#[test]
fn plan_env_neutralizes_a_stale_managed_file_when_the_desired_env_empties() {
    // Deleting every `spec.env` entry must stop the exports taking effect; the
    // last generated file would otherwise keep exporting them forever.
    let home = tempfile::tempdir().unwrap();
    let env_file = home.path().join(".cfgd.env");
    let neutral = "# managed by cfgd \u{2014} do not edit\n";
    std::fs::write(&env_file, format!("{neutral}export FOO=\"bar\"\n")).unwrap();
    let managed = vec![crate::to_posix_string(&env_file)];

    let actions = Reconciler::plan_env_with_home(
        &[],
        &[],
        &Default::default(),
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &managed,
        home.path(),
    )
    .actions;

    assert_eq!(actions.len(), 1, "{actions:?}");
    match &actions[0] {
        Action::Env(EnvAction::WriteEnvFile {
            path,
            content,
            vars: 0,
            aliases: 0,
        }) => {
            assert_eq!(path, &env_file);
            assert_eq!(content, neutral);
        }
        other => panic!("expected a managed-file rewrite, got {other:?}"),
    }

    // Already neutral: nothing left to strip.
    std::fs::write(&env_file, neutral).unwrap();
    let actions = Reconciler::plan_env_with_home(
        &[],
        &[],
        &Default::default(),
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &managed,
        home.path(),
    )
    .actions;
    assert!(actions.is_empty(), "{actions:?}");

    // A file cfgd's generator did not write is not cfgd's to strip.
    std::fs::write(&env_file, "export FOO=\"user-authored\"\n").unwrap();
    let actions = Reconciler::plan_env_with_home(
        &[],
        &[],
        &Default::default(),
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &managed,
        home.path(),
    )
    .actions;
    assert!(actions.is_empty(), "{actions:?}");
}

#[cfg(unix)]
#[test]
fn plan_env_leaves_a_generated_file_this_state_store_never_recorded() {
    // The gate that keeps a home directory reached from a machine with no
    // record of writing it — every test with a fresh state store included —
    // from having its env file stripped.
    let home = tempfile::tempdir().unwrap();
    let env_file = home.path().join(".cfgd.env");
    let body = "# managed by cfgd \u{2014} do not edit\nexport FOO=\"bar\"\n";
    std::fs::write(&env_file, body).unwrap();

    let actions = Reconciler::plan_env_with_home(
        &[],
        &[],
        &Default::default(),
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        home.path(),
    )
    .actions;

    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), body);
}

#[test]
fn reconciler_env_surfaces_resolve_against_the_home_it_was_built_with() {
    // Every apply-side env path reads the home the reconciler was constructed
    // with, so no call site can reach a home of its own — including the
    // mid-apply regeneration that runs when a secret-backed env var or a
    // bootstrapped PATH directory appears after planning.
    let elsewhere = tempfile::tempdir().unwrap();
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::with_home(&registry, &state, elsewhere.path());
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];

    let actions = reconciler
        .plan_env(
            &env,
            &[],
            &Default::default(),
            EnvScope::Interactive,
            &[],
            &[],
            &[],
            &[],
        )
        .actions;

    assert!(!actions.is_empty(), "the env plan must not be empty");
    for action in &actions {
        let path = match action {
            Action::Env(EnvAction::WriteEnvFile { path, .. }) => path.clone(),
            Action::Env(EnvAction::InjectSourceLine { rc_path, .. }) => rc_path.clone(),
            _ => continue,
        };
        assert!(
            path.starts_with(elsewhere.path()),
            "{} escaped the home the reconciler was built with",
            path.posix()
        );
    }
}

#[cfg(unix)]
#[test]
fn apply_env_inject_backs_up_and_rolls_back_through_a_symlinked_rc() {
    // The write follows the link, so the backup has to read through it too:
    // a link-only row carries no bytes, and rollback would have nothing to put
    // back for exactly the population that symlinks its rc into a dotfile repo.
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(dir.path());
    let repo_rc = dir.path().join("dotfiles/bashrc");
    std::fs::create_dir_all(repo_rc.parent().unwrap()).unwrap();
    let original = "# my config\nexport FOO=bar\n";
    std::fs::write(&repo_rc, original).unwrap();
    let rc_path = dir.path().join(".bashrc");
    std::os::unix::fs::symlink(&repo_rc, &rc_path).unwrap();

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Env(EnvAction::InjectSourceLine {
                rc_path: rc_path.clone(),
                line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
            })],
        )],
        warnings: vec![],
    };
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    assert_eq!(result.status, ApplyStatus::Success);

    let key = crate::to_posix_fs_key(&rc_path);
    let backups = state.file_backups_after_apply(0).unwrap();
    let row = backups
        .iter()
        .find(|b| b.file_path == key)
        .expect("a symlinked rc still needs a backup row");
    assert_eq!(
        String::from_utf8(row.content.clone()).unwrap(),
        original,
        "the row must hold the bytes the write replaced, read through the link"
    );

    assert_eq!(
        super::restore_file_from_backup(&rc_path, row, &printer),
        RestoreOutcome::Restored
    );
    assert!(
        std::fs::symlink_metadata(&rc_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "rollback must not leave a regular file where the dotfile link was"
    );
    assert_eq!(std::fs::read_to_string(&repo_rc).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn apply_env_write_refuses_a_link_redirecting_it_out_of_the_owner_s_tree() {
    // `sudo -E cfgd apply` keeps the invoking user's HOME, so a link that user
    // plants at ~/.cfgd.env decides where an elevated write lands, with content
    // their own `spec.env` supplies. A link may only redirect a write inside
    // the tree its own owner already controls.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _guard = crate::with_test_home_guard(&home);

    let outside = dir.path().join("outside-the-home");
    let original = "root:x:0:0:root:/root:/bin/sh\n";
    std::fs::write(&outside, original).unwrap();
    let env_path = home.join(".cfgd.env");
    std::os::unix::fs::symlink(&outside, &env_path).unwrap();

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content: "# managed by cfgd\nexport EVIL=\"1\"\n".to_string(),
        vars: 0,
        aliases: 0,
    };
    let printer = test_printer();

    if !crate::is_root() {
        // Only root can stage a foreign owner, so unprivileged this asserts the
        // permitted half: link and target share one uid, the write follows.
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .unwrap();
        assert!(std::fs::read_to_string(&outside).unwrap().contains("EVIL"));
        return;
    }

    std::os::unix::fs::chown(&env_path, Some(12345), Some(12345)).unwrap();
    let err =
        Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
            .expect_err("a link out of the owner's tree must be refused");

    assert!(
        err.to_string().contains("refusing to write through it"),
        "the refusal must name its reason: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        original,
        "the redirected-to file must be byte-identical"
    );
    assert!(
        std::fs::symlink_metadata(&env_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "refusing must not degrade to replacing the link"
    );
}

#[cfg(unix)]
#[test]
fn apply_env_inject_writes_through_a_symlinked_rc() {
    // stow and chezmoi leave ~/.bashrc as a link into a dotfile repo. Replacing
    // the link would strand the repo copy and lose the injection at the next
    // re-link, so the injected line has to land in the file the link names.
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(dir.path());
    let repo_rc = dir.path().join("dotfiles/bashrc");
    std::fs::create_dir_all(repo_rc.parent().unwrap()).unwrap();
    std::fs::write(&repo_rc, "export FOO=bar\n").unwrap();
    let rc_path = dir.path().join(".bashrc");
    std::os::unix::fs::symlink(&repo_rc, &rc_path).unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && . ~/.cfgd.env".to_string(),
    };
    let printer = test_printer();
    Reconciler::apply_env_action(&action, &printer, crate::providers::NoteSink::discarded())
        .unwrap();

    assert!(
        std::fs::symlink_metadata(&rc_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the rc symlink must survive the injection"
    );
    let repo_body = std::fs::read_to_string(&repo_rc).unwrap();
    assert_eq!(
        repo_body,
        "export FOO=bar\n[ -f ~/.cfgd.env ] && . ~/.cfgd.env\n"
    );
}

// --- owner groups, Rule P dispatch, and the journal index ---

/// A package manager that appends every bootstrap and install to a log shared
/// with its siblings, so a test can assert the ORDER the reconciler reached
/// them in rather than merely that each ran.
struct DispatchLogManager {
    name: String,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    available: std::sync::Mutex<bool>,
    installed: std::sync::Mutex<HashSet<String>>,
    /// The rendezvous a concurrency test drives this manager's operations
    /// through. `None` for every ordering-only fixture, which then behaves
    /// exactly as it did before lanes existed.
    probe: Option<std::sync::Arc<LaneProbe>>,
    /// Bootstrapping leaves the manager unavailable, so every one of its
    /// actions keeps draining the phase — the "forced to one lane" half of the
    /// concurrent-versus-sequential comparison.
    stays_unavailable: bool,
    /// Write and read this manager's resolved prefix from inside `install`,
    /// which in a lane reaches the coordinator's connection through the proxy.
    touches_state: bool,
    /// Panic inside `install`, so a test can drive the lane-panic path.
    panics: bool,
    /// Lines pushed into the lane around this manager's rendezvous, so a test
    /// can force two lanes to interleave their child output.
    lane_lines: Option<(String, String)>,
    /// Where `bootstrap` writes the method the plan named, for the test that
    /// pins the `via` reaching execution. A slot rather than a log line
    /// because every other fixture asserts on the log's exact contents.
    seen_provision_via: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
}

impl DispatchLogManager {
    fn new(
        name: &str,
        log: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        available: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            log: std::sync::Arc::clone(log),
            available: std::sync::Mutex::new(available),
            installed: std::sync::Mutex::new(HashSet::new()),
            probe: None,
            stays_unavailable: false,
            touches_state: false,
            panics: false,
            lane_lines: None,
            seen_provision_via: None,
        }
    }

    fn with_probe(mut self, probe: &std::sync::Arc<LaneProbe>) -> Self {
        self.probe = Some(std::sync::Arc::clone(probe));
        self
    }

    fn stays_unavailable(mut self) -> Self {
        self.stays_unavailable = true;
        self
    }

    fn with_state_writes(mut self) -> Self {
        self.touches_state = true;
        self
    }

    fn panicking(mut self) -> Self {
        self.panics = true;
        self
    }

    fn with_lane_lines(mut self, first: &str, second: &str) -> Self {
        self.lane_lines = Some((first.to_string(), second.to_string()));
        self
    }

    fn recording_provision_via(
        mut self,
        slot: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> Self {
        self.seen_provision_via = Some(std::sync::Arc::clone(slot));
        self
    }

    fn record(&self, event: String) {
        self.log.lock().unwrap().push(event);
    }

    /// Report this operation to the probe and block while the test holds it.
    fn rendezvous(&self, label: &str) {
        if let Some(probe) = &self.probe {
            probe.enter(label);
        }
    }
}

/// A rendezvous a fixture manager blocks in, so a concurrency test pins the
/// exact interleaving two lanes reach rather than racing for it.
///
/// Every wait is bounded: a scheduler that never dispatches must fail the
/// assertion that follows rather than hang the suite.
#[derive(Default)]
struct LaneProbe {
    state: std::sync::Mutex<LaneProbeState>,
    signal: std::sync::Condvar,
}

#[derive(Default)]
struct LaneProbeState {
    /// `start:<label>` / `end:<label>`, in the order the operations reached
    /// them — the completion order, which is what plan order is not.
    events: Vec<String>,
    in_flight: usize,
    peak: usize,
    held: HashSet<String>,
}

/// How long a probe waits before it gives up and reports what it saw. It turns
/// a hang into a failure, so it is not a rendezvous budget and nothing here
/// spends it on a green run.
///
/// Generous because a lane worker is SPAWNED, and a spawn takes the shared
/// `PATH` guard: another test in this binary holding the exclusive one
/// (`CwdGuard`, any `PATH` mutation) stalls every worker for as long as its
/// body runs, and several such bodies can queue. A wait that expires early
/// reports the harness rather than the code.
///
/// It is not what makes the dispatch tests reliable, and widening it never
/// was: the stall they used to hit was a gate deadlock rather than slowness —
/// a worker inside the read side, a writer queued behind it, and the sibling
/// worker the test is waiting for shut out by that writer. `PATH_ENV_LOCK`'s
/// admission rule is what removed it (see
/// `a_lane_dispatch_is_not_stalled_by_a_test_that_is_waiting_to_mutate_path`);
/// this only decides how long a future one takes to go red.
const LANE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl LaneProbe {
    /// A probe whose named operations block until the test releases them.
    fn holding(labels: &[&str]) -> std::sync::Arc<Self> {
        let probe = Self::default();
        probe.state.lock().unwrap().held = labels.iter().map(|l| (*l).to_string()).collect();
        std::sync::Arc::new(probe)
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, LaneProbeState> {
        self.state.lock().unwrap()
    }

    /// One whole operation: record its start, block while the test holds it,
    /// then record its end.
    fn enter(&self, label: &str) {
        let mut state = self.locked();
        state.events.push(format!("start:{label}"));
        state.in_flight += 1;
        state.peak = state.peak.max(state.in_flight);
        self.signal.notify_all();
        let (mut state, _) = self
            .signal
            .wait_timeout_while(state, LANE_PROBE_TIMEOUT, |s| s.held.contains(label))
            .unwrap();
        state.events.push(format!("end:{label}"));
        state.in_flight -= 1;
        self.signal.notify_all();
    }

    fn release(&self, label: &str) {
        self.locked().held.remove(label);
        self.signal.notify_all();
    }

    fn release_all(&self) {
        self.locked().held.clear();
        self.signal.notify_all();
    }

    /// Wait until `predicate` holds. False on timeout, which every caller
    /// asserts on rather than ignoring.
    fn await_state(&self, predicate: impl Fn(&LaneProbeState) -> bool) -> bool {
        let state = self.locked();
        let (state, _) = self
            .signal
            .wait_timeout_while(state, LANE_PROBE_TIMEOUT, |s| !predicate(s))
            .unwrap();
        predicate(&state)
    }

    fn await_in_flight(&self, n: usize) -> bool {
        self.await_state(|s| s.in_flight >= n)
    }

    fn await_started(&self, label: &str) -> bool {
        let want = format!("start:{label}");
        self.await_state(|s| s.events.contains(&want))
    }

    fn await_finished(&self, label: &str) -> bool {
        let want = format!("end:{label}");
        self.await_state(|s| s.events.contains(&want))
    }

    fn started(&self, label: &str) -> bool {
        let want = format!("start:{label}");
        self.locked().events.contains(&want)
    }

    fn in_flight(&self) -> usize {
        self.locked().in_flight
    }

    fn peak(&self) -> usize {
        self.locked().peak
    }

    fn events(&self) -> Vec<String> {
        self.locked().events.clone()
    }
}

/// Position of a probe event, panicking when it never happened — an ordering
/// assertion over a missing event would otherwise pass vacuously.
fn event_at(events: &[String], event: &str) -> usize {
    events
        .iter()
        .position(|e| e == event)
        .unwrap_or_else(|| panic!("no {event:?} in {events:?}"))
}

impl PackageManager for DispatchLogManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        *self.available.lock().unwrap()
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        Some(crate::providers::BootstrapPlan::new("stub"))
    }
    fn bootstrap(&self, cx: &PackageContext<'_>) -> Result<()> {
        let label = format!("bootstrap:{}", self.name);
        self.record(label.clone());
        if let Some(slot) = &self.seen_provision_via {
            *slot.lock().unwrap() = cx.planned_method().map(str::to_string);
        }
        if !self.stays_unavailable {
            *self.available.lock().unwrap() = true;
        }
        self.rendezvous(&label);
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(self.installed.lock().unwrap().clone())
    }
    fn install(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        let label = format!("{}:{}", self.name, packages.join(","));
        self.record(format!("install:{label}"));
        assert!(!self.panics, "install:{label} panicked");
        if self.touches_state {
            cx.state
                .record_resolved_prefix(&self.name, &format!("/opt/{}", self.name), false)?;
        }
        if let Some((first, _)) = &self.lane_lines
            && let Some(lane) = cx.lane()
        {
            lane.push_line(first);
        }
        self.rendezvous(&label);
        if let Some((_, second)) = &self.lane_lines
            && let Some(lane) = cx.lane()
        {
            lane.push_line(second);
        }
        if self.touches_state {
            let stored = cx.state.resolved_prefix(&self.name)?;
            assert_eq!(
                stored.map(|(prefix, _)| prefix),
                Some(format!("/opt/{}", self.name)),
                "a lane must read back what it wrote through the coordinator"
            );
        }
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.insert(p.clone());
        }
        Ok(())
    }
    fn uninstall(&self, packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.remove(p);
        }
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, cx: &PackageContext<'_>) -> Result<()> {
        // npm's refresh resolves its global prefix from `cx.state`, and an
        // index refresh now runs on a lane like every other action. Nothing
        // is recorded in the log, so every ordering fixture is unaffected.
        if self.touches_state {
            cx.state
                .record_resolved_prefix(&self.name, &format!("/opt/{}", self.name), false)?;
            let stored = cx.state.resolved_prefix(&self.name)?;
            assert_eq!(
                stored.map(|(prefix, _)| prefix),
                Some(format!("/opt/{}", self.name)),
                "an index refresh must read back what it wrote through the coordinator"
            );
        }
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

fn new_dispatch_log() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
}

fn dispatch_log(log: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().unwrap().clone()
}

fn install_action(manager: &str, packages: &[&str]) -> Action {
    Action::Package(PackageAction::Install {
        manager: manager.to_string(),
        packages: packages.iter().map(|p| (*p).to_string()).collect(),
        origin: "local".to_string(),
    })
}

fn uninstall_action(manager: &str, packages: &[&str]) -> Action {
    Action::Package(PackageAction::Uninstall {
        manager: manager.to_string(),
        packages: packages.iter().map(|p| (*p).to_string()).collect(),
        origin: "local".to_string(),
    })
}

fn owner_resolved_package(manager: &str, package: &str) -> ResolvedPackage {
    ResolvedPackage {
        canonical_name: package.to_string(),
        resolved_name: package.to_string(),
        manager: manager.to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    }
}

fn module_install_action(module: &str, manager: &str, package: &str) -> Action {
    Action::Module(ModuleAction {
        module_name: module.to_string(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![owner_resolved_package(manager, package)],
        },
        origin: None,
    })
}

/// A `prefer: [script]` package: the pseudo-manager `script` plus the body the
/// module ships instead of a package name.
fn script_resolved_package(package: &str, script: &str) -> ResolvedPackage {
    ResolvedPackage {
        script: Some(script.to_string()),
        ..owner_resolved_package("script", package)
    }
}

fn module_script_install_action(module: &str, package: &str, script: &str) -> Action {
    Action::Module(ModuleAction {
        module_name: module.to_string(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![script_resolved_package(package, script)],
        },
        origin: None,
    })
}

fn module_for(name: &str, manager: &str, package: &str) -> ResolvedModule {
    module_with(name, &[(manager, package)])
}

/// A resolved module declaring one package per `(manager, package)` pair.
fn module_with(name: &str, packages: &[(&str, &str)]) -> ResolvedModule {
    let mut module = make_resolved_module(name);
    module.packages = packages
        .iter()
        .map(|(manager, package)| owner_resolved_package(manager, package))
        .collect();
    module
}

fn owner_tokens(phase: &Phase) -> Vec<String> {
    phase.groups().iter().map(|g| g.owner.token()).collect()
}

fn packages_phase(actions: Vec<Action>) -> Plan {
    Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("work"),
            actions,
        )],
        warnings: vec![],
    }
}

fn run_apply(
    reconciler: &Reconciler<'_>,
    plan: &Plan,
    modules: &[ResolvedModule],
    filter: Option<&PhaseFilter>,
) -> ApplyResult {
    let resolved = make_empty_resolved();
    let printer = test_printer();
    reconciler
        .apply(
            plan,
            &resolved,
            Path::new("."),
            &printer,
            filter,
            modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply")
}

#[test]
fn interleaved_owner_actions_collapse_into_one_group_each() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("work"),
        vec![
            module_install_action("nvim", "brew", "neovim"),
            install_action("brew", &["ripgrep"]),
            module_install_action("zsh", "brew", "zsh"),
            module_install_action("nvim", "brew", "tree-sitter"),
            install_action("brew", &["fd"]),
        ],
    );

    assert_eq!(
        owner_tokens(&phase),
        vec!["profile:work", "module:nvim", "module:zsh"],
        "one group per owner, in sort_key order, however the actions interleave"
    );
    let nvim = &phase.groups()[1].actions;
    assert_eq!(nvim.len(), 2);
    assert!(
        format_plan_item(&nvim[0]).contains("neovim"),
        "first-appearance order survives inside a group: {:?}",
        plan_items(&phase)
    );
    assert!(format_plan_item(&nvim[1]).contains("tree-sitter"));
    assert_eq!(phase.action_count(), 5, "grouping loses no action");
}

#[test]
fn owner_order_is_profile_first_in_every_phase() {
    let profile = Owner::profile("work");
    let phases = vec![
        Phase::from_actions(
            PhaseName::Packages,
            &profile,
            vec![
                module_install_action("nvim", "brew", "neovim"),
                install_action("brew", &["ripgrep"]),
            ],
        ),
        Phase::from_actions(
            PhaseName::Files,
            &profile,
            vec![
                Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![],
                        declared_total: 0,
                    },
                    origin: None,
                }),
                Action::File(FileAction::Skip {
                    target: PathBuf::from("/home/u/.gitconfig"),
                    reason: "in sync".to_string(),
                    origin: "local".to_string(),
                }),
            ],
        ),
        Phase::from_actions(
            PhaseName::System,
            &profile,
            vec![
                Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::Skip {
                        reason: "platform".to_string(),
                    },
                    origin: None,
                }),
                Action::System(SystemAction::SetValue {
                    configurator: "sysctl".to_string(),
                    key: "net.ipv4.ip_forward".to_string(),
                    desired: "1".to_string(),
                    current: "0".to_string(),
                    origin: "local".to_string(),
                }),
            ],
        ),
    ];

    for phase in &phases {
        assert_eq!(
            owner_tokens(phase),
            vec!["profile:work", "module:nvim"],
            "{} must read profile-first like every other phase",
            phase.name.as_str()
        );
    }
}

#[test]
fn managers_group_is_built_at_rank_one() {
    let phase = Phase::from_actions(
        PhaseName::Prerequisites,
        &Owner::profile("work"),
        vec![
            Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            }),
            module_install_action("nvim", "brew", "neovim"),
        ],
    );

    assert_eq!(
        owner_tokens(&phase),
        vec!["cfgd:managers", "module:nvim"],
        "a manager action is cfgd's, not the profile's whose planner emitted it"
    );
    assert_eq!(phase.groups()[0].actions.len(), 1);
}

#[test]
fn no_manager_action_builds_no_managers_group() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("work"),
        vec![
            install_action("brew", &["ripgrep"]),
            module_install_action("nvim", "brew", "neovim"),
        ],
    );

    assert_eq!(owner_tokens(&phase), vec!["profile:work", "module:nvim"]);
    assert!(
        !phase
            .groups()
            .iter()
            .any(|g| g.owner.kind == OwnerKind::Cfgd),
        "an owner with no actions in a phase produces no group"
    );
}

#[test]
fn module_package_work_dispatches_before_profile_package_work() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(DispatchLogManager::new("brew", &log, true)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![
        install_action("brew", &["fd"]),
        module_install_action("nvim", "brew", "neovim"),
    ]);
    let modules = vec![module_for("nvim", "brew", "neovim")];

    let result = run_apply(&reconciler, &plan, &modules, None);

    assert_eq!(
        dispatch_log(&log),
        vec!["install:brew:neovim", "install:brew:fd"],
        "a module's package work is a barrier ahead of the profile's, whatever the display order"
    );
    assert_eq!(result.status, ApplyStatus::Success);
}

#[test]
fn apply_manager_provision_is_skipped_when_already_available() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(DispatchLogManager::new("brew", &log, true)));

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("test"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    };

    let (result, _) = apply_manager_plan(&registry, &state, &plan);

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
    assert!(
        !dispatch_log(&log).iter().any(|e| e == "bootstrap:brew"),
        "an already-available manager's bootstrap() is never called: {:?}",
        dispatch_log(&log)
    );
}

/// A provision that failed is the run's own verdict that the manager is not on
/// the machine, and it has to outrank every later probe: `is_available()`
/// bottoms out in a path lookup the intervening installs moved, so a manager
/// cfgd has just reported it could not provision can answer "available" one
/// phase later and be spawned into an `ENOENT`. Both package shapes that name a
/// manager are withheld, and neither the install nor the installed-state re-read
/// behind it reaches the binary.
#[test]
fn a_package_action_for_a_manager_whose_provision_failed_is_never_spawned() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        DispatchLogManager::new("brew", &log, false).stays_unavailable(),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::profile("work"),
                vec![Action::Manager(ManagerAction::Provision {
                    manager: "brew".to_string(),
                    via: "stub".to_string(),
                    declared: None,
                    batched: vec![],
                    depends_on: vec![],
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("work"),
                vec![
                    install_action("brew", &["fd"]),
                    module_install_action("nvim", "brew", "neovim"),
                ],
            ),
        ],
        warnings: vec![],
    };
    let modules = vec![module_for("nvim", "brew", "neovim")];

    let result = run_apply(&reconciler, &plan, &modules, None);

    assert!(
        !dispatch_log(&log).iter().any(|e| e.starts_with("install:")),
        "nothing may be handed to a manager this run failed to provision: {:?}",
        dispatch_log(&log)
    );
    let packages: Vec<&ActionResult> = result
        .action_results
        .iter()
        .filter(|r| r.phase == PhaseName::Packages.as_str())
        .collect();
    assert_eq!(packages.len(), 2, "both package rows settled");
    for row in packages {
        let error = row.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("brew is not provisioned"),
            "a withheld package action states the recovery, not an errno: {error}"
        );
    }
}

#[test]
fn action_index_is_the_plan_position_not_the_dispatch_counter() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let build_plan = || Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::profile("work"),
                vec![Action::Manager(ManagerAction::Provision {
                    manager: "brew".to_string(),
                    via: "homebrew installer".to_string(),
                    declared: None,
                    batched: vec![],
                    depends_on: vec![],
                })],
            ),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("work"),
                vec![
                    install_action("brew", &["fd"]),
                    module_install_action("nvim", "brew", "neovim"),
                ],
            ),
            Phase::from_actions(
                PhaseName::Files,
                &Owner::profile("work"),
                vec![Action::File(FileAction::Skip {
                    target: PathBuf::from("/home/u/.gitconfig"),
                    reason: "in sync".to_string(),
                    origin: "local".to_string(),
                })],
            ),
        ],
        warnings: vec![],
    };
    let modules = vec![module_for("nvim", "brew", "neovim")];

    let plan = build_plan();
    let result = run_apply(&reconciler, &plan, &modules, None);
    assert_eq!(result.status, ApplyStatus::Success);

    // `journal_entries` orders by `action_index`, so this vec IS the recorded
    // plan order.
    let entries = state.journal_entries(result.apply_id).unwrap();
    assert_eq!(
        entries.iter().map(|e| e.action_index).collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "the column stays dense across the run"
    );
    let by_plan: Vec<&str> = entries.iter().map(|e| e.resource_id.as_str()).collect();
    assert_eq!(
        by_plan,
        vec![
            "provision:brew",
            "brew:install:fd",
            "nvim:packages:neovim",
            "/home/u/.gitconfig",
        ],
        "indices follow the flattened group order, not dispatch order"
    );

    // Row ids ascend in insertion order, which is dispatch order — and it is a
    // different order, which is what makes the derivation change observable:
    // module-owned Packages work dispatches before the profile's, even though
    // the profile's action was declared first.
    let mut by_dispatch: Vec<(i64, &str)> = entries
        .iter()
        .map(|e| (e.id, e.resource_id.as_str()))
        .collect();
    by_dispatch.sort_by_key(|(id, _)| *id);
    assert_eq!(
        by_dispatch.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
        vec![
            "provision:brew",
            "nvim:packages:neovim",
            "brew:install:fd",
            "/home/u/.gitconfig",
        ]
    );

    // A `--phase`-filtered run indexes only the actions that survive the
    // filter, dense from zero — exactly what the pre-change dispatch counter
    // produced.
    let filtered_plan = build_plan();
    let filtered = run_apply(
        &reconciler,
        &filtered_plan,
        &modules,
        Some(&PhaseFilter::Phase(PhaseName::Files)),
    );
    let filtered_entries = state.journal_entries(filtered.apply_id).unwrap();
    assert_eq!(
        filtered_entries
            .iter()
            .map(|e| (e.action_index, e.resource_id.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "/home/u/.gitconfig")]
    );
}

// --- concurrent package lanes ---

/// An apply driven on a worker thread, so the test thread can steer a fixture's
/// rendezvous while lanes are still in flight.
struct ConcurrentApply {
    registry: ProviderRegistry,
    state: crate::state::StateStore,
    plan: Plan,
    modules: Vec<ResolvedModule>,
    abort: crate::AbortFlag,
}

struct ConcurrentOutcome {
    result: ApplyResult,
    state: crate::state::StateStore,
    transcript: String,
}

impl ConcurrentApply {
    fn new(registry: ProviderRegistry, plan: Plan) -> Self {
        Self {
            registry,
            state: test_state(),
            plan,
            modules: Vec::new(),
            abort: crate::AbortFlag::new(),
        }
    }

    fn with_modules(mut self, modules: Vec<ResolvedModule>) -> Self {
        self.modules = modules;
        self
    }

    /// Drive the run with a flag the test also holds, so a driver closure can
    /// request cancellation at a rendezvous it chose rather than racing a
    /// timer for it.
    fn with_abort(mut self, abort: crate::AbortFlag) -> Self {
        self.abort = abort;
        self
    }

    fn run(self, drive: impl FnOnce()) -> ConcurrentOutcome {
        self.run_watching(|_| drive())
    }

    /// [`ConcurrentApply::run`] with the transcript readable from the driving
    /// thread, for an assertion about what is on screen WHILE the lanes are
    /// still holding rather than about what the run left behind.
    fn run_watching(self, drive: impl FnOnce(&crate::output::DocCapture)) -> ConcurrentOutcome {
        let (printer, cap) = crate::output::Printer::for_test_doc();
        let watch = cap.clone();
        let (result, state) = self.run_on(printer, || drive(&watch));
        ConcurrentOutcome {
            result,
            state,
            transcript: crate::output::strip_ansi(&cap.human()),
        }
    }

    /// The run as a TERMINAL leaves it: the phase's rows are drawn in a live
    /// region, and the transcript is the permanent scrollback they committed
    /// to — what the reader still has once the region is gone.
    fn run_live(self, drive: impl FnOnce()) -> ConcurrentOutcome {
        let (printer, buf) = crate::output::Printer::for_test_live_scrollback();
        let (result, state) = self.run_on(printer, drive);
        let transcript = crate::test_helpers::captured_text(&buf);
        ConcurrentOutcome {
            result,
            state,
            transcript,
        }
    }

    /// Apply on a worker thread with `printer`, running `drive` here meanwhile.
    fn run_on(
        self,
        printer: crate::output::Printer,
        drive: impl FnOnce(),
    ) -> (ApplyResult, crate::state::StateStore) {
        let Self {
            registry,
            state,
            plan,
            modules,
            abort,
        } = self;
        let worker = std::thread::spawn(move || {
            let result = {
                let reconciler = Reconciler::new(&registry, &state);
                reconciler
                    .apply(
                        &plan,
                        &make_empty_resolved(),
                        Path::new("."),
                        &printer,
                        None,
                        &modules,
                        ReconcileContext::Apply,
                        false,
                        None,
                        &abort,
                    )
                    .expect("apply")
            };
            (result, state)
        });
        drive();
        worker.join().expect("apply thread")
    }
}

fn lane_registry(managers: Vec<DispatchLogManager>) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    for manager in managers {
        registry.add_package_manager(Box::new(manager));
    }
    registry
}

#[test]
fn worked_example_nvim_takes_brew_while_tmux_holds_apt() {
    // `tmux` declares apt only; `nvim` declares brew and apt.
    let probe = LaneProbe::holding(&["brew:neovim", "apt:tmux"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("nvim", "apt", "ripgrep"),
        module_install_action("tmux", "apt", "tmux"),
    ]);
    let modules = vec![
        module_with("nvim", &[("brew", "neovim"), ("apt", "ripgrep")]),
        module_for("tmux", "apt", "tmux"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            // 1. `nvim` takes brew, because nothing holds it.
            // 2. `tmux` takes apt.
            assert!(
                driver.await_in_flight(2),
                "two managers, two lanes: {:?}",
                driver.events()
            );
            assert!(driver.started("brew:neovim") && driver.started("apt:tmux"));
            assert!(
                !driver.started("apt:ripgrep"),
                "an owner already holding a lane must not take a second one \
                 while another owner's only manager is idle: {:?}",
                driver.events()
            );

            // 3. brew finishes; `nvim`'s apt work waits, because `tmux` still
            //    holds apt.
            driver.release("brew:neovim");
            assert!(driver.await_finished("brew:neovim"));
            assert!(
                !driver.started("apt:ripgrep"),
                "nvim's apt work started while tmux still held apt: {:?}",
                driver.events()
            );

            // 4. `tmux`'s apt finishes; `nvim`'s apt proceeds.
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(
        event_at(&events, "start:apt:ripgrep") > event_at(&events, "end:apt:tmux"),
        "{events:?}"
    );
    assert_eq!(probe.peak(), 2, "the phase really ran two lanes at once");
}

#[test]
fn one_owners_actions_still_fill_every_free_lane() {
    // The other half of the owner's-turn rule: with no second owner to yield a
    // lane to, one owner's actions run across every manager in the phase, which
    // is the concurrency bound rule 1 states.
    let probe = LaneProbe::holding(&["brew:fd", "apt:curl"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        install_action("brew", &["fd"]),
        install_action("apt", &["curl"]),
    ]);

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan).run(move || {
        assert!(
            driver.await_in_flight(2),
            "one owner, two managers, two lanes: {:?}",
            driver.events()
        );
        driver.release_all();
    });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    assert_eq!(probe.peak(), 2);
}

#[test]
fn an_owners_second_lane_keeps_it_busy_after_its_first_finishes() {
    // `nvim` holds two lanes at once (brew:neovim, apt:ripgrep) and has a
    // third, still-pending action on brew (tree-sitter) that can only become
    // eligible once brew frees. `zsh`'s only action also wants brew and is
    // blocked the same way. Occupancy accounting is what decides who gets the
    // lane brew frees: correct accounting still counts nvim as busy — its
    // apt:ripgrep lane is still running — so zsh, the owner with no lane at
    // all, takes it. A `HashSet` that dropped nvim from `owners_busy` the
    // moment its FIRST lane finished would hand the freed lane back to nvim's
    // own tree-sitter action instead, since that action is earlier in
    // dispatch order than zsh's.
    let probe = LaneProbe::holding(&[
        "brew:neovim",
        "apt:ripgrep",
        "brew:tree-sitter",
        "brew:zshpkg",
    ]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("nvim", "apt", "ripgrep"),
        module_install_action("nvim", "brew", "tree-sitter"),
        module_install_action("zsh", "brew", "zshpkg"),
    ]);
    let modules = vec![
        module_with(
            "nvim",
            &[
                ("brew", "neovim"),
                ("apt", "ripgrep"),
                ("brew", "tree-sitter"),
            ],
        ),
        module_for("zsh", "brew", "zshpkg"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            assert!(
                driver.await_in_flight(2),
                "nvim holds both lanes at once: {:?}",
                driver.events()
            );
            assert!(driver.started("brew:neovim") && driver.started("apt:ripgrep"));
            assert!(
                !driver.started("brew:tree-sitter") && !driver.started("brew:zshpkg"),
                "brew is fully occupied by neovim: {:?}",
                driver.events()
            );

            // Free brew, but leave apt (nvim's second lane) running.
            driver.release("brew:neovim");
            assert!(driver.await_finished("brew:neovim"));
            assert!(
                driver.await_started("brew:zshpkg"),
                "zsh must take the freed lane; nvim is still busy on apt: {:?}",
                driver.events()
            );
            assert!(
                !driver.started("brew:tree-sitter"),
                "brew is exclusive: nvim's own pending action cannot also be running: {:?}",
                driver.events()
            );

            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(
        event_at(&events, "start:brew:zshpkg") < event_at(&events, "start:brew:tree-sitter"),
        "the fresh owner takes the freed lane before nvim's own remaining action: {events:?}"
    );
    assert!(
        event_at(&events, "start:brew:tree-sitter") > event_at(&events, "end:brew:zshpkg"),
        "brew is one lane: nvim's third action only starts once zsh's is done: {events:?}"
    );
}

#[test]
fn profile_packages_never_dispatch_before_module_packages_complete() {
    // The assertion a partition cannot make and a barrier must: the profile's
    // lane is free the whole time and it still does not start.
    let probe = LaneProbe::holding(&["apt:neovim"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        install_action("brew", &["fd"]),
        module_install_action("nvim", "apt", "neovim"),
    ]);
    let modules = vec![module_for("nvim", "apt", "neovim")];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            assert!(driver.await_started("apt:neovim"));
            assert!(
                !driver.started("brew:fd"),
                "tier 1 dispatched while tier 0 was still running: {:?}",
                driver.events()
            );
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(
        event_at(&events, "start:brew:fd") > event_at(&events, "end:apt:neovim"),
        "a tier is released when the tier above COMPLETES: {events:?}"
    );
}

#[test]
fn a_lane_worker_resolves_tilde_against_the_callers_test_home() {
    // `dispatch_package_lanes` spawns each action on a fresh `thread::scope`
    // worker, and a fresh thread does not inherit `TEST_HOME_OVERRIDE` — a
    // thread-local — unless the coordinator explicitly carries it across. A
    // `prefer: [script]` package install resolves its default working
    // directory from `~` (`script_default_workdir`) ON the worker thread, so
    // it is the one production call this dispatcher makes that can prove the
    // override actually made the trip: run the apply synchronously (so this
    // test thread is the one `dispatch_package_lanes` spawns FROM, the same
    // thread the guard below is installed on) and assert the script's own
    // child process actually ran with that directory as its CWD — proof at
    // the OS level, not just a re-read of the Rust-side thread-local.
    let home = tempfile::tempdir().expect("tempdir");
    let _guard = crate::with_test_home_guard(home.path());

    let registry = lane_registry(vec![]);
    let modules = vec![make_resolved_module("toolbox")];
    let plan = packages_phase(vec![module_script_install_action(
        "toolbox",
        "widget",
        "touch marker.txt",
    )]);
    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        home.path().join("marker.txt").exists(),
        "the lane worker's script did not run against the test home {:?}: {:?}",
        home.path(),
        std::fs::read_dir(home.path())
            .map(|entries| entries
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
}

#[test]
fn a_dependents_packages_wait_for_its_dependencys_packages_to_complete() {
    let probe = LaneProbe::holding(&["apt:gcc"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("base", "apt", "gcc"),
    ]);
    let mut nvim = module_for("nvim", "brew", "neovim");
    nvim.depends = vec!["base".to_string()];
    let modules = vec![nvim, module_for("base", "apt", "gcc")];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            assert!(driver.await_started("apt:gcc"));
            assert!(
                !driver.started("brew:neovim"),
                "a dependent started while its dependency was still running: {:?}",
                driver.events()
            );
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(
        event_at(&events, "start:brew:neovim") > event_at(&events, "end:apt:gcc"),
        "{events:?}"
    );
    assert_eq!(probe.peak(), 1, "a declared edge is not concurrency");
}

#[test]
fn a_dispatch_stall_fails_the_run_and_names_the_stuck_action() {
    // Two modules whose `depends` point at each other: neither's
    // `depends_satisfied` can ever be true, so nothing is ever dispatched —
    // `pick_next` returns `None` forever with no worker in flight to unblock
    // it. Before this fix that silently ended the run `Success` (the
    // `running == 0` branch only logged a `tracing::warn!`); the fix collects
    // every still-`Waiting` slot as a failed action, so a stall reads as the
    // failed run it is and names the manager it never got a lane on. Run on a
    // bounded channel rather than joined directly: a regression that turned
    // this back into a real loop must fail the test, not hang the suite.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![DispatchLogManager::new("brew", &log, true)]);
    let plan = packages_phase(vec![
        module_install_action("alpha", "brew", "alpha-pkg"),
        module_install_action("beta", "brew", "beta-pkg"),
    ]);
    let mut alpha = module_for("alpha", "brew", "alpha-pkg");
    alpha.depends = vec!["beta".to_string()];
    let mut beta = module_for("beta", "brew", "beta-pkg");
    beta.depends = vec!["alpha".to_string()];
    let modules = vec![alpha, beta];
    let state = test_state();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reconciler = Reconciler::new(&registry, &state);
        let outcome = run_apply(&reconciler, &plan, &modules, None);
        let _ = tx.send(outcome);
    });
    let result = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a dispatch stall must terminate the run, not hang it");

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(
        result.action_results.len(),
        2,
        "{:?}",
        result.action_results
    );
    for stuck in &result.action_results {
        assert!(!stuck.success, "{stuck:?}");
        assert!(
            stuck
                .error
                .as_deref()
                .is_some_and(|e| e.contains("brew") && e.contains("stalled")),
            "the stuck action's own manager must be named: {stuck:?}"
        );
    }
}

#[test]
#[serial_test::serial]
fn a_lane_worker_blocks_behind_an_exclusively_held_path_lock() {
    // The write half of `PATH_ENV_LOCK` is taken here, on the TEST thread,
    // before `ConcurrentApply` ever spawns the worker that runs
    // `dispatch_package_lanes` — so `path_env_exclusive_guard_held()`'s
    // own-thread precondition check (evaluated on the worker thread) never
    // trips, and the write guard is provably held for the worker's entire
    // dispatch window. If the lane worker takes its own
    // `path_env_read_guard()` before running the action (the fix), it blocks
    // on `PATH_ENV_LOCK` for as long as this thread holds the write half, so
    // `install` cannot have recorded anything by the time `drive()` checks.
    // A worker missing that guard races straight past the held lock and
    // finishes near-instantly instead.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![DispatchLogManager::new("brew", &log, true)]);
    let plan = packages_phase(vec![module_install_action("alpha", "brew", "alpha-pkg")]);
    let modules = vec![module_for("alpha", "brew", "alpha-pkg")];

    let excl = crate::test_helpers::path_env_mutation_guard();
    let drive_log = std::sync::Arc::clone(&log);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            // sleep-ok: correctness here comes from the still-held write guard, not the duration — this only gives a correctly-guarded worker room to reach and block on it
            std::thread::sleep(std::time::Duration::from_millis(150));
            assert!(
                dispatch_log(&drive_log).is_empty(),
                "a lane worker without its own path_env_read_guard() raced \
                 ahead of the held write lock and ran the action anyway: {:?}",
                dispatch_log(&drive_log)
            );
            drop(excl);
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    assert_eq!(
        dispatch_log(&log),
        vec!["install:brew:alpha-pkg".to_string()],
        "the action must still run to completion once the lock is released"
    );
}

#[test]
fn unavailable_manager_action_drains_the_phase() {
    // A manager the registry reports unavailable forces every action naming
    // it to run alone in the phase — provisioning now happens ahead of time,
    // in Prerequisites, so this is the defensive floor for a manager that is
    // STILL unavailable when Packages runs (a provision that failed, or a
    // manager no Prerequisites node ever named).
    let probe = LaneProbe::holding(&["brew:neovim"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("tmux", "apt", "tmux"),
    ]);
    let modules = vec![
        module_for("nvim", "brew", "neovim"),
        module_for("tmux", "apt", "tmux"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            assert!(driver.await_started("brew:neovim"));
            assert_eq!(
                driver.in_flight(),
                1,
                "an action on a not-currently-available manager must run alone: {:?}",
                driver.events()
            );
            assert!(!driver.started("apt:tmux"));
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(event_at(&events, "end:brew:neovim") < event_at(&events, "start:apt:tmux"));
    assert_eq!(probe.peak(), 1);
}

#[test]
// `set_hook` is process-wide: without this, a concurrently running test that
// panics loses its message to the silencer below, and two tests swapping the
// hook race on restoring it.
#[serial_test::serial]
fn a_panicking_lane_fails_its_action_and_the_phase_finishes() {
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).panicking(),
        DispatchLogManager::new("apt", &log, true),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("tmux", "apt", "tmux"),
    ]);
    let modules = vec![
        module_for("nvim", "brew", "neovim"),
        module_for("tmux", "apt", "tmux"),
    ];

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(|| {});
    std::panic::set_hook(hook);

    let failed = outcome
        .result
        .action_results
        .iter()
        .find(|r| !r.success)
        .expect("the panicking lane's action failed");
    assert!(
        failed.error.as_deref().unwrap_or_default().contains("brew"),
        "the failure names the lane's manager: {:?}",
        failed.error
    );
    assert!(
        dispatch_log(&log).contains(&"install:apt:tmux".to_string()),
        "a panicking worker must not stall the coordinator: {:?}",
        dispatch_log(&log)
    );
}

#[test]
fn lane_state_writes_are_serialized_through_the_coordinator() {
    // Both lanes write and read package state while the coordinator is parked
    // in `recv()`: the proxy has to be serviced from the same loop that
    // collects completions, or this deadlocks.
    let probe = LaneProbe::holding(&["brew:neovim", "apt:tmux"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true)
            .with_probe(&probe)
            .with_state_writes(),
        DispatchLogManager::new("apt", &log, true)
            .with_probe(&probe)
            .with_state_writes(),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("tmux", "apt", "tmux"),
    ]);
    let modules = vec![
        module_for("nvim", "brew", "neovim"),
        module_for("tmux", "apt", "tmux"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            assert!(driver.await_in_flight(2), "{:?}", driver.events());
            driver.release_all();
        });

    use crate::providers::PackageStateStore as _;

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    for manager in ["brew", "apt"] {
        assert_eq!(
            outcome
                .state
                .resolved_prefix(manager)
                .unwrap()
                .map(|(prefix, _)| prefix),
            Some(format!("/opt/{manager}")),
            "a lane's write reached the one connection"
        );
    }
}

#[test]
fn rollback_report_reads_in_completion_order_not_plan_order() {
    // Plan order and completion order have to DISAGREE, or the report's
    // ordering column is unobservable. The tier barrier is what makes them
    // disagree without a race: the profile's package sorts first in the plan
    // and dispatches last, because tier 1 is released only once every tier-0
    // action has completed. Forcing the same inversion by holding one lane
    // would leave the last two completions racing for the channel.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true),
        DispatchLogManager::new("apt", &log, true),
    ]);
    let plan = packages_phase(vec![
        install_action("brew", &["fd"]),
        module_install_action("nvim", "apt", "neovim"),
    ]);
    let modules = vec![module_for("nvim", "apt", "neovim")];

    let job = ConcurrentApply::new(registry, plan).with_modules(modules);
    // The apply to roll back TO: the report collects what ran AFTER it.
    let baseline = job
        .state
        .record_apply("test", "hash1", ApplyStatus::Success, None)
        .unwrap();
    let outcome = job.run(|| {});

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let entries = outcome
        .state
        .journal_entries(outcome.result.apply_id)
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.action_index, e.resource_id.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "brew:install:fd"), (1, "nvim:packages:neovim"),],
        "the plan position is unchanged by the dispatch order"
    );

    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &outcome.state);
    let rollback = reconciler
        .rollback_apply(baseline, &test_printer())
        .expect("rollback");
    assert_eq!(
        rollback
            .non_file_actions
            .iter()
            .map(|(_, resource)| resource.as_str())
            .collect::<Vec<_>>(),
        vec!["brew:install:fd", "nvim:packages:neovim"],
        "most recent first is COMPLETION order, not plan order"
    );
    assert_eq!(
        (rollback.files_restored, rollback.files_removed),
        (0, 0),
        "the restore reads file_backups and is untouched by the report's order"
    );
}

#[test]
fn aborted_dispatch_starts_nothing_new_and_records_aborted() {
    let probe = LaneProbe::holding(&["apt:neovim"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    // One lane, two actions: the second cannot already be in flight when the
    // abort lands, so "dispatches nothing new" is observable.
    let plan = packages_phase(vec![
        module_install_action("nvim", "apt", "neovim"),
        module_install_action("tmux", "apt", "tmux"),
    ]);
    let modules = vec![
        module_for("nvim", "apt", "neovim"),
        module_for("tmux", "apt", "tmux"),
    ];

    let job = ConcurrentApply::new(registry, plan).with_modules(modules);
    let abort = job.abort.clone();
    let driver = std::sync::Arc::clone(&probe);
    let outcome = job.run(move || {
        assert!(driver.await_started("apt:neovim"));
        abort.set(130);
        driver.release_all();
    });

    assert_eq!(outcome.result.status, ApplyStatus::Aborted);
    assert!(
        !probe.started("apt:tmux"),
        "an action dispatched after the abort: {:?}",
        probe.events()
    );
    assert!(
        probe.events().contains(&"end:apt:neovim".to_string()),
        "an in-flight lane still finishes: {:?}",
        probe.events()
    );
}

#[test]
fn concurrent_phase_tree_matches_sequential_tree() {
    let tree_for = |forced_to_one_lane: bool| {
        let log = new_dispatch_log();
        let manager = |name: &str| {
            let m = DispatchLogManager::new(name, &log, !forced_to_one_lane);
            if forced_to_one_lane {
                m.stays_unavailable()
            } else {
                m
            }
        };
        let registry = lane_registry(vec![manager("brew"), manager("apt")]);
        let plan = packages_phase(vec![
            module_install_action("nvim", "brew", "neovim"),
            module_install_action("tmux", "apt", "tmux"),
        ]);
        let modules = vec![
            module_for("nvim", "brew", "neovim"),
            module_for("tmux", "apt", "tmux"),
        ];
        let outcome = ConcurrentApply::new(registry, plan)
            .with_modules(modules)
            .run(|| {});
        assert_eq!(outcome.result.status, ApplyStatus::Success);
        packages_tree(&outcome.transcript)
    };

    assert_eq!(
        tree_for(false),
        tree_for(true),
        "the phase tree is the plan's shape, never the dispatch's"
    );
}

/// The `Packages` phase block of a transcript, with elapsed times folded — the
/// tree, without the run header the index refresh differs in.
fn packages_tree(transcript: &str) -> Vec<String> {
    let normalized = crate::normalize_snapshot_durations(transcript);
    normalized
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("Phase: Packages"))
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn non_tty_concurrent_phase_captures_not_streams() {
    // Two lanes interleave their child output in TIME; off a TTY each action's
    // output has to come back as one contiguous block, or a CI log and every
    // golden are non-deterministic. The capture path is reachable here without
    // a redirected suite, because a test printer pins its live region off.
    let probe = LaneProbe::holding(&["brew:neovim", "apt:tmux"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true)
            .with_probe(&probe)
            .with_lane_lines("brew-line-one", "brew-line-two"),
        DispatchLogManager::new("apt", &log, true)
            .with_probe(&probe)
            .with_lane_lines("apt-line-one", "apt-line-two"),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("tmux", "apt", "tmux"),
    ]);
    let modules = vec![
        module_for("nvim", "brew", "neovim"),
        module_for("tmux", "apt", "tmux"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            // Both lanes have written their FIRST line by now, so a streaming
            // lane would have put them side by side.
            assert!(driver.await_in_flight(2), "{:?}", driver.events());
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let lines = transcript_lines(&outcome.transcript);
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no {needle:?} in {lines:?}"))
    };
    assert_eq!(
        at("brew-line-two"),
        at("brew-line-one") + 1,
        "a lane's body is one block: {lines:?}"
    );
    assert_eq!(
        at("apt-line-two"),
        at("apt-line-one") + 1,
        "a lane's body is one block: {lines:?}"
    );
    assert!(
        at("brew install neovim") < at("brew-line-one"),
        "the body sits beneath the action it belongs to: {lines:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_script_install_reports_through_its_lane() {
    // A `prefer: [script]` install is the one package arm whose child process
    // cfgd spawns itself, so it is the arm most likely to be routed at the
    // printer by hand. Through the printer its body streams at ambient phase
    // depth WHILE the action runs — above the line naming it, interleaved with
    // every other lane — and the script settles a second status line beside the
    // coordinator's. Through the lane it does neither.
    let plan = packages_phase(vec![module_script_install_action(
        "nvim",
        "pynvim",
        "echo script-body-line",
    )]);
    let mut module = make_resolved_module("nvim");
    module.packages = vec![script_resolved_package("pynvim", "echo script-body-line")];

    let outcome = ConcurrentApply::new(lane_registry(vec![]), plan)
        .with_modules(vec![module])
        .run(|| {});

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let lines = transcript_lines(&outcome.transcript);
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no {needle:?} in {lines:?}"))
    };
    assert!(
        at("pynvim") < at("script-body-line"),
        "the script's body sits beneath the action it belongs to: {lines:?}"
    );
    assert_eq!(
        status_line_count(&outcome.transcript),
        1,
        "the action's one status line is the coordinator's: {lines:?}"
    );
}

#[test]
fn a_failing_laned_script_install_reports_its_own_exit_status() {
    // Inside a lane the script settles no line of its own, so the coordinator's
    // is the ONLY one the reader gets. A mapped error that discarded the
    // script's message would open with "something failed" and say nothing about
    // what — the captured body below it is not a substitute for the first line.
    let plan = packages_phase(vec![module_script_install_action(
        "nvim", "pynvim", "exit 3",
    )]);
    let mut module = make_resolved_module("nvim");
    module.packages = vec![script_resolved_package("pynvim", "exit 3")];

    let outcome = ConcurrentApply::new(lane_registry(vec![]), plan)
        .with_modules(vec![module])
        .run(|| {});

    assert_eq!(outcome.result.status, ApplyStatus::Failed);
    let failure = outcome
        .result
        .action_results
        .iter()
        .find(|r| !r.success)
        .and_then(|r| r.error.clone())
        .unwrap_or_else(|| panic!("no failed action in {:?}", outcome.result.action_results));
    assert!(
        failure.contains("exit 3"),
        "the coordinator's error carries the script's own exit status: {failure}"
    );
    let settled = crate::test_helpers::settled_status_lines(&outcome.transcript);
    assert!(
        settled.iter().any(|l| l.contains("exit 3")),
        "and reaches the action's status line: {settled:?}"
    );
}

#[test]
fn a_live_region_commits_each_lane_action_once_and_in_dispatch_order() {
    // The whole contract, through the real dispatcher: `tmux`'s apt work is
    // released FIRST and still commits second, because `nvim`'s brew work was
    // dispatched ahead of it and a row never moves. Each line is written
    // exactly once — the tree settles what it drew, so `emit_phase_tree` has
    // nothing left to re-emit.
    //
    // `zsh` is the rendezvous rather than a third subject: it shares tmux's
    // apt lane, and the coordinator collects a finished action — settling its
    // row — BEFORE the dispatch pass that can hand that lane to the next
    // action. So `start:apt:zsh` is the observable that tmux is SETTLED,
    // where `end:apt:tmux` says only that the manager's `install` returned.
    // Waiting on the weaker one leaves "nvim still running while tmux is
    // already settled" to timing; waiting on this one establishes it. Its name
    // sorts last because groups render in `Owner::sort_key` order, which is
    // the dispatch order this test is about.
    let probe = LaneProbe::holding(&["brew:neovim", "apt:tmux", "apt:zsh"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("tmux", "apt", "tmux"),
        module_install_action("zsh", "apt", "zsh"),
    ]);
    let modules = vec![
        module_for("nvim", "brew", "neovim"),
        module_for("tmux", "apt", "tmux"),
        module_for("zsh", "apt", "zsh"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run_live(move || {
            assert!(driver.await_in_flight(2), "{:?}", driver.events());
            driver.release("apt:tmux");
            assert!(driver.await_started("apt:zsh"), "{:?}", driver.events());
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let transcript = &outcome.transcript;
    for once in [
        "brew install neovim",
        "apt install tmux",
        "apt install zsh",
        "module:nvim",
        "module:tmux",
        "module:zsh",
    ] {
        assert_eq!(
            transcript.matches(once).count(),
            1,
            "{once:?} reached the scrollback twice: {transcript}"
        );
    }
    let at = |needle: &str| {
        transcript
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle:?} in {transcript}"))
    };
    assert!(
        at("brew install neovim") < at("apt install tmux"),
        "the scrollback followed completion order rather than dispatch order: {transcript}"
    );
    assert!(
        at("apt install tmux") < at("apt install zsh"),
        "the rest of the phase followed the head out of order: {transcript}"
    );
}

#[test]
fn a_lane_dispatch_is_not_stalled_by_a_test_that_is_waiting_to_mutate_path() {
    // The shape that made the ordering test above flaky under a loaded suite.
    // Every lane worker takes the shared `PATH` guard for its whole body, so a
    // worker parked in this probe is a READER that cannot leave until the
    // dispatch moves on — and the dispatch cannot move on until the NEXT
    // worker, a fresh thread taking a real read, starts. Any of the binary's
    // `PATH`-mutating tests queueing a writer between those two acquisitions
    // used to shut the second one out, deadlocking the probe, the writer and
    // every other reader until the probe's timeout expired a minute later.
    let probe = LaneProbe::holding(&["brew:neovim", "apt:tmux", "apt:zsh"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("tmux", "apt", "tmux"),
        module_install_action("zsh", "apt", "zsh"),
    ]);
    let modules = vec![
        module_for("nvim", "brew", "neovim"),
        module_for("tmux", "apt", "tmux"),
        module_for("zsh", "apt", "zsh"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run_live(move || {
            assert!(driver.await_in_flight(2), "{:?}", driver.events());
            let mutator = std::thread::spawn(|| {
                let _exclusive = crate::test_helpers::path_env_mutation_guard();
            });
            assert!(
                crate::test_helpers::await_queued_path_writer(LANE_PROBE_TIMEOUT),
                "the mutating test never reached the gate"
            );
            // `zsh` is dispatched onto the lane `tmux` frees, so its worker
            // takes its read guard with the writer already queued and `neovim`
            // still inside.
            driver.release("apt:tmux");
            assert!(driver.await_started("apt:zsh"), "{:?}", driver.events());
            driver.release_all();
            mutator.join().expect("the mutation window closes");
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
}

#[test]
fn a_live_region_commits_swept_dependents_once_in_dispatch_order() {
    // Concern 3 of the elision re-review: the only test driving
    // `fail_dependents` (`a_failed_node_fails_its_dependents_with_the_root_cause`)
    // runs off a TTY, where `settles_in_place == false` and `PhaseTree::settled`
    // is never reached — so no test proved the real `LaneCollector`/
    // `fail_dependents` wiring reaches a LIVE tree at all. This re-drives the
    // same failure — brew's provision fails, npm and pnpm sweep behind it,
    // neither ever dispatched — through the real dispatcher with a live
    // region, end to end through `apply.rs`'s own settle closure. (The
    // `held_unseen()` summary claim — that the swept rows are counted while
    // genuinely held, not yet committed — is pinned deterministically in
    // `lanes.rs`'s own test module, where the tree's head can be pinned
    // Running without a race.)
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).stays_unavailable(),
        DispatchLogManager::new("npm", &log, false),
        DispatchLogManager::new("pnpm", &log, false),
    ]);
    let plan = prerequisites_phase(vec![
        provision_node("brew", "curl", &[]),
        provision_node("npm", "brew", &[ManagerAction::provision_node("brew")]),
        provision_node("pnpm", "npm", &[ManagerAction::provision_node("npm")]),
    ]);

    let outcome = ConcurrentApply::new(registry, plan).run_live(|| {});
    assert_eq!(outcome.result.status, ApplyStatus::Failed);
    let transcript = &outcome.transcript;
    for once in ["provision npm via brew", "provision pnpm via npm"] {
        assert_eq!(
            transcript.matches(once).count(),
            1,
            "{once:?} did not commit exactly once: {transcript}"
        );
    }
    assert_eq!(
        transcript
            .matches("did not run — provision brew via curl failed earlier in this phase")
            .count(),
        2,
        "both dependents must name the root cause: {transcript}"
    );
    let at = |needle: &str| {
        transcript
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle:?} in {transcript}"))
    };
    assert!(
        at("provision npm via brew") < at("provision pnpm via npm"),
        "the sweep did not commit in dispatch order: {transcript}"
    );
}

#[test]
fn wait_line_never_reaches_the_transcript() {
    let probe = LaneProbe::holding(&["apt:tmux"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, true).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("nvim", "brew", "neovim"),
        module_install_action("nvim", "apt", "ripgrep"),
        module_install_action("tmux", "apt", "tmux"),
    ]);
    let modules = vec![
        module_with("nvim", &[("brew", "neovim"), ("apt", "ripgrep")]),
        module_for("tmux", "apt", "tmux"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            // nvim's apt work is genuinely blocked here — the state a wait line
            // describes — and it still leaves no trace in the transcript.
            assert!(driver.await_started("apt:tmux"));
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    assert!(
        !outcome.transcript.contains("waiting on"),
        "a wait line is live-region only: {}",
        outcome.transcript
    );
}

/// Position of a phase in the plan, panicking when the plan does not hold it —
/// an ordering assertion on a missing phase would otherwise pass vacuously.
fn phase_index(plan: &Plan, name: PhaseName) -> usize {
    plan.phases
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("plan holds no {} phase", name.as_str()))
}

#[test]
fn profile_files_precede_system_phase() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(
        MockSystemConfigurator::new("systemdUnits").with_drift(vec![
            crate::providers::SystemDrift {
                key: "cfgd-agent.service".to_string(),
                expected: "enabled".to_string(),
                actual: "disabled".to_string(),
            },
        ]),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.system.insert(
        "systemdUnits".to_string(),
        serde_yaml::from_str("{cfgd-agent.service: enabled}").unwrap(),
    );

    let plan = reconciler
        .plan(
            &resolved,
            vec![FileAction::Create {
                source: PathBuf::from("/src/cfgd-agent.service"),
                target: PathBuf::from("/etc/systemd/system/cfgd-agent.service"),
                origin: "local".to_string(),
                strategy: crate::config::FileStrategy::default(),
                source_hash: None,
                patch: None,
            }],
            Vec::new(),
            Vec::new(),
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(
        phase_index(&plan, PhaseName::Files) < phase_index(&plan, PhaseName::System),
        "a unit file must be on disk before the configurator that enables it runs: {:?}",
        plan.phases
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn deployed_unit_file_precedes_systemd_enable() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(
        MockSystemConfigurator::new("systemdUnits").with_drift(vec![
            crate::providers::SystemDrift {
                key: "cfgd-agent.service".to_string(),
                expected: "enabled".to_string(),
                actual: "disabled".to_string(),
            },
        ]),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.system.insert(
        "systemdUnits".to_string(),
        serde_yaml::from_str("{cfgd-agent.service: enabled}").unwrap(),
    );

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("cfgd-agent.service");
    std::fs::write(&source, "[Unit]").unwrap();

    let mut module = make_resolved_module("agent");
    module.packages = vec![];
    module.files = vec![ResolvedFile {
        source,
        target: PathBuf::from("/etc/systemd/system/cfgd-agent.service"),
        is_git_source: false,
        strategy: None,
        encryption: None,
        permissions: None,
        patch: None,
    }];

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let files = phase_index(&plan, PhaseName::Files);
    assert!(
        files < phase_index(&plan, PhaseName::System),
        "a module-deployed unit file must precede the systemdUnits action for the same unit: {:?}",
        plan.phases
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
    );
    let deploy = plan.phases[files]
        .actions()
        .next()
        .expect("the Files phase holds the module deploy");
    assert!(format_plan_item(deploy).contains("cfgd-agent.service"));
}

#[test]
fn retain_actions_drops_the_groups_it_empties() {
    let profile = Owner::profile("work");
    let mut phase = Phase::from_actions(
        PhaseName::Packages,
        &profile,
        vec![
            install_action("brew", &["ripgrep"]),
            Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            }),
            module_install_action("nvim", "brew", "neovim"),
        ],
    );

    phase.retain_actions(|a| !matches!(a, Action::Manager(_)));

    assert_eq!(
        phase
            .groups()
            .iter()
            .map(|g| g.owner.token())
            .collect::<Vec<_>>(),
        vec!["profile:work", "module:nvim"],
        "the emptied cfgd:managers group must not survive as a zero-action group"
    );
    assert_eq!(phase.action_count(), 2);
}

#[test]
fn retain_actions_and_batches_shrinks_a_batch_before_dropping_it() {
    // A filter that names ONE package must not take the whole batch with it:
    // the action survives carrying the packages that passed, and is dropped
    // only when nothing is left to install.
    let profile = Owner::profile("work");
    let mut phase = Phase::from_actions(
        PhaseName::Packages,
        &profile,
        vec![
            install_action("brew", &["ripgrep", "fd"]),
            uninstall_action("brew", &["exa"]),
            module_install_action("nvim", "brew", "neovim"),
        ],
    );

    phase.retain_actions_and_batches(
        |_| true,
        |manager, package| !(manager == "brew" && matches!(package, "fd" | "exa" | "neovim")),
        |_| true,
    );

    assert_eq!(
        owner_tokens(&phase),
        vec!["profile:work"],
        "both emptied batches drop their action, and the module group with it"
    );
    let Action::Package(PackageAction::Install { packages, .. }) = phase
        .actions()
        .next()
        .expect("the shrunk install batch survives")
    else {
        panic!("the survivor is the install batch");
    };
    assert_eq!(packages, &vec!["ripgrep".to_string()]);
}

#[test]
fn a_withheld_file_leaves_the_declared_set_with_it() {
    // Pruning one file from a two-file batch must not leave the survivor
    // rendering `1 already deployed` — that shape claims the other file CONVERGED
    // when it was withheld by a pending decision. The declared set shrinks
    // with the batch, so the render and the persisted id both describe the
    // batch that remains.
    let resolved_file = |target: &str| ResolvedFile {
        source: std::path::PathBuf::from("/src").join(target),
        target: std::path::PathBuf::from("/dst").join(target),
        is_git_source: false,
        strategy: Some(crate::config::FileStrategy::Copy),
        encryption: None,
        permissions: None,
        patch: None,
    };
    let files = vec![resolved_file("kept.txt"), resolved_file("withheld.txt")];
    let mut phase = Phase::from_actions(
        PhaseName::Files,
        &Owner::profile("work"),
        vec![Action::Module(ModuleAction {
            module_name: "mymod".to_string(),
            kind: ModuleActionKind::DeployFiles {
                declared_total: files.len(),
                files,
            },
            origin: None,
        })],
    );

    phase.retain_actions_and_batches(
        |_| true,
        |_, _| true,
        |target| !target.ends_with("withheld.txt"),
    );

    let action = phase.actions().next().expect("the shrunk batch survives");
    assert_eq!(format_action_description(action), "module:mymod:files:1");
    let item = format_plan_item(action);
    assert!(
        !item.contains("already deployed"),
        "the survivor must not claim a converged sibling, got: {item}"
    );
}

#[test]
fn retain_actions_leaves_an_already_empty_batch_exactly_as_it_found_it() {
    // `retain_actions` retains every package, so it must stay a pure
    // action-level filter: only a batch the filter EMPTIED loses its action.
    let profile = Owner::profile("work");
    let mut phase = Phase::from_actions(
        PhaseName::Packages,
        &profile,
        vec![install_action("brew", &[]), install_action("apt", &["fd"])],
    );

    phase.retain_actions(|_| true);

    assert_eq!(phase.action_count(), 2);
}

#[test]
fn retain_groups_keeps_the_surviving_owners_in_sort_key_order() {
    let profile = Owner::profile("work");
    let mut phase = Phase::from_actions(
        PhaseName::Packages,
        &profile,
        vec![
            install_action("brew", &["ripgrep"]),
            Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            }),
            module_install_action("nvim", "brew", "neovim"),
            module_install_action("apt-mod", "apt", "fd"),
        ],
    );

    phase.retain_groups(|owner| owner.kind != OwnerKind::Profile);

    assert_eq!(
        phase
            .groups()
            .iter()
            .map(|g| g.owner.token())
            .collect::<Vec<_>>(),
        vec!["cfgd:managers", "module:apt-mod", "module:nvim"],
    );
}

#[test]
fn groups_mut_cannot_reorder_the_owners_it_edits() {
    // The mutable view hands out an owner's actions, never the group vec, so a
    // caller can empty or rewrite a group but not move one past another.
    let profile = Owner::profile("work");
    let mut phase = Phase::from_actions(
        PhaseName::Packages,
        &profile,
        vec![
            install_action("brew", &["ripgrep"]),
            module_install_action("nvim", "brew", "neovim"),
        ],
    );

    for (owner, actions) in phase.groups_mut() {
        if owner.kind == OwnerKind::Profile {
            actions.clear();
        }
    }
    phase.prune_empty_groups();

    assert_eq!(
        phase
            .groups()
            .iter()
            .map(|g| g.owner.token())
            .collect::<Vec<_>>(),
        vec!["module:nvim"],
    );
}

#[test]
fn to_hash_string_is_stable_across_group_permutation() {
    let profile = Owner::profile("work");
    // Group ORDER is not permutable — `Phase::from_actions` is the only
    // constructor and always sorts. What a caller still controls is the order
    // actions arrive in, which sets both the walk order and each group's
    // internal order, so that is the permutation the hash must ignore. The
    // Provision node lives in its own Prerequisites phase — the planner never
    // puts one in Packages — so only the Packages actions are permuted here.
    let prereq_actions = || vec![provision_node("brew", "homebrew installer", &[])];
    let package_actions = || {
        vec![
            install_action("brew", &["ripgrep"]),
            module_install_action("nvim", "brew", "neovim"),
            install_action("apt", &["fd"]),
        ]
    };
    let permuted_package_actions = || {
        let mut a = package_actions();
        a.reverse();
        a
    };

    let plan = Plan {
        phases: vec![
            Phase::from_actions(PhaseName::Prerequisites, &profile, prereq_actions()),
            Phase::from_actions(PhaseName::Packages, &profile, package_actions()),
        ],
        warnings: vec![],
    };

    let permuted = Plan {
        phases: vec![
            Phase::from_actions(PhaseName::Prerequisites, &profile, prereq_actions()),
            Phase::from_actions(PhaseName::Packages, &profile, permuted_package_actions()),
        ],
        warnings: vec![],
    };

    let walk: Vec<String> = plan.phases[1].actions().map(format_plan_item).collect();
    let permuted_walk: Vec<String> = permuted.phases[1].actions().map(format_plan_item).collect();
    assert_ne!(
        walk, permuted_walk,
        "the fixture must actually permute the walk order, or the assertion below is vacuous"
    );

    assert_eq!(
        plan.to_hash_string(),
        permuted.to_hash_string(),
        "the hash identifies the SET of planned actions, not the walk order"
    );
}

// --- the Prerequisites phase's cfgd:managers DAG ---

fn prerequisites_phase(actions: Vec<Action>) -> Plan {
    Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("work"),
            actions,
        )],
        warnings: vec![],
    }
}

fn provision_node(manager: &str, via: &str, depends_on: &[String]) -> Action {
    Action::Manager(ManagerAction::Provision {
        manager: manager.to_string(),
        via: via.to_string(),
        declared: None,
        batched: vec![],
        depends_on: depends_on.to_vec(),
    })
}

fn prerequisite_node(tool: &str, installer: &str, required_by: &[&str]) -> Action {
    Action::Manager(ManagerAction::Prerequisite {
        tool: tool.to_string(),
        installer: installer.to_string(),
        required_by: required_by.iter().map(|m| (*m).to_string()).collect(),
        depends_on: Vec::new(),
    })
}

/// Apply a manager-node plan on this thread, returning the run, the tree it
/// rendered and the state it wrote — the shape for every node test that needs
/// no rendezvous.
/// A skipped action is not a success. The footer counted `13 actions
/// succeeded` over a tree where one of the thirteen wore the skip dash, and
/// the stored column said the same, because the tally read `!failed`. Both
/// halves — what the reader sees and what the row records — carry the split.
#[test]
fn an_apply_with_a_skipped_action_renders_and_stores_the_split() {
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));
    let state = test_state();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("work"),
            vec![
                Action::Package(PackageAction::Install {
                    manager: "brew".to_string(),
                    packages: vec!["ripgrep".to_string()],
                    origin: "local".to_string(),
                }),
                Action::Package(PackageAction::Skip {
                    manager: "apt".to_string(),
                    reason: "not available on this host".to_string(),
                    origin: "local".to_string(),
                }),
            ],
        )],
        warnings: vec![],
    };

    let (printer, cap) = crate::output::Printer::for_test_doc();
    let mut exec = ReconcilerExecutor {
        reconciler: &reconciler,
        resolved: &resolved,
        modules: &[],
    };
    let result = crate::reconciler::ApplyRun::new(
        crate::reconciler::RunContext {
            title: crate::reconciler::RunTitle::Apply,
            config_path: None,
            profile: Some("work"),
            sources: &[],
            modules: &[],
            profile_inherits: &[],
            trigger: None,
            subject: None,
            unit_source: None,
        },
        &plan,
    )
    .execute(&printer, crate::reconciler::Confirm::Skip, &mut exec)
    .expect("apply");
    let out = crate::output::strip_ansi(&cap.human());
    let crate::reconciler::RunDisposition::Applied { result, .. } = result else {
        panic!("the run applied its plan: {out}");
    };

    assert_eq!(result.succeeded(), 1, "{:?}", result.action_results);
    assert_eq!(result.skipped(), 1, "{:?}", result.action_results);
    assert_eq!(result.failed(), 0);
    assert!(
        out.contains("1 action succeeded") && out.contains("\u{2205} 1 skipped"),
        "the footer must not claim a skip as work done, and states it on its \
         own line in the role the skipped row wore: {out}"
    );

    let stored = state
        .get_apply(result.apply_id)
        .unwrap()
        .and_then(|record| record.summary)
        .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&stored).expect("summary json");
    assert_eq!(parsed["succeeded"], 1, "stored: {stored}");
    assert_eq!(parsed["skipped"], 1, "stored: {stored}");
    assert_eq!(parsed["total"], 2, "stored: {stored}");
    assert_eq!(
        crate::state::ApplySummary::prose(&stored),
        "1 succeeded, 1 skipped"
    );
}

fn apply_manager_plan(
    registry: &ProviderRegistry,
    state: &crate::state::StateStore,
    plan: &Plan,
) -> (ApplyResult, String) {
    apply_manager_plan_at(registry, state, plan, crate::output::Verbosity::Quiet)
}

/// `apply_manager_plan` at a chosen verbosity, for a test reading a settled
/// row's DETAIL: `Quiet` suppresses the human tree and keeps only what fails.
fn apply_manager_plan_at(
    registry: &ProviderRegistry,
    state: &crate::state::StateStore,
    plan: &Plan,
    verbosity: crate::output::Verbosity,
) -> (ApplyResult, String) {
    let (printer, buf) = Printer::for_test_at(verbosity);
    let reconciler = Reconciler::new(registry, state);
    let result = reconciler
        .apply(
            plan,
            &make_empty_resolved(),
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");
    (result, crate::test_helpers::captured_text(&buf))
}

#[test]
fn a_node_waits_for_the_node_it_names() {
    // The edge `apt(index) -> curl(prereq) -> brew(provision)`, minus the
    // refresh: brew's provision may not begin while the tool its cascade shells
    // out to is still being installed.
    let probe = LaneProbe::holding(&["apt:curl"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
        DispatchLogManager::new("brew", &log, false).with_probe(&probe),
    ]);
    let plan = prerequisites_phase(vec![
        prerequisite_node("curl", "apt", &["brew"]),
        provision_node("brew", "curl", &[ManagerAction::prereq_node("curl")]),
    ]);

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan).run(move || {
        assert!(
            driver.await_started("apt:curl"),
            "the prerequisite never ran: {:?}",
            driver.events()
        );
        assert!(
            !driver.started("bootstrap:brew"),
            "a provision started while the prerequisite it named was still \
             running: {:?}",
            driver.events()
        );
        driver.release_all();
    });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(
        event_at(&events, "start:bootstrap:brew") > event_at(&events, "end:apt:curl"),
        "{events:?}"
    );
    assert_eq!(probe.peak(), 1, "a declared edge is not concurrency");
}

#[test]
fn independent_provisions_run_concurrently() {
    // The drain rule removed. Both managers are ABSENT — exactly the condition
    // `drains_phase` keys on — so under the `Packages` sub-gate each would wait
    // for an empty phase and the graph would run one node at a time. Nothing
    // connects them, so the phase whose purpose is provisioning runs them at
    // once.
    let probe = LaneProbe::holding(&["bootstrap:brew", "bootstrap:cargo"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).with_probe(&probe),
        DispatchLogManager::new("cargo", &log, false).with_probe(&probe),
    ]);
    let plan = prerequisites_phase(vec![
        provision_node("brew", "curl", &[]),
        provision_node("cargo", "rustup", &[]),
    ]);

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan).run(move || {
        assert!(
            driver.await_in_flight(2),
            "two unconnected provisions, two lanes: {:?}",
            driver.events()
        );
        driver.release_all();
    });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    assert_eq!(probe.peak(), 2, "the phase really provisioned two at once");
}

#[test]
fn two_nodes_on_one_manager_share_its_lane() {
    // Mutual exclusion survives the drain rule's removal: two prerequisites
    // installed by apt are two `apt install` commands, and one manager runs one
    // command at a time whatever the graph says.
    let probe = LaneProbe::holding(&["apt:curl"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = prerequisites_phase(vec![
        prerequisite_node("curl", "apt", &["brew"]),
        prerequisite_node("git", "apt", &["cargo"]),
    ]);

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan).run(move || {
        assert!(driver.await_started("apt:curl"), "{:?}", driver.events());
        assert!(
            !driver.started("apt:git"),
            "two nodes ran one manager's command at once: {:?}",
            driver.events()
        );
        driver.release_all();
    });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    assert_eq!(probe.peak(), 1);
}

#[test]
fn a_failed_node_fails_its_dependents_with_the_root_cause() {
    // brew's provision fails, so neither npm nor pnpm — which install
    // through it, one at a remove — runs at all, and each names brew rather
    // than the link above it.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).stays_unavailable(),
        DispatchLogManager::new("npm", &log, false),
        DispatchLogManager::new("pnpm", &log, false),
    ]);
    let state = test_state();
    let plan = prerequisites_phase(vec![
        provision_node("brew", "curl", &[]),
        provision_node("npm", "brew", &[ManagerAction::provision_node("brew")]),
        provision_node("pnpm", "npm", &[ManagerAction::provision_node("npm")]),
    ]);

    let (result, rendered) = apply_manager_plan(&registry, &state, &plan);

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(
        result.action_results.iter().filter(|r| !r.success).count(),
        3,
        "the failed node and both of its dependents are failures: {:?}",
        result.action_results
    );
    let events = dispatch_log(&log);
    assert_eq!(
        events,
        vec!["bootstrap:brew"],
        "a node whose dependency failed must not run: {events:?}"
    );
    assert_eq!(
        rendered
            .matches("did not run — provision brew via curl failed earlier in this phase")
            .count(),
        2,
        "both dependents name the ROOT failure, not the link above them: {rendered}"
    );
    // A node that never ran opens no journal row — the same treatment a
    // stalled action gets, and for the same reason: there is nothing to
    // record beginning.
    let journalled: Vec<String> = state
        .journal_entries(result.apply_id)
        .unwrap()
        .into_iter()
        .map(|e| e.resource_id)
        .collect();
    assert_eq!(journalled, vec!["provision:brew".to_string()]);
}

#[test]
fn an_aborted_run_reports_neither_a_failures_dependents_nor_its_siblings() {
    // One rule for everything an abort stopped. npm's provision fails at the
    // instant cancellation arrives, taking down a node that depends on it and
    // leaving a sibling queued behind its lane. Reporting the dependent as a
    // failure while the sibling says nothing would be two rules for one
    // "planned, never began" fact inside a single run — so an aborted run
    // reports what it BEGAN, and the shortfall is the rollup's to name.
    let probe = LaneProbe::holding(&["bootstrap:npm"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("npm", &log, false)
            .stays_unavailable()
            .with_probe(&probe),
        DispatchLogManager::new("pipx", &log, false),
    ]);
    let plan = prerequisites_phase(vec![
        // `provision npm via brew` occupies its mediator's lane — the
        // command that runs is brew's.
        provision_node("npm", "brew", &[]),
        // Downstream of the failure.
        provision_node("pipx", "npm", &[ManagerAction::provision_node("npm")]),
        // A sibling with no edge at all, held only by the brew lane its own
        // installer shares with the running provision's mediator.
        prerequisite_node("git", "brew", &["pipx"]),
    ]);

    let abort = crate::AbortFlag::new();
    let requested = abort.clone();
    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_abort(abort)
        .run(move || {
            assert!(
                driver.await_started("bootstrap:npm"),
                "the provision never began: {:?}",
                driver.events()
            );
            requested.set(130);
            driver.release_all();
        });

    assert_eq!(outcome.result.aborted, Some(130));
    assert_eq!(outcome.result.status, ApplyStatus::Aborted);
    let reported: Vec<&str> = outcome
        .result
        .action_results
        .iter()
        .map(|r| r.description.as_str())
        .collect();
    assert_eq!(
        reported,
        vec!["manager:provision:npm"],
        "only the action the run began may be reported"
    );
    assert!(
        !outcome.transcript.contains("did not run —"),
        "an aborted run must not blame a dependent it never began: {}",
        outcome.transcript
    );
    assert_eq!(
        dispatch_log(&log),
        vec!["bootstrap:npm"],
        "nothing may be dispatched after the abort"
    );
    // The shortfall is named once, numerically, for the dependent and the
    // sibling alike — the record must not read as a clean sweep of a
    // three-action plan that only ever attempted one.
    assert_eq!(outcome.result.planned_total, 3);
    let summary = outcome
        .state
        .get_apply(outcome.result.apply_id)
        .unwrap()
        .and_then(|record| record.summary)
        .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&summary).expect("summary json");
    assert_eq!(parsed["total"], 3, "total is the plan, not what it reached");
    assert_eq!(parsed["notRun"], 2, "both unstarted actions are accounted");
}

#[test]
fn an_edge_naming_a_node_the_run_does_not_hold_is_satisfied() {
    // A phase filter selects actions, not sub-graphs, so a surviving node can
    // name one that was never planned. Waiting for it would stall the run the
    // user asked for.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![DispatchLogManager::new("brew", &log, false)]);
    let state = test_state();
    let plan = prerequisites_phase(vec![provision_node(
        "brew",
        "curl",
        &[ManagerAction::refresh_node("apt")],
    )]);

    let (result, _rendered) = apply_manager_plan(&registry, &state, &plan);

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(dispatch_log(&log), vec!["bootstrap:brew"]);
}

#[test]
fn a_manager_node_journals_under_the_phase_that_planned_it() {
    // The dispatcher serves two phases now, so the journal's phase column is
    // read from the plan rather than assumed — a row filed under the wrong
    // phase is a row no phase query finds.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![DispatchLogManager::new("apt", &log, true)]);
    let state = test_state();
    let plan = prerequisites_phase(vec![prerequisite_node("curl", "apt", &["brew"])]);

    let (result, _rendered) = apply_manager_plan(&registry, &state, &plan);

    assert_eq!(result.status, ApplyStatus::Success);
    let phases: Vec<String> = state
        .journal_entries(result.apply_id)
        .unwrap()
        .into_iter()
        .map(|e| e.phase)
        .collect();
    assert_eq!(phases, vec![PhaseName::Prerequisites.as_str().to_string()]);
}

#[test]
// `set_hook` is process-wide — see the note on the package-lane panic test.
#[serial_test::serial]
fn a_panicking_node_fails_the_run_rather_than_stalling_the_graph() {
    // Panic containment reaches the new action kind: the worker still sends a
    // completion, so the coordinator settles the node AND everything waiting
    // behind it instead of parking forever on a message that cannot arrive.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("apt", &log, true).panicking(),
        DispatchLogManager::new("brew", &log, false),
    ]);
    let state = test_state();
    let plan = prerequisites_phase(vec![
        prerequisite_node("curl", "apt", &["brew"]),
        provision_node("brew", "curl", &[ManagerAction::prereq_node("curl")]),
    ]);

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (result, rendered) = apply_manager_plan(&registry, &state, &plan);
    std::panic::set_hook(hook);

    assert_eq!(result.status, ApplyStatus::Failed);
    assert!(
        !dispatch_log(&log).contains(&"bootstrap:brew".to_string()),
        "the provision ran on a tool that never arrived: {:?}",
        dispatch_log(&log)
    );
    assert!(
        rendered.contains("did not run — curl failed earlier in this phase"),
        "the dependent names what stopped it: {rendered}"
    );
}

#[test]
fn a_cyclic_edge_fails_the_run_instead_of_hanging_it() {
    // The planner's graph is acyclic by construction, so this is the
    // dispatcher's guard against a plan it did not build: nothing runnable and
    // nothing in flight is a FAILED run, never a green tick over work that
    // never ran and never a wait with no end.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false),
        DispatchLogManager::new("npm", &log, false),
    ]);
    let state = test_state();
    let plan = prerequisites_phase(vec![
        provision_node("brew", "npm", &[ManagerAction::provision_node("npm")]),
        provision_node("npm", "brew", &[ManagerAction::provision_node("brew")]),
    ]);

    let (result, rendered) = apply_manager_plan(&registry, &state, &plan);

    assert_eq!(result.status, ApplyStatus::Failed);
    assert!(
        dispatch_log(&log).is_empty(),
        "neither node ran: {:?}",
        dispatch_log(&log)
    );
    assert_eq!(
        rendered.matches("dispatch stalled").count(),
        2,
        "every slot the dispatcher walked away from is reported: {rendered}"
    );
}

#[test]
fn an_index_refresh_in_a_lane_reads_the_real_state_store() {
    // npm resolves its global prefix from `cx.state` inside `update()`. Backed
    // by a stub that store would answer nothing and swallow the write; a lane
    // is backed by the proxy, which messages the one thread that owns the
    // SQLite connection, so the refresh reads and writes the run's real state.
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("npm", &log, true).with_state_writes(),
    ]);
    let state = test_state();
    let plan = prerequisites_phase(vec![Action::Manager(ManagerAction::RefreshIndex {
        manager: "npm".to_string(),
    })]);

    let (result, _rendered) = apply_manager_plan(&registry, &state, &plan);

    use crate::providers::PackageStateStore;
    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(
        state.resolved_prefix("npm").unwrap(),
        Some(("/opt/npm".to_string(), false)),
        "the refresh's write landed in the run's own state store"
    );
}

#[test]
fn the_managers_group_completes_before_the_env_group_begins() {
    // The producer-before-consumer rule, which the phase's split dispatch is
    // what could break: `cfgd:managers` creates the binaries and `cfgd:env`
    // publishes where they live, so no env surface may be written while a
    // provision is still running.
    let home = tempfile::tempdir().unwrap();
    let env_file = home.path().join(".cfgd.env");
    let probe = LaneProbe::holding(&["bootstrap:brew"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).with_probe(&probe),
    ]);
    let plan = prerequisites_phase(vec![
        provision_node("brew", "curl", &[]),
        Action::Env(EnvAction::WriteEnvFile {
            path: env_file.clone(),
            content: "export PATH=\"/home/linuxbrew/.linuxbrew/bin:$PATH\"\n".to_string(),
            vars: 0,
            aliases: 0,
        }),
    ]);

    let driver = std::sync::Arc::clone(&probe);
    let held = env_file.clone();
    let outcome = ConcurrentApply::new(registry, plan).run(move || {
        assert!(
            driver.await_started("bootstrap:brew"),
            "{:?}",
            driver.events()
        );
        assert!(
            !held.exists(),
            "the env file was published while its producer was still running"
        );
        driver.release_all();
    });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    assert!(env_file.exists(), "the env group still ran");
    assert_eq!(
        outcome
            .transcript
            .find("provision brew via curl")
            .zip(outcome.transcript.find(".cfgd.env"))
            .map(|(managers, env)| managers < env),
        Some(true),
        "the tree reads producer before consumer: {}",
        outcome.transcript
    );
}

// --- T5: the execution tree ---

/// A `Reconciler` behind the `RunExecutor` seam, so a test can assert the FULL
/// transcript of a run — header, tree and rollup — rather than the tree alone.
struct ReconcilerExecutor<'a> {
    reconciler: &'a Reconciler<'a>,
    resolved: &'a crate::config::ResolvedProfile,
    modules: &'a [ResolvedModule],
}

impl crate::reconciler::RunExecutor for ReconcilerExecutor<'_> {
    fn apply(&mut self, plan: &Plan, printer: &Printer) -> Result<ApplyResult> {
        self.reconciler.apply(
            plan,
            self.resolved,
            Path::new("."),
            printer,
            None,
            self.modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
    }
}

fn apply_transcript(
    reconciler: &Reconciler<'_>,
    plan: &Plan,
    resolved: &crate::config::ResolvedProfile,
    modules: &[ResolvedModule],
) -> (ApplyResult, String) {
    let (printer, cap) = crate::output::Printer::for_test_doc();
    let result = reconciler
        .apply(
            plan,
            resolved,
            Path::new("."),
            &printer,
            None,
            modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");
    let out = crate::output::strip_ansi(&cap.human());
    (result, out)
}

/// How many settled status lines a transcript holds.
fn status_line_count(out: &str) -> usize {
    crate::test_helpers::settled_status_lines(out).len()
}

/// Every non-empty line of a transcript, trimmed — the shape assertions below
/// are about ORDER, and a trailing-space diff is not what they are pinning.
fn transcript_lines(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn resolved_for(profile: &str, brew_formulae: &[&str]) -> crate::config::ResolvedProfile {
    let mut resolved = make_empty_resolved();
    resolved.layers[0].profile_name = profile.to_string();
    resolved.merged.packages.brew = Some(crate::config::BrewSpec {
        formulae: brew_formulae.iter().map(|f| (*f).to_string()).collect(),
        ..Default::default()
    });
    resolved
}

#[test]
fn manager_action_renders_in_cfgd_managers_group() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![
        install_action("brew", &["ripgrep"]),
        Action::Manager(ManagerAction::Provision {
            manager: "brew".to_string(),
            via: "homebrew installer".to_string(),
            declared: None,
            batched: vec![],
            depends_on: vec![],
        }),
        module_install_action("nvim", "brew", "neovim"),
    ]);
    let resolved = resolved_for("work", &["ripgrep"]);
    let modules = vec![module_for("nvim", "brew", "neovim")];
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &modules);
    let lines = transcript_lines(&out);

    assert!(
        out.contains("Phase: Packages"),
        "the phase opens a block: {out}"
    );
    let managers = lines
        .iter()
        .position(|l| l.trim() == "cfgd:managers")
        .unwrap_or_else(|| panic!("no cfgd:managers heading in: {out}"));
    let profile = lines
        .iter()
        .position(|l| l.trim() == "profile:work")
        .expect("profile group");
    let module = lines
        .iter()
        .position(|l| l.trim() == "module:nvim")
        .expect("module group");
    assert!(
        profile < managers && managers < module,
        "cfgd:managers renders second, between profile and modules: {out}"
    );
}

#[test]
fn manager_action_group_is_display_only() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let action = Action::Manager(ManagerAction::Provision {
        manager: "brew".to_string(),
        via: "homebrew installer".to_string(),
        declared: None,
        batched: vec![],
        depends_on: vec![],
    });
    assert_eq!(
        crate::reconciler::format_action_description(&action),
        "manager:provision:brew",
        "the resource id reads no owner"
    );

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("work"),
            vec![action],
        )],
        warnings: vec![],
    };
    let resolved = resolved_for("work", &["ripgrep"]);
    let (result, _) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    assert_eq!(result.action_results.len(), 1);
    assert_eq!(
        result.action_results[0].description,
        "manager:provision:brew"
    );
    assert_eq!(result.action_results[0].phase, "prerequisites");
    assert_eq!(result.planned_total, 1);
}

#[test]
fn the_prerequisites_serial_groups_render_below_the_managers_tree() {
    // `cfgd:managers` runs in lanes and writes its tree the moment they drain;
    // `cfgd:session` runs serially and streams its own line as it settles.
    // Held back to phase close, the tree would print under a group that
    // follows it in the plan and in every other phase.
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = prerequisites_phase(vec![
        provision_node("brew", "homebrew installer", &[]),
        Action::Env(EnvAction::RefreshLiveSession { vars: vec![] }),
    ]);
    let resolved = make_empty_resolved();
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);
    let lines = transcript_lines(&out);

    let position = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no {needle} in: {out}"))
    };
    let managers = position("cfgd:managers");
    let provision = position("provision brew via homebrew installer");
    let session = position("cfgd:session");

    assert!(
        managers < provision && provision < session,
        "the lane tree is written before the serial half streams: {out}"
    );
    assert!(
        lines[managers].contains("cfgd:managers") && managers > position("Phase: Prerequisites"),
        "the group label sits under its phase heading: {out}"
    );
}

#[test]
fn the_managers_label_is_on_screen_while_its_lanes_run() {
    // `Prerequisites` carries exactly ONE lane group, so its label is written
    // when the lanes start rather than when they drain: the wait bars and
    // command windows of those nodes paint below the last committed line, so a
    // label still deferred at that point lands under the very work it
    // introduces. Read while a node is held mid-bootstrap, which is precisely
    // the window a drain-time label is absent for.
    let probe = LaneProbe::holding(&["bootstrap:brew"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).with_probe(&probe),
    ]);
    let plan = prerequisites_phase(vec![provision_node("brew", "homebrew installer", &[])]);

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan).run_watching(move |screen| {
        assert!(
            driver.await_started("bootstrap:brew"),
            "the node never reached its lane: {:?}",
            driver.events()
        );
        let on_screen = crate::output::strip_ansi(&screen.human());
        assert!(
            on_screen.contains("cfgd:managers"),
            "the group label must be committed before its lanes paint: {on_screen}"
        );
        driver.release_all();
    });

    assert_eq!(
        outcome.result.succeeded(),
        1,
        "the held node still completes: {}",
        outcome.transcript
    );
}

/// The two details a run settles that must NOT render muted: a withheld row's
/// reason, which is the only new information on a line whose subject says the
/// work did not happen, and an error, which the reader has to act on.
///
/// `crate::output::renderer::action_detail_is_muted` is the rule both read;
/// this is the run-level proof that a real apply reaches it.
#[test]
fn a_withheld_reason_and_an_error_detail_both_render_bright() {
    /// Whether the detail beginning at `needle` is preceded by styling — the
    /// separator-to-text window carries an escape only when a detail style was
    /// supplied.
    fn detail_is_styled(raw: &str, needle: &str) -> bool {
        let idx = raw
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle} in {raw}"));
        let head = &raw[..idx];
        let sep = head
            .rfind(" \u{2014} ")
            .unwrap_or_else(|| panic!("no detail separator before {needle} in {raw}"));
        head[sep..].contains('\u{1b}')
    }

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    // `RefreshLiveSession` with no vars returns the skipped suffix, which is
    // the tree's `unchanged` metadata detail.
    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::cfgd("env"),
                vec![Action::Env(EnvAction::RefreshLiveSession { vars: vec![] })],
            ),
            packages_phase(vec![install_action("nosuch", &["x"])])
                .phases
                .remove(0),
        ],
        warnings: vec![],
    };

    let (printer, cap) = crate::output::Printer::for_test_doc();
    reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply");
    let raw = cap.human();

    assert!(
        !detail_is_styled(&raw, "unchanged"),
        "a withheld row's reason carries the line, so it renders bright"
    );
    assert!(
        !detail_is_styled(&raw, "package manager 'nosuch'"),
        "an error detail is never muted"
    );
}

/// Every action that RAN reports how long it took; a skipped one reports no
/// time at all.
///
/// A one-second floor made the suffix's absence ambiguous — "finished in under
/// a second" and "never ran" rendered identically — so a reader comparing two
/// runs could not tell which lines the second run had actually done.
#[test]
fn an_executed_action_is_timed_and_a_skipped_one_is_not() {
    let log = new_dispatch_log();
    let registry = lane_registry(vec![DispatchLogManager::new("brew", &log, true)]);
    let plan = Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Prerequisites,
                &Owner::cfgd("session"),
                vec![Action::Env(EnvAction::RefreshLiveSession { vars: vec![] })],
            ),
            packages_phase(vec![install_action("brew", &["neovim"])])
                .phases
                .remove(0),
        ],
        warnings: vec![],
    };

    let outcome = ConcurrentApply::new(registry, plan).run(|| {});
    let transcript = crate::normalize_snapshot_durations(&outcome.transcript);
    let line_with = |needle: &str| {
        transcript
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing from:\n{transcript}"))
            .to_string()
    };

    let installed = line_with("neovim");
    assert!(
        installed.contains("(XXs)"),
        "an executed action is timed however briefly: {installed:?}"
    );
    let skipped = line_with("the live session");
    assert!(
        !skipped.contains("(XXs)"),
        "a skipped action did no work, so it has no elapsed time: {skipped:?}"
    );
}

/// A plan resolves session-manager availability from the machine and renders
/// the publish it is CERTAIN to skip as skipped — with the detail the apply
/// would give — while leaving it out of `N actions planned`.
///
/// Previewing it as ordinary work made `cfgd plan` promise an action the apply
/// then reported skipped, and the two counts disagreed by one on every host
/// with no session manager.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
#[serial_test::serial]
fn a_plan_pre_skips_the_session_publish_no_manager_can_perform() {
    let _seam =
        crate::test_helpers::EnvVarGuard::set(crate::SYSTEMCTL_BIN_ENV, "/no/such/systemctl");

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::cfgd("session"),
            vec![Action::Env(EnvAction::RefreshLiveSession {
                vars: vec![("EDITOR".to_string(), "nvim".to_string())],
            })],
        )],
        warnings: vec![],
    };

    assert_eq!(
        plan.total_actions(),
        0,
        "a publish no manager can perform is not an action this run will take"
    );

    let (printer, cap) = crate::output::Printer::for_test_doc();
    crate::reconciler::render_plan_tree(&plan, None, &printer);
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());
    assert!(
        out.contains(crate::NO_SESSION_MANAGER),
        "the pre-skipped line states the apply's own reason: {out}"
    );
    assert!(
        out.contains(&crate::output::Theme::default().icon_skipped),
        "a pre-skipped line wears the skip glyph: {out}"
    );
}

/// A live-session refresh that cannot reach any session manager must say so —
/// never render as "unchanged", which claims the surface was already correct
/// rather than never reachable. Guards the fix for the defect where an
/// unprovisioned Linux host (no systemd user manager) lied about `cfgd:session`.
///
/// Gated to the systemd dispatch branch: `refresh_session_env`'s
/// `cfg!(target_os)` arms are compile-time constants fixed to the build
/// target, so this gate mirrors them explicitly — a macOS build compiles the
/// `launchctl` branch instead and would hit `refuse_unseamed_session_write`
/// for a seam this test never points anywhere (see `env_session.rs`'s
/// Linux/BSD-gated unit test for the same reasoning).
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
#[serial_test::serial]
fn refresh_live_session_reports_no_session_manager_when_unavailable() {
    // A missing path: `command_available_with_seam` reads it as "not
    // available" regardless of what the host running this test actually has,
    // so the test is deterministic on a workstation carrying a real systemd
    // user manager just as on a container that has none.
    let _seam =
        crate::test_helpers::EnvVarGuard::set(crate::SYSTEMCTL_BIN_ENV, "/no/such/systemctl");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::cfgd("session"),
            vec![Action::Env(EnvAction::RefreshLiveSession {
                vars: vec![("EDITOR".to_string(), "nvim".to_string())],
            })],
        )],
        warnings: vec![],
    };

    let (printer, cap) = crate::output::Printer::for_test_doc();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .expect("apply must succeed even when the session manager is unavailable");
    let raw = cap.human();

    assert!(
        raw.contains("no session manager"),
        "the skipped line must say why, not just that nothing changed: {raw}"
    );
    assert!(
        !raw.contains("publish 1 var to the live session \u{2014} unchanged"),
        "the unavailable case must not render as the generic unchanged detail: {raw}"
    );

    // The result carries the new suffix the same way the pre-existing
    // `:skipped` suffix already rides on `ActionResult.description` (see
    // `env:write:` results elsewhere in this file) — a display-adjacent
    // annotation on the in-memory result, not on the persisted id. What must
    // stay byte-identical is what `parse_resource_from_description` derives
    // from it once the suffix is stripped, which `managed_resources`/journal
    // rows are keyed on; `parse_resource_from_description_cases` above pins
    // that derivation for the bare `LIVE_SESSION_RESOURCE_ID` already.
    assert_eq!(
        result.action_results.len(),
        1,
        "one action, one result: {:?}",
        result.action_results
    );
    assert_eq!(
        result.action_results[0]
            .description
            .strip_suffix(super::apply::ENV_NO_SESSION_MANAGER_SUFFIX)
            .unwrap_or(&result.action_results[0].description),
        super::format::LIVE_SESSION_RESOURCE_ID,
        "the description strips back to the same resource id every other run uses"
    );
    assert!(
        !result.action_results[0].changed,
        "nothing was actually applied to the session"
    );
    assert!(
        result.action_results[0].success,
        "an absent session manager is not a failure"
    );

    // The tally reads the same predicate the header did: the publish is not
    // attempted, not skipped, so the counted rollup and `N actions planned`
    // are one number and the reason travels with the count.
    assert_eq!(
        result.action_results[0].not_attempted.as_deref(),
        Some(crate::NO_SESSION_MANAGER),
        "the result carries the plan's own pre-skip reason"
    );
    assert!(
        !result.action_results[0].skipped,
        "a withheld action did not run, so it is not a skip that ran"
    );
    assert_eq!(
        plan.total_actions(),
        result.succeeded() + result.skipped() + result.failed(),
        "the header's count and the counted rollup agree"
    );
    assert_eq!(
        result.not_attempted(),
        vec![crate::NO_SESSION_MANAGER.to_string()]
    );
    let stored = state
        .get_apply(result.apply_id)
        .expect("the apply row is readable")
        .and_then(|row| row.summary)
        .expect("the apply row carries a summary");
    assert_eq!(
        crate::state::ApplySummary::prose(&stored),
        "0 succeeded, 1 not attempted",
        "the stored summary prices the withheld action outside its total"
    );
}

#[test]
fn packages_tree_renders_profile_first_while_modules_execute_first() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(DispatchLogManager::new("brew", &log, true)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![
        install_action("brew", &["ripgrep"]),
        module_install_action("nvim", "brew", "neovim"),
    ]);
    let resolved = resolved_for("work", &["ripgrep"]);
    let modules = vec![module_for("nvim", "brew", "neovim")];
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &modules);
    let lines = transcript_lines(&out);

    assert_eq!(
        dispatch_log(&log),
        vec!["install:brew:neovim", "install:brew:ripgrep"],
        "Rule P dispatches module-owned work first"
    );
    let profile = lines
        .iter()
        .position(|l| l.trim() == "profile:work")
        .expect("profile group");
    let module = lines
        .iter()
        .position(|l| l.trim() == "module:nvim")
        .expect("module group");
    assert!(
        profile < module,
        "the deferred tree reads in Owner::sort_key order: {out}"
    );
}

#[test]
fn platform_skip_renders_as_header_annotation_not_a_phase() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = resolved_for("work", &["ripgrep"]);

    let skip = || {
        Action::Module(ModuleAction {
            module_name: "wsl-tools".to_string(),
            kind: ModuleActionKind::Skip {
                reason: "platform not matched (requires: windows)".to_string(),
            },
            origin: None,
        })
    };
    let plan = Plan {
        phases: vec![
            Phase::from_actions(PhaseName::Modules, &Owner::profile("work"), vec![skip()]),
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("work"),
                vec![install_action("brew", &["ripgrep"])],
            ),
        ],
        warnings: vec![],
    };

    let module_names = vec![
        crate::output::HeaderModule {
            name: "nvim".to_string(),
            platform_skip_reason: None,
        },
        crate::output::HeaderModule {
            name: "wsl-tools".to_string(),
            platform_skip_reason: None,
        },
    ];
    let (printer, cap) = crate::output::Printer::for_test_doc();
    let mut exec = ReconcilerExecutor {
        reconciler: &reconciler,
        resolved: &resolved,
        modules: &[],
    };
    crate::reconciler::ApplyRun::new(
        crate::reconciler::RunContext {
            title: crate::reconciler::RunTitle::Apply,
            config_path: None,
            profile: Some("work"),
            sources: &[],
            modules: &module_names,
            profile_inherits: &[],
            trigger: None,
            subject: None,
            unit_source: None,
        },
        &plan,
    )
    .execute(&printer, crate::reconciler::Confirm::Skip, &mut exec)
    .expect("apply");
    let out = crate::output::strip_ansi(&cap.human());

    assert!(
        !out.contains("Phase: Modules"),
        "no Modules block, ever: {out}"
    );
    assert!(
        out.contains(
            "Modules   nvim (wsl-tools skipped: platform not matched (requires: windows))"
        ) || out.contains(
            "Modules  nvim (wsl-tools skipped: platform not matched (requires: windows))"
        ),
        "the row carries the skip's own reason string: {out}"
    );
    assert!(
        out.contains("Phases   Packages") || out.contains("Phases  Packages"),
        "Modules is not listed among the phases: {out}"
    );
    assert!(!out.contains("Phases   Modules"), "got: {out}");
    assert!(
        out.contains("2 planned"),
        "a skip is an in-scope action and is counted: {out}"
    );
    assert!(
        out.contains("1 action succeeded") && out.contains("\u{2205} 1 skipped"),
        "the rollup reconciles against the planned count, and a skip is \
         counted as a skip rather than as work that was done: {out}"
    );

    // A run whose ONLY in-scope work is a platform-gated skip renders header +
    // annotation + rollup and NOTHING else — asserted as the complete
    // transcript, because "no heading" is what a stray warning line satisfies.
    let skip_only = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Modules,
            &Owner::profile("work"),
            vec![skip()],
        )],
        warnings: vec![],
    };
    let only_names = vec![crate::output::HeaderModule {
        name: "wsl-tools".to_string(),
        platform_skip_reason: None,
    }];
    let (printer, cap) = crate::output::Printer::for_test_doc();
    let mut exec = ReconcilerExecutor {
        reconciler: &reconciler,
        resolved: &resolved,
        modules: &[],
    };
    crate::reconciler::ApplyRun::new(
        crate::reconciler::RunContext {
            title: crate::reconciler::RunTitle::Apply,
            config_path: None,
            profile: Some("work"),
            sources: &[],
            modules: &only_names,
            profile_inherits: &[],
            trigger: None,
            subject: None,
            unit_source: None,
        },
        &skip_only,
    )
    .execute(&printer, crate::reconciler::Confirm::Skip, &mut exec)
    .expect("apply");
    let lines: Vec<String> = crate::output::strip_ansi(&cap.human())
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            // Only the rollup carries a wall-clock suffix, and it is always the
            // LAST parenthesis on the line — a skip reason has parentheses of
            // its own that must survive.
            match l.rfind(" (") {
                Some(i) if l.ends_with(" wall)") && l.starts_with('\u{2713}') => l[..i].to_string(),
                _ => l.to_string(),
            }
        })
        .collect();

    assert_eq!(
        lines,
        vec![
            "Apply".to_string(),
            "Profile  work".to_string(),
            "Modules  wsl-tools skipped: platform not matched (requires: windows)".to_string(),
            "Actions  1 planned".to_string(),
            "\u{2713} Apply complete".to_string(),
            // The skip states itself in the role its own row wears, rather
            // than riding the tick's detail as though it were work done.
            "\u{2205} 1 action skipped".to_string(),
        ],
        "header + annotation + rollup and nothing else"
    );
}

#[test]
fn every_action_emits_exactly_one_line() {
    // One action from each arm whose bespoke status line the tree replaced,
    // except the one that cannot share a run (`SecretAction::ResolveEnv` feeds
    // the collector that triggers a late env regeneration, so it is pinned by
    // `a_resolve_env_action_emits_one_line`): if any comes back, this run emits
    // more lines than actions.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));
    registry.secret_backend = Some(Box::new(TestSecretBackend {
        decrypted_value: "my-secret-token".to_string(),
    }));
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("provider-secret-value"),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let tmp = tempfile::tempdir().unwrap();
    let modules = vec![make_resolved_module("nvim")];
    let enc = tmp.path().join("token.enc");
    std::fs::write(&enc, "encrypted-data").unwrap();

    let actions = vec![
        Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: ModuleActionKind::DeployFiles {
                files: vec![],
                declared_total: 0,
            },
            origin: None,
        }),
        Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: ModuleActionKind::Skip {
                reason: "encryption mode Always incompatible".to_string(),
            },
            origin: None,
        }),
        Action::Env(EnvAction::WriteEnvFile {
            path: tmp.path().join(".cfgd.env"),
            content: "export A=1\n".to_string(),
            vars: 0,
            aliases: 0,
        }),
        Action::Env(EnvAction::InjectSourceLine {
            rc_path: tmp.path().join(".bashrc"),
            line: "source ~/.cfgd.env".to_string(),
        }),
        Action::Env(EnvAction::RefreshLiveSession { vars: vec![] }),
        Action::Package(PackageAction::Skip {
            manager: "apt".to_string(),
            reason: "not available on this host".to_string(),
            origin: "local".to_string(),
        }),
        Action::Secret(SecretAction::Decrypt {
            source: enc.clone(),
            target: tmp.path().join("token.txt"),
            backend: "test-sops".to_string(),
            origin: "local".to_string(),
        }),
        Action::Secret(SecretAction::Resolve {
            provider: "vault".to_string(),
            reference: "secret/data/app#key".to_string(),
            target: tmp.path().join("resolved.txt"),
            template: None,
            origin: "local".to_string(),
        }),
        Action::Secret(SecretAction::Skip {
            source: "creds.enc".to_string(),
            reason: "sops not installed".to_string(),
            origin: "local".to_string(),
        }),
        Action::System(SystemAction::Skip {
            configurator: "sysctl".to_string(),
            reason: "not available on this host".to_string(),
            origin: "local".to_string(),
            unknown: false,
        }),
        Action::System(SystemAction::Skip {
            configurator: "gti".to_string(),
            reason: "no configurator registered".to_string(),
            origin: "local".to_string(),
            unknown: true,
        }),
    ];
    let planned = actions.len();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            actions,
        )],
        warnings: vec![],
    };
    let (result, out) = apply_transcript(&reconciler, &plan, &resolved, &modules);

    assert_eq!(result.action_results.len(), planned);
    assert_eq!(
        status_line_count(&out),
        planned,
        "one line per action, no more: {out}"
    );
}

#[test]
fn a_resolve_env_action_emits_one_line() {
    // Applied on its own because a resolved env secret feeds the collector that
    // regenerates the shell env files at the end of the run: that regeneration
    // appends results, so this arm cannot be counted alongside the others.
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("ghp_abc123"),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();
    // The regeneration would otherwise reach the developer's real login session,
    // which no test home can sandbox; the surfaces that stay on disk are enough.
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Secrets,
            &Owner::profile("test"),
            vec![Action::Secret(SecretAction::ResolveEnv {
                provider: "vault".to_string(),
                reference: "secret/data/gh#token".to_string(),
                envs: vec!["GH_TOKEN".to_string()],
                template: None,
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    };
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    assert_eq!(
        status_line_count(&out),
        1,
        "the resolved-env action owns one line; the regeneration owns none: {out}"
    );
}

#[cfg(unix)]
#[test]
fn a_script_phase_emits_one_line_per_script_not_two() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let entry = |run: &str| crate::config::ScriptEntry::Simple(run.to_string());
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::PostScripts,
            &Owner::profile("test"),
            vec![
                Action::Script(ScriptAction::Run {
                    entry: entry("true"),
                    phase: ScriptPhase::PostApply,
                    origin: "local".to_string(),
                }),
                Action::Script(ScriptAction::Run {
                    entry: entry("echo two"),
                    phase: ScriptPhase::PostApply,
                    origin: "local".to_string(),
                }),
            ],
        )],
        warnings: vec![],
    };
    let (result, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    assert_eq!(result.action_results.len(), 2);
    assert_eq!(
        status_line_count(&out),
        2,
        "n scripts emit n status lines, not 2n: {out}"
    );
}

#[test]
fn failure_renders_inside_its_owner_group() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(FailingPackageManager::new("brew")));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("work"),
            vec![module_install_action("nvim", "brew", "neovim")],
        )],
        warnings: vec![],
    };
    let (_, out) = apply_transcript(
        &reconciler,
        &plan,
        &resolved,
        &[module_for("nvim", "brew", "neovim")],
    );
    let lines = transcript_lines(&out);

    let group = lines
        .iter()
        .position(|l| l.trim() == "module:nvim")
        .expect("owner group");
    let failure = lines
        .iter()
        .position(|l| l.trim_start().starts_with('\u{2717}'))
        .expect("failure line");
    assert!(
        group < failure,
        "the failure lands inside its owner group: {out}"
    );
    assert!(
        !out.contains("Failed:"),
        "the [i/total] failure prefix is gone: {out}"
    );
    assert!(!out.contains("[1/1]"), "no positional prefix: {out}");
}

#[cfg(unix)]
#[test]
fn streaming_phase_lines_appear_as_work_completes() {
    // A file phase streams: the first action's line is on the wire before the
    // second action runs. Driven by a script that reads the capture mid-run
    // is not available, so the ordering is asserted structurally — the second
    // action's own window output must appear AFTER the first action's status.
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let entry = |run: &str| crate::config::ScriptEntry::Simple(run.to_string());
    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::PostScripts,
            &Owner::profile("work"),
            vec![
                Action::Script(ScriptAction::Run {
                    entry: entry("echo first-body"),
                    phase: ScriptPhase::PostApply,
                    origin: "local".to_string(),
                }),
                Action::Script(ScriptAction::Run {
                    entry: entry("echo second-body"),
                    phase: ScriptPhase::PostApply,
                    origin: "local".to_string(),
                }),
            ],
        )],
        warnings: vec![],
    };
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    let first_status = out
        .find("\u{2713} run postApply script: echo first-body")
        .expect("first status");
    let second_body = out.find("second-body").expect("second body");
    assert!(
        first_status < second_body,
        "a live section emits as work completes, not at close: {out}"
    );
}

#[test]
fn action_notes_collect_into_the_run_wide_caveats_group() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(NotePushingManager::new("brew")));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = resolved_for("work", &["neovim"]);

    let plan = packages_phase(vec![install_action("brew", &["neovim"])]);
    let (result, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    // `apply()` no longer attaches a package manager's notes under the action
    // line itself — they ride in `ApplyResult.caveats`, grouped by owner, for
    // a caller to render once as the run's closing `Caveats` section.
    assert!(
        !out.contains('\u{26A0}'),
        "apply()'s own transcript carries no attached notes: {out}"
    );
    assert_eq!(
        result.caveats.len(),
        1,
        "one owner group for the one action that reported: {:?}",
        result.caveats
    );
    let (owner, notes) = &result.caveats[0];
    assert_eq!(*owner, Owner::profile("work"));
    assert_eq!(
        notes.iter().map(ActionNote::body).collect::<Vec<_>>(),
        vec![
            // Re-tagged with the action's subject: the closing section groups by
            // owner, so `[brew]` alone would not say which package spoke.
            "[brew install neovim] add /opt/brew/bin to PATH".to_string(),
            "[brew install neovim] restart your shell".to_string(),
        ],
        "one note per push, in order, under the action's owner: {notes:?}"
    );
}

#[test]
fn an_empty_note_drain_emits_nothing() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(TrackingPackageManager::new("brew")));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = resolved_for("work", &["neovim"]);

    let plan = packages_phase(vec![install_action("brew", &["neovim"])]);
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    assert!(!out.contains('\u{26A0}'), "no note, no line: {out}");
}

/// A manager that pushes two post-install notes from `install`, the way a real
/// one does from its captured output.
struct NotePushingManager {
    name: String,
}

impl NotePushingManager {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl PackageManager for NotePushingManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn bootstrap_plan_given(
        &self,
        _delivered: &dyn Fn(&str) -> bool,
    ) -> Option<crate::providers::BootstrapPlan> {
        None
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, _packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        for message in ["add /opt/brew/bin to PATH", "restart your shell"] {
            cx.notes
                .push(crate::providers::ActionNote::warn(&self.name, message));
        }
        Ok(())
    }
    fn uninstall(&self, _: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// A caveat names the action that produced it, not the manager that spoke.
///
/// `extract_caveats` tags every note with the manager (`[brew]`), and the
/// closing section then groups by OWNER — so the action line, the only thing
/// naming what brew spoke ABOUT, is gone by the time the note renders. A
/// `profile:base` installing nine formulae stacked several `[brew]` bodies
/// with nothing tying any of them to a package.
///
/// The section's fold is by MESSAGE, so the attribution the subject supplies
/// cannot defeat it: two actions saying different things keep both lines and
/// each names its own action, while two actions repeating ONE machine fact
/// keep one line, under the subject that said it first.
#[test]
fn every_caveat_names_the_subject_that_produced_it() {
    let owner = Owner::profile("base");
    let repeated = "Bash completion has been installed to: /opt/brew/etc";
    let mut caveats: Vec<(Owner, Vec<crate::providers::ActionNote>)> = Vec::new();
    crate::reconciler::apply::collect_caveats(
        &mut caveats,
        &owner,
        "brew install gum",
        vec![
            crate::providers::ActionNote::info("brew", repeated),
            crate::providers::ActionNote::info("brew", "gum needs a TTY to render"),
        ],
    );
    // The SAME machine fact, from a second action of the same manager.
    crate::reconciler::apply::collect_caveats(
        &mut caveats,
        &owner,
        "brew install fzf",
        vec![
            crate::providers::ActionNote::info("brew", repeated),
            crate::providers::ActionNote::info("brew", "fzf key bindings are in /opt/brew/opt"),
        ],
    );
    // An untagged note carries no tag precisely because its owner group already
    // identifies it, so re-tagging must leave it alone.
    crate::reconciler::apply::collect_caveats(
        &mut caveats,
        &owner,
        "set shell.defaultShell: bash -> zsh",
        vec![crate::providers::ActionNote::untagged(
            crate::output::Role::Info,
            "Updated /etc/shells",
        )],
    );

    let (printer, cap) = crate::output::Printer::for_test_doc();
    crate::reconciler::render_caveats(&printer, &caveats);
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());

    assert!(
        out.contains("[brew install gum]") && out.contains("[brew install fzf]"),
        "each caveat names the action that produced it: {out}"
    );
    assert!(
        !out.contains("[brew] "),
        "no caveat is left tagged with the manager alone: {out}"
    );
    assert!(
        out.contains("[brew install gum] gum needs a TTY")
            && out.contains("[brew install fzf] fzf key bindings"),
        "two actions with things of their own to say keep both lines: {out}"
    );
    assert_eq!(
        out.matches(repeated).count(),
        1,
        "one machine-level fact, one line, however many actions restated it: {out}"
    );
    assert!(
        out.contains(&format!("[brew install gum] {repeated}")),
        "and it keeps the attribution of the action that said it first: {out}"
    );
    assert!(
        out.contains("Updated /etc/shells") && !out.contains("[set shell"),
        "an untagged note keeps its shape: {out}"
    );
}

/// A next step is not a warning. `⚠ run `source ~/.cfgd.env`, or open a new
/// shell` marked an instruction with the glyph a problem wears, and stood
/// among the run's real warnings; it renders as a hint, after everything the
/// run had to report, and reads as an instruction ("Run", not "run").
#[test]
fn a_next_step_renders_as_a_hint_below_the_reports() {
    let (printer, cap) = crate::output::Printer::for_test_doc();
    crate::reconciler::render_caveats(
        &printer,
        &[(
            Owner::cfgd("env"),
            vec![
                crate::providers::ActionNote::next_step("Run `source ~/.cfgd.env`"),
                crate::providers::ActionNote::warn("npm", "deprecated: glob@7"),
                crate::providers::ActionNote::info("npm", "installed into ~/.npm-global"),
            ],
        )],
    );
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let position = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing from: {out}"))
    };
    assert!(
        position("Run `source") > position("deprecated: glob@7")
            && position("Run `source") > position("installed into"),
        "the next step must come last: {out}"
    );
    let step = lines[position("Run `source")];
    assert!(
        !step.starts_with('\u{26a0}'),
        "a next step wears the warning glyph: {step:?}"
    );
    assert!(
        step.contains("Run `source"),
        "a next step is an instruction, capitalized: {step:?}"
    );
}

/// A next step closes the REPORT, not an owner group inside `Caveats`.
///
/// Nested under `cfgd:env` it read as a remark about that one owner, indented
/// two levels below a heading whose subject is "things that went sideways" —
/// while the thing it actually says is what the reader does next about the
/// whole run. It renders after the section closes, at the report's foot, and a
/// run whose only note is a next step opens no `Caveats` heading at all.
#[test]
fn a_next_step_renders_below_the_closed_caveats_section() {
    let (printer, cap) = crate::output::Printer::for_test_doc();
    crate::reconciler::render_caveats(
        &printer,
        &[
            (
                Owner::cfgd("env"),
                vec![crate::providers::ActionNote::next_step(
                    "Run `source ~/.cfgd.env`",
                )],
            ),
            (
                Owner::profile("work"),
                vec![crate::providers::ActionNote::warn(
                    "npm",
                    "deprecated: glob@7",
                )],
            ),
        ],
    );
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());
    let step = out
        .lines()
        .find(|l| l.contains("Run `source"))
        .unwrap_or_else(|| panic!("the next step must render: {out}"));
    let warn = out
        .lines()
        .find(|l| l.contains("deprecated: glob@7"))
        .unwrap_or_else(|| panic!("the report must render: {out}"));
    let indent = |l: &str| l.len() - l.trim_start().len();
    assert_eq!(
        indent(step),
        0,
        "a next step closes the report at column 0, not inside a caveat group: {out}"
    );
    assert!(
        indent(warn) > 0,
        "a report still nests under its owner group: {out}"
    );
    assert!(
        !out.contains("cfgd:env"),
        "an owner whose only note is a next step opens no caveat group: {out}"
    );
}

/// A run whose only note is a next step prints the step and no `Caveats`
/// heading — the heading would introduce an empty section.
#[test]
fn a_lone_next_step_opens_no_caveats_heading() {
    let (printer, cap) = crate::output::Printer::for_test_doc();
    crate::reconciler::render_caveats(
        &printer,
        &[(
            Owner::cfgd("env"),
            vec![crate::providers::ActionNote::next_step(
                "Run `source ~/.cfgd.env`",
            )],
        )],
    );
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());
    assert!(out.contains("Run `source"), "the step must render: {out}");
    assert!(
        !out.contains("Caveats"),
        "nothing to caveat, so no heading: {out}"
    );
}

/// A caveat states a fact about the MACHINE, and a run that provisions a
/// manager in `Prerequisites` and uses it again in `Packages` files that one
/// fact under two owners. The section printed the byte-identical
/// `Bash completion has been installed to: …` twice, once under
/// `cfgd:managers` and once under `module:nvim`, reading as though brew had
/// installed completions twice to the same path.
///
/// Driven through `collect_caveats`, the real path: it re-tags every note with
/// the SUBJECT of the action that produced it, so a fold keyed on the composed
/// body could never fire on a run — the two copies carry two subjects — and
/// passed only in a test that assembled the groups by hand.
#[test]
fn a_caveat_message_renders_once_per_report() {
    let message = "Bash completion has been installed to /home/linuxbrew/etc";
    let mut caveats: Vec<(Owner, Vec<crate::providers::ActionNote>)> = Vec::new();
    crate::reconciler::apply::collect_caveats(
        &mut caveats,
        &Owner::cfgd("managers"),
        "provision brew via curl",
        vec![crate::providers::ActionNote::info("brew", message)],
    );
    crate::reconciler::apply::collect_caveats(
        &mut caveats,
        &Owner::module("nvim"),
        "brew install neovim",
        // The repeat, plus a note only this owner produced: the group must
        // still open for the second, and drop only the first.
        vec![
            crate::providers::ActionNote::warn("npm", "no writable global prefix"),
            crate::providers::ActionNote::info("brew", message),
        ],
    );
    // Every note this owner holds is a repeat, so it opens no heading.
    crate::reconciler::apply::collect_caveats(
        &mut caveats,
        &Owner::profile("work"),
        "brew install fzf",
        vec![crate::providers::ActionNote::info("brew", message)],
    );

    let (printer, cap) = crate::output::Printer::for_test_doc();
    crate::reconciler::render_caveats(&printer, &caveats);
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());
    assert_eq!(
        out.matches(message).count(),
        1,
        "one machine-level fact, one line: {out}"
    );
    assert!(
        out.contains("[provision brew via curl] "),
        "the action that produced it FIRST keeps it: {out}"
    );
    assert!(
        out.contains("cfgd:managers") && out.contains("module:nvim"),
        "the owner that produced it FIRST keeps it, and a group with a note of \
         its own still opens: {out}"
    );
    assert!(
        !out.contains("profile:work"),
        "an owner whose every note was a repeat opens no group: {out}"
    );
    assert!(
        out.contains("no writable global prefix"),
        "a distinct note is untouched: {out}"
    );
}

/// The report half and the hint half are two slots on one section, and for a
/// while only the hint half deduplicated. Walk both, so they cannot diverge
/// again: the same message, filed under two owners, renders once whichever
/// slot carries it — and the report slots go through `collect_caveats`, so the
/// per-action attribution is in the way of the fold exactly as it is on a run.
#[test]
fn every_caveat_slot_dedupes_by_message() {
    type NoteBuilder = fn(&str) -> crate::providers::ActionNote;
    let slots: [(&str, NoteBuilder); 4] = [
        ("hint", |m| crate::providers::ActionNote::next_step(m)),
        ("report/warn", |m| {
            crate::providers::ActionNote::untagged(crate::output::Role::Warn, m)
        }),
        ("report/info", |m| {
            crate::providers::ActionNote::untagged(crate::output::Role::Info, m)
        }),
        ("report/tagged", |m| {
            crate::providers::ActionNote::info("brew", m)
        }),
    ];
    for (slot, build) in slots {
        let message = "one fact about this machine";
        let mut caveats: Vec<(Owner, Vec<crate::providers::ActionNote>)> = Vec::new();
        crate::reconciler::apply::collect_caveats(
            &mut caveats,
            &Owner::cfgd("managers"),
            "provision brew via curl",
            vec![build(message)],
        );
        crate::reconciler::apply::collect_caveats(
            &mut caveats,
            &Owner::module("nvim"),
            "brew install neovim",
            vec![build(message)],
        );
        // Twice within ONE group as well: the snapshot-bridge caller hands
        // `render_caveats` a single group, so it can only ever duplicate this
        // way — and two actions of one owner is the other way.
        crate::reconciler::apply::collect_caveats(
            &mut caveats,
            &Owner::profile("work"),
            "brew install fzf",
            vec![build(message), build(message)],
        );
        let (printer, cap) = crate::output::Printer::for_test_doc();
        crate::reconciler::render_caveats(&printer, &caveats);
        drop(printer);
        let out = crate::output::strip_ansi(&cap.human());
        assert_eq!(
            out.matches(message).count(),
            1,
            "the {slot} slot printed the same message more than once: {out}"
        );
    }
}

/// A configurator that narrates from `apply`, the way every real one does while
/// it walks the keys it is setting.
struct NarratingConfigurator;

impl crate::providers::SystemConfigurator for NarratingConfigurator {
    fn name(&self) -> &str {
        "sysctl"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Null)
    }
    fn diff(&self, _: &serde_yaml::Value) -> Result<Vec<crate::providers::SystemDrift>> {
        Ok(vec![])
    }
    fn apply(&self, _: &serde_yaml::Value, cx: &crate::providers::SystemContext<'_>) -> Result<()> {
        cx.report(crate::output::Role::Info, "sysctl -w net.ipv4.ip_forward=1");
        cx.report(
            crate::output::Role::Warn,
            "reload deferred: /proc is read-only",
        );
        Ok(())
    }
}

fn sysctl_set_value_plan() -> Plan {
    Plan {
        phases: vec![Phase::from_actions(
            PhaseName::System,
            &Owner::profile("work"),
            vec![Action::System(SystemAction::SetValue {
                configurator: "sysctl".to_string(),
                key: "net.ipv4.ip_forward".to_string(),
                desired: "1".to_string(),
                current: "0".to_string(),
                origin: "local".to_string(),
            })],
        )],
        warnings: vec![],
    }
}

fn resolved_with_sysctl_key() -> crate::config::ResolvedProfile {
    let mut resolved = make_empty_resolved();
    resolved.layers[0].profile_name = "work".to_string();
    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("net.ipv4.ip_forward".to_string()),
        serde_yaml::Value::String("1".to_string()),
    );
    resolved
        .merged
        .system
        .insert("sysctl".to_string(), serde_yaml::Value::Mapping(settings));
    resolved
}

#[test]
fn configurator_narration_collects_into_the_run_wide_caveats_group() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_system_configurator(Box::new(NarratingConfigurator));
    let reconciler = Reconciler::new(&registry, &state);

    let (result, out) = apply_transcript(
        &reconciler,
        &sysctl_set_value_plan(),
        &resolved_with_sysctl_key(),
        &[],
    );
    let lines = transcript_lines(&out);

    let status = lines
        .iter()
        .position(|l| l.contains("set sysctl.net.ipv4.ip_forward"))
        .unwrap_or_else(|| panic!("the action's own status line: {out}"));
    assert!(
        lines[status].trim_start().starts_with('\u{2713}'),
        "the action settles its own line first: {out}"
    );
    // `apply()` no longer attaches a configurator's narration under the
    // action line — it rides in `ApplyResult.caveats` for a caller to render
    // once as the run's closing `Caveats` section.
    assert!(
        status + 1 >= lines.len() || !lines[status + 1].trim_start().starts_with('\u{25C9}'),
        "apply()'s own transcript carries no attached narration: {out}"
    );
    assert_eq!(
        result.caveats.len(),
        1,
        "one owner group for the one action that reported: {:?}",
        result.caveats
    );
    let (owner, notes) = &result.caveats[0];
    assert_eq!(*owner, Owner::profile("work"));
    // Untagged: the action line already says which configurator spoke.
    assert_eq!(
        notes.iter().map(ActionNote::body).collect::<Vec<_>>(),
        vec![
            "sysctl -w net.ipv4.ip_forward=1".to_string(),
            "reload deferred: /proc is read-only".to_string(),
        ],
        "narration collects in order, keeping its role: {notes:?}"
    );
}

#[test]
fn configurator_narration_settles_on_its_own_when_no_caller_drains_it() {
    // The standalone shape — a `SystemContext::new` caller owns no action line,
    // so a report the sink would otherwise hold is the only output the user
    // gets and must still reach the terminal.
    let (printer, cap) = crate::output::Printer::for_test_doc();
    let cx = crate::providers::SystemContext::new(&printer);
    crate::providers::SystemConfigurator::apply(
        &NarratingConfigurator,
        &serde_yaml::Value::Null,
        &cx,
    )
    .expect("apply");
    let out = crate::output::strip_ansi(&cap.human());

    assert!(
        out.contains("sysctl -w net.ipv4.ip_forward=1"),
        "an undrained report still settles rather than vanishing: {out}"
    );
    assert!(
        out.contains("reload deferred: /proc is read-only"),
        "including the warning: {out}"
    );
}

#[test]
fn decision_store_ownership_matches_only_the_runs_own_scope() {
    // The store a run opens is resolved from ITS scope, so only that scope's
    // default config speaks for it: judged cross-scope, `cfgd --config
    // /etc/cfgd/cfgd.yaml apply` (a user-scope run) would sweep the per-user
    // store with the system picture's subscription list.
    let staging = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(staging.path());
    let user_cfg = crate::default_config_dir_for(crate::Scope::User).join("cfgd.yaml");
    let system_cfg = crate::default_config_dir_for(crate::Scope::System).join("cfgd.yaml");

    assert!(
        owns_decision_store(&user_cfg, false, crate::Scope::User),
        "the user scope's own default config owns the user store"
    );
    assert!(
        !owns_decision_store(&system_cfg, false, crate::Scope::User),
        "the system config is a different machine picture to the user store"
    );
    assert!(
        owns_decision_store(&system_cfg, false, crate::Scope::System),
        "the system scope's own default config owns the system store"
    );
    assert!(
        !owns_decision_store(&user_cfg, false, crate::Scope::System),
        "and the user config does not own the system store"
    );
    assert!(
        owns_decision_store(
            Path::new("/somewhere/else/cfgd.yaml"),
            true,
            crate::Scope::User
        ),
        "a --state-dir override grants ownership regardless: the swept store is the one the config was aimed at"
    );
}

/// The mediator the plan named has to reach the bootstrap that runs, or the
/// manager re-probes and can pick a different one — installing outside the
/// concurrency lane the action was serialized on, and contradicting the line
/// the user read.
#[test]
fn a_provisions_planned_via_reaches_the_bootstrap_that_executes_it() {
    let log = new_dispatch_log();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        DispatchLogManager::new("npm", &log, false).recording_provision_via(&seen),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &Owner::profile("work"),
            vec![Action::Manager(ManagerAction::Provision {
                manager: "npm".to_string(),
                via: "apt".to_string(),
                declared: None,
                batched: vec![],
                depends_on: vec![],
            })],
        )],
        warnings: vec![],
    };
    let resolved = resolved_for("work", &[]);
    let (result, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    assert!(result.action_results[0].success, "the provision ran: {out}");
    assert_eq!(
        seen.lock().unwrap().as_deref(),
        Some("apt"),
        "bootstrap must see the method the plan resolved, not None"
    );
}

#[test]
fn the_post_apply_snapshot_covers_only_the_files_the_run_touched() {
    // One module, two files: one the run has to write and one already holding
    // the source bytes. The snapshot follows the run's own backup rows, so the
    // converged target is not re-read, re-hashed and re-stored as a blob to
    // record that nothing happened to it.
    let dir = tempfile::tempdir().unwrap();
    let converged_source = dir.path().join("converged-source.txt");
    let converged_target = dir.path().join("converged-target.txt");
    std::fs::write(&converged_source, "already there").unwrap();
    std::fs::write(&converged_target, "already there").unwrap();
    let written_source = dir.path().join("written-source.txt");
    let written_target = dir.path().join("written-target.txt");
    std::fs::write(&written_source, "fresh").unwrap();
    std::fs::write(&written_target, "stale").unwrap();

    let resolved_file = |source: &std::path::Path, target: &std::path::Path| ResolvedFile {
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        is_git_source: false,
        strategy: Some(crate::config::FileStrategy::Copy),
        encryption: None,
        permissions: None,
        patch: None,
    };
    let files = vec![
        resolved_file(&converged_source, &converged_target),
        resolved_file(&written_source, &written_target),
    ];

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.default_file_strategy = crate::config::FileStrategy::Copy;
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "mymod".to_string(),
        packages: vec![],
        files: files.clone(),
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        system: BTreeMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
        origin: None,
        platform_skip_reason: None,
    }];

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Files,
            &Owner::profile("test"),
            vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    declared_total: files.len(),
                    files,
                },
                origin: None,
            })],
        )],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseFilter::Phase(PhaseName::Files)),
            &modules,
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(
        std::fs::read_to_string(&written_target).unwrap(),
        "fresh",
        "the file the run had to write must be written"
    );

    let rows = state.get_apply_backups(result.apply_id).unwrap();
    let rows_for = |p: &std::path::Path| {
        let key = crate::to_posix_fs_key(p);
        rows.iter().filter(|r| r.file_path == key).count()
    };
    // The written target carries both rows: the pre-write backup a rollback
    // restores through, and the post-apply snapshot of what it ended up
    // holding.
    assert_eq!(rows_for(&written_target), 2);
    assert_eq!(
        rows_for(&converged_target),
        0,
        "an untouched managed target must not be snapshotted"
    );
}

/// The phase blocks a plan renders, as `(phase, items)` — what a preview and a
/// `-o json` payload are both built from, so two plans that agree here are the
/// same plan as far as any surface can tell.
fn plan_render(plan: &Plan) -> Vec<(String, Vec<String>)> {
    plan.phases
        .iter()
        .map(|p| (p.name.display_name().to_string(), plan_items(p)))
        .collect()
}

#[test]
fn plan_observed_reports_every_computed_phase_in_order() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(MockPackageManager::new("brew")));
    let reconciler = Reconciler::new(&registry, &state);

    let mut resolved = make_empty_resolved();
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Simple("scripts/pre.sh".to_string())];
    let modules = vec![make_resolved_module("dev")];

    let mut seen: Vec<PhaseName> = Vec::new();
    let observed = reconciler
        .plan_observed(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules.clone(),
            ReconcileContext::Apply,
            &mut |phase| seen.push(phase),
        )
        .unwrap();

    // Computation order, not render order: `Prerequisites` is planned from the
    // package work that survived dedup, so it cannot be reported before
    // `Packages` even though it renders ahead of it. `PostScripts` never fires
    // — its actions are computed in the same passes as `PreScripts` and
    // `Modules`.
    assert_eq!(
        seen,
        vec![
            PhaseName::Files,
            PhaseName::PreScripts,
            PhaseName::Modules,
            PhaseName::Packages,
            PhaseName::Prerequisites,
            PhaseName::System,
            PhaseName::Secrets,
        ]
    );

    let one_pass = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();
    assert_eq!(plan_render(&observed), plan_render(&one_pass));
}

/// The package items a plan holds, across every phase.
fn all_plan_items(plan: &Plan) -> Vec<String> {
    plan.phases.iter().flat_map(plan_items).collect()
}

#[test]
fn a_module_package_the_manager_already_has_is_not_planned() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim"]),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();

    // `make_resolved_module` declares neovim + ripgrep under brew; only neovim
    // is on the machine.
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![make_resolved_module("dev")],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("ripgrep"),
        "the uninstalled package must still be planned, got:\n{items}"
    );
    assert!(
        !items.contains("neovim"),
        "the installed package must be elided, got:\n{items}"
    );
}

/// The whole path the daemon's tick took: a bare `- name: npm` resolved
/// through the run's own installed-state context lands on brew, and the plan
/// built over that resolution carries no install of it — not through apt,
/// not through brew. `prefer: [apt]` over the same machine plans the apt
/// install the author asked for.
#[test]
fn a_bare_entry_another_manager_holds_is_neither_re_resolved_nor_planned() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(MockPackageManager::new("apt")));
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&["npm"]),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();
    let platform = crate::test_helpers::linux_ubuntu_platform();
    let managers = registry.manager_map();

    let plan_for = |entry: ModulePackageEntry| {
        let pkg = crate::modules::resolve_package(&entry, "nvim", &platform, &managers, Some(&cx))
            .unwrap()
            .unwrap();
        let manager = pkg.manager.clone();
        let mut module = make_resolved_module("nvim");
        module.packages = vec![pkg];
        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                vec![module],
                ReconcileContext::Apply,
            )
            .unwrap();
        (manager, all_plan_items(&plan).join("\n"))
    };

    let (manager, items) = plan_for(ModulePackageEntry {
        name: "npm".into(),
        ..Default::default()
    });
    assert_eq!(manager, "brew");
    assert!(
        !items.contains("npm"),
        "a tool brew already holds is not installed again, got:\n{items}"
    );

    let (manager, items) = plan_for(ModulePackageEntry {
        name: "npm".into(),
        prefer: vec!["apt".into()],
        ..Default::default()
    });
    assert_eq!(manager, "apt");
    assert!(
        items.contains("npm"),
        "an authored `prefer: [apt]` plans the apt install, got:\n{items}"
    );
}

/// `package_survives_elision` is the ONE predicate, and the in-run arm is
/// part of it: a tool a node of THIS run provisioned is elided from an
/// EMPTY listing, and only for an entry whose author named no manager.
#[test]
fn a_tool_this_run_provisioned_is_elided_by_the_one_predicate() {
    let apt = MockPackageManager::new("apt");
    let none = crate::providers::InstalledPackages::default();
    let mut pkg = make_resolved_module("nvim").packages.remove(0);
    pkg.canonical_name = "npm".into();
    pkg.resolved_name = "npm".into();
    pkg.manager = "apt".into();
    pkg.manager_declared = false;

    assert!(Reconciler::package_survives_elision(&apt, &none, &pkg, &[]));
    assert!(
        !Reconciler::package_survives_elision(&apt, &none, &pkg, &["npm".to_string()]),
        "the run's own provision of npm satisfies the bare entry"
    );
    pkg.manager_declared = true;
    assert!(
        Reconciler::package_survives_elision(&apt, &none, &pkg, &["npm".to_string()]),
        "a declared route is judged by its own manager's listing, never by the cascade"
    );
}

/// A module declares `minVersion` and the machine's copy is older: the package
/// is still planned, because "present" was never the question the floor asks.
#[test]
fn an_installed_package_below_its_min_version_is_still_planned() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed_at("neovim", "0.9.5"),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();

    let mut module = make_resolved_module("dev");
    module.packages.retain(|p| p.canonical_name == "neovim");
    module.packages[0].min_version = Some("0.11".to_string());

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("neovim"),
        "an installed copy below the declared floor must still be planned, got:\n{items}"
    );
}

/// The same module against a machine whose copy already clears the floor: the
/// package is elided exactly as an unconstrained installed package is.
#[test]
fn an_installed_package_meeting_its_min_version_is_elided() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed_at("neovim", "0.11.2"),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();

    let mut module = make_resolved_module("dev");
    module.packages.retain(|p| p.canonical_name == "neovim");
    module.packages[0].min_version = Some("0.11".to_string());

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        !items.contains("neovim"),
        "an installed copy clearing the declared floor must be elided, got:\n{items}"
    );
}

/// The same converged machine against a floor DECLARED with a `v` prefix, the
/// spelling a `minVersion` arrives in as often as the bare one. This is the
/// layer the harm landed on: a floor the comparator cannot read makes the
/// package survive elision on every run — an install planned forever on a
/// machine that already holds it — and files a `Below` finding that no apply
/// can heal.
#[test]
fn a_v_prefixed_floor_neither_replans_nor_drifts_a_package_that_clears_it() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed_at("neovim", "1.2.3"),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();

    let mut module = make_resolved_module("dev");
    module.packages.retain(|p| p.canonical_name == "neovim");
    module.packages[0].min_version = Some("v1.2.0".to_string());

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();
    let items = all_plan_items(&plan).join("\n");
    assert!(
        !items.contains("neovim"),
        "a copy clearing a `v`-prefixed floor is elided like any other, got:\n{items}"
    );

    let mgr = MockPackageManager::new("brew").with_installed_at("neovim", "1.2.3");
    let installed = cx.installed_for(&mgr).expect("the mock lists");
    assert!(
        matches!(
            crate::reconciler::package_version_floor(&mgr, &installed, "neovim", Some("v1.2.0")),
            crate::reconciler::VersionFloor::Met
        ),
        "and the scan files no finding against it"
    );
}

#[test]
fn a_module_whose_packages_are_all_installed_plans_nothing() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim", "ripgrep"]),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![make_resolved_module("dev")],
            ReconcileContext::Apply,
        )
        .unwrap();

    // A converged module contributes no action, so it mints no manager
    // prerequisite either and the whole run reads as "nothing to do".
    assert!(
        plan.is_empty(),
        "a converged module must plan nothing, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn a_manager_that_cannot_be_queried_still_plans_the_whole_declared_set() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew").with_installed_error("brew db unreadable"),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![make_resolved_module("dev")],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("neovim") && items.contains("ripgrep"),
        "an unobservable machine fails open, got:\n{items}"
    );
}

/// A manager name no other test in this binary shares, so the process-global
/// available-version memo (keyed by `(manager, package)`) can never answer one
/// of these tests out of another's fixture.
fn unshared_manager_name(prefix: &str) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    format!(
        "{prefix}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    )
}

/// A version-less resolved package under `manager`, the shape
/// `fill_planned_versions` prices.
fn unpriced_package(manager: &str, name: &str) -> crate::modules::ResolvedPackage {
    crate::modules::ResolvedPackage {
        canonical_name: name.to_string(),
        resolved_name: name.to_string(),
        manager: manager.to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    }
}

/// The whole of a converged `cfgd plan`'s silent multi-second wait was pricing
/// packages the plan then elided: a satisfied package produces no action, no
/// rendered description and no stored string, so its version query buys
/// nothing. The survivor gate must ask NOTHING for a converged module.
#[test]
#[serial_test::serial(available_version_memo)]
fn a_converged_module_is_priced_with_zero_version_queries() {
    let state = test_state();
    let printer = test_printer();
    let mgr_name = unshared_manager_name("conv-priced-mgr");
    let mgr = MockPackageManager::new(&mgr_name)
        .with_installed(&["demofix-conv-a", "demofix-conv-b"])
        .with_package("demofix-conv-a", "1.0.0")
        .with_package("demofix-conv-b", "2.0.0");
    let queries = mgr.version_query_counter();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(mgr));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);

    let mut module = make_resolved_module("dev");
    module.packages = vec![
        unpriced_package(&mgr_name, "demofix-conv-a"),
        unpriced_package(&mgr_name, "demofix-conv-b"),
    ];
    let mut modules = vec![module];
    reconciler.fill_planned_versions(&mut modules, &registry.manager_map());

    assert_eq!(
        queries.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a converged module's packages are elided from the plan, so pricing them buys nothing"
    );
    assert!(
        modules[0].packages.iter().all(|p| p.version.is_none()),
        "nothing asked means nothing filled"
    );
}

/// The other half of the contract: a package the plan WILL surface is still
/// priced through the same memoized query, and its planned `InstallPackages`
/// description renders the version byte-identically to the unconditional fill.
#[test]
#[serial_test::serial(available_version_memo)]
fn an_unsatisfied_package_is_still_priced_and_planned_with_its_version() {
    let state = test_state();
    let printer = test_printer();
    let mgr_name = unshared_manager_name("survivor-priced-mgr");
    let mgr = MockPackageManager::new(&mgr_name)
        .with_installed(&["demofix-have"])
        .with_package("demofix-have", "1.0.0")
        .with_package("demofix-need", "3.1.4");
    let queries = mgr.version_query_counter();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(mgr));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);

    let mut module = make_resolved_module("dev");
    module.packages = vec![
        unpriced_package(&mgr_name, "demofix-have"),
        unpriced_package(&mgr_name, "demofix-need"),
    ];
    let mut modules = vec![module];
    reconciler.fill_planned_versions(&mut modules, &registry.manager_map());

    assert_eq!(
        queries.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "only the surviving package is priced"
    );
    assert_eq!(
        modules[0]
            .packages
            .iter()
            .find(|p| p.resolved_name == "demofix-need")
            .and_then(|p| p.version.as_deref()),
        Some("3.1.4")
    );

    let resolved = make_empty_resolved();
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();
    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains(&format!("{mgr_name} install demofix-need (3.1.4)")),
        "the planned install must carry the version its manager quoted, got:\n{items}"
    );
}

/// Fail-open parity with the planner: a manager that cannot be queried plans
/// the declared set in full, so the same set is priced in full — an
/// unobservable machine must not render version-less strings the next
/// observable run then rewrites.
#[test]
#[serial_test::serial(available_version_memo)]
fn an_unreadable_manager_is_priced_in_full_matching_the_plan() {
    let state = test_state();
    let printer = test_printer();
    let mgr_name = unshared_manager_name("unreadable-priced-mgr");
    let mgr = MockPackageManager::new(&mgr_name)
        .with_installed_error("db unreadable")
        .with_package("demofix-blind", "2.2.2")
        .with_package("demofix-deaf", "4.4.4");
    let queries = mgr.version_query_counter();
    let enumerations = mgr.enumeration_counter();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(mgr));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);

    let mut module = make_resolved_module("dev");
    module.packages = vec![
        unpriced_package(&mgr_name, "demofix-blind"),
        unpriced_package(&mgr_name, "demofix-deaf"),
    ];
    let mut modules = vec![module];
    reconciler.fill_planned_versions(&mut modules, &registry.manager_map());

    assert_eq!(queries.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        enumerations.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a failed read is held for the run, never retried per package — \
         only successes memoize, and each retry is a full failing subprocess"
    );
    assert_eq!(
        modules[0].packages[0].version.as_deref(),
        Some("2.2.2"),
        "the elision gate fails open exactly where the planner does"
    );
    assert_eq!(modules[0].packages[1].version.as_deref(), Some("4.4.4"));
}

#[test]
fn an_unavailable_manager_is_never_asked_what_it_holds() {
    let state = test_state();
    let printer = test_printer();
    let mut registry = ProviderRegistry::new();
    // Unavailable AND erroring: the error is what would surface if the diff
    // asked anyway, and a bootstrappable manager's packages are planned in full
    // because nothing is installed under a manager that is not there yet.
    registry.add_package_manager(Box::new(
        MockPackageManager::new("brew")
            .unavailable()
            .bootstrappable()
            .with_installed(&["neovim", "ripgrep"]),
    ));
    let cx = test_package_context(&printer, &state);
    let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
    let resolved = make_empty_resolved();

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![make_resolved_module("dev")],
            ReconcileContext::Apply,
        )
        .unwrap();

    let items = all_plan_items(&plan).join("\n");
    assert!(
        items.contains("neovim") && items.contains("ripgrep"),
        "a manager that is not on the machine yet holds nothing, got:\n{items}"
    );
}

// --- Module file convergence: the plan diffs deployed targets ---

fn deployable_file(source: &Path, target: &Path) -> ResolvedFile {
    ResolvedFile {
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        is_git_source: false,
        // Explicit, because the registry default is `Symlink` and these
        // fixtures deploy real content.
        strategy: Some(FileStrategy::Copy),
        encryption: None,
        permissions: None,
        patch: None,
    }
}

/// A files-only module bracketed by lifecycle hooks, the shape whose converged
/// runs must go silent.
fn hooked_file_module(name: &str, files: Vec<ResolvedFile>) -> ResolvedModule {
    let mut module = make_resolved_module(name);
    module.packages.clear();
    module.files = files;
    module.pre_apply_scripts = vec![ScriptEntry::Simple("echo pre".to_string())];
    module.post_apply_scripts = vec![ScriptEntry::Simple("echo post".to_string())];
    module
}

/// Plan the modules against a state whose manifest already OWNS the seeded
/// `(module, target)` rows — the precondition for eliding a converged file.
/// With no rows the run behaves like a first deploy and elides nothing.
fn plan_modules_recorded(modules: Vec<ResolvedModule>, seeded: &[(&str, &Path)]) -> Plan {
    let state = test_state();
    if !seeded.is_empty() {
        // `last_applied` is a foreign key into `applies`, so the seeded rows
        // hang off one recorded run the way real deploys do.
        let apply_id = state
            .record_apply("test", "hash", ApplyStatus::Success, None)
            .unwrap();
        for (module, target) in seeded {
            state
                .upsert_module_file(
                    module,
                    &crate::to_posix_fs_key(target),
                    "",
                    "Copy",
                    apply_id,
                )
                .unwrap();
        }
    }
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap()
}

fn plan_modules_only(modules: Vec<ResolvedModule>) -> Plan {
    plan_modules_recorded(modules, &[])
}

#[test]
fn a_deployed_file_matching_its_source_is_elided_and_the_subset_names_itself() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let tgt = dir.path().join("tgt");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tgt).unwrap();
    std::fs::write(src.join("init.lua"), "settled").unwrap();
    std::fs::write(tgt.join("init.lua"), "settled").unwrap();
    std::fs::write(src.join("keys.lua"), "changed upstream").unwrap();

    let plan = plan_modules_recorded(
        vec![hooked_file_module(
            "nvim",
            vec![
                deployable_file(&src.join("init.lua"), &tgt.join("init.lua")),
                deployable_file(&src.join("keys.lua"), &tgt.join("keys.lua")),
            ],
        )],
        &[
            ("nvim", &tgt.join("init.lua")),
            ("nvim", &tgt.join("keys.lua")),
        ],
    );

    let files_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Files)
        .expect("the changed file must still be planned");
    let items = plan_items(files_phase).join("\n");
    assert!(
        items.contains("keys.lua") && !items.contains("init.lua"),
        "only the changed file survives, got:\n{items}"
    );
    let details: Vec<String> = files_phase
        .actions()
        .filter_map(|a| super::action_produced_detail(a, None, 0, &[]))
        .collect();
    assert_eq!(
        details,
        vec!["1 already deployed".to_string()],
        "a subset counts against the declared set, in the row's detail"
    );

    // A module that still has work runs its hooks around it.
    for phase in [PhaseName::PreScripts, PhaseName::PostScripts] {
        assert!(
            plan.phases.iter().any(|p| p.name == phase && !p.is_empty()),
            "a partially converged module still runs its {phase:?} hooks"
        );
    }
}

#[test]
fn a_module_whose_files_all_match_plans_nothing_and_runs_no_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let tgt = dir.path().join("tgt");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tgt).unwrap();
    for name in ["init.lua", "keys.lua"] {
        std::fs::write(src.join(name), "settled").unwrap();
        std::fs::write(tgt.join(name), "settled").unwrap();
    }

    let plan = plan_modules_recorded(
        vec![hooked_file_module(
            "nvim",
            vec![
                deployable_file(&src.join("init.lua"), &tgt.join("init.lua")),
                deployable_file(&src.join("keys.lua"), &tgt.join("keys.lua")),
            ],
        )],
        &[
            ("nvim", &tgt.join("init.lua")),
            ("nvim", &tgt.join("keys.lua")),
        ],
    );

    assert!(
        plan.is_empty(),
        "a converged module plans no deploy and no hooks, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn a_matching_target_the_manifest_does_not_own_is_still_planned() {
    // A first deploy over a target that already holds the source's bytes:
    // eliding it would leave `module_file_manifest` without the row that
    // `cfgd status <module>` and `profile remove-module` read, so the file
    // would be deployed in fact and unowned on record. Convergence elides
    // only what the manifest already owns.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let tgt = dir.path().join("tgt");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tgt).unwrap();
    std::fs::write(src.join("init.lua"), "settled").unwrap();
    std::fs::write(tgt.join("init.lua"), "settled").unwrap();

    let module = || {
        hooked_file_module(
            "nvim",
            vec![deployable_file(
                &src.join("init.lua"),
                &tgt.join("init.lua"),
            )],
        )
    };

    let plan = plan_modules_only(vec![module()]);
    assert!(
        all_plan_items(&plan).join("\n").contains("init.lua"),
        "an unrecorded match is planned so the deploy records it, got:\n{:?}",
        all_plan_items(&plan)
    );

    // The same fixture with the manifest row present is the settled machine.
    let plan = plan_modules_recorded(vec![module()], &[("nvim", &tgt.join("init.lua"))]);
    assert!(
        plan.is_empty(),
        "a recorded match elides, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn a_module_file_whose_source_does_not_exist_refuses_the_plan() {
    // The silent alternative shipped: a fresh home settled `∅ unchanged` over
    // files that were never written. A declaration naming a source that does
    // not exist is refused while the plan is read — the same refusal the
    // profile file path makes — never planned around.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("src").join("init.lua");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let err = reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            vec![hooked_file_module(
                "nvim",
                vec![deployable_file(
                    &missing,
                    &dir.path().join("tgt").join("init.lua"),
                )],
            )],
            ReconcileContext::Apply,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("source file not found"),
        "a missing module file source is a refused declaration, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_entry_is_converged_only_when_the_link_points_at_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("rc"), "x").unwrap();
    std::fs::write(src.join("other"), "y").unwrap();
    let good = dir.path().join("good-link");
    let stale = dir.path().join("stale-link");
    std::os::unix::fs::symlink(src.join("rc"), &good).unwrap();
    std::os::unix::fs::symlink(src.join("other"), &stale).unwrap();

    let mut correct = deployable_file(&src.join("rc"), &good);
    correct.strategy = Some(FileStrategy::Symlink);
    let mut repointed = deployable_file(&src.join("rc"), &stale);
    repointed.strategy = Some(FileStrategy::Symlink);

    let plan = plan_modules_recorded(
        vec![hooked_file_module("links", vec![correct, repointed])],
        &[("links", &good), ("links", &stale)],
    );

    let files_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Files)
        .expect("the repointed link must still be planned");
    let items = plan_items(files_phase).join("\n");
    assert!(
        items.contains("stale-link") && !items.contains("good-link"),
        "a link already pointing at its source is converged, got:\n{items}"
    );
}

#[test]
fn a_directory_deploy_is_not_converged_while_the_target_holds_an_extra_entry() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src-tree");
    let tgt = dir.path().join("tgt-tree");
    std::fs::create_dir_all(src.join("lua")).unwrap();
    std::fs::write(src.join("lua/a.lua"), "a").unwrap();
    std::fs::create_dir_all(tgt.join("lua")).unwrap();
    std::fs::write(tgt.join("lua/a.lua"), "a").unwrap();

    let entry = deployable_file(&src, &tgt);
    let plan = plan_modules_recorded(
        vec![hooked_file_module("tree", vec![entry.clone()])],
        &[("tree", &tgt)],
    );
    assert!(
        plan.is_empty(),
        "an identical tree is converged, got:\n{:?}",
        all_plan_items(&plan)
    );

    // A deploy is remove-then-clone, so an extra deployed entry is drift the
    // deploy corrects — the tree must be planned again.
    std::fs::write(tgt.join("stray.lua"), "left behind").unwrap();
    let plan = plan_modules_recorded(
        vec![hooked_file_module("tree", vec![entry])],
        &[("tree", &tgt)],
    );
    assert!(
        !plan.is_empty(),
        "an extra deployed entry un-converges the tree"
    );
}

#[test]
fn a_manager_refresh_elides_when_no_install_for_its_family_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("rc");
    std::fs::write(&src, "changed").unwrap();
    let mut module = make_resolved_module("dev");
    module.files = vec![deployable_file(&src, &dir.path().join("deployed-rc"))];

    let plan_against = |installed: &[&str]| {
        let state = test_state();
        let printer = test_printer();
        let mut registry = ProviderRegistry::new();
        // The test-helpers mock, because the refresh node exists only for a
        // manager that keeps a local index, which the stub does not claim.
        registry.add_package_manager(Box::new(
            crate::test_helpers::MockPackageManager::new("brew").with_installed(installed),
        ));
        let cx = test_package_context(&printer, &state);
        let reconciler = Reconciler::new(&registry, &state).diffing_installed(&cx);
        reconciler
            .plan(
                &make_empty_resolved(),
                Vec::new(),
                Vec::new(),
                vec![module.clone()],
                ReconcileContext::Apply,
            )
            .unwrap()
    };

    // Control: a surviving brew install wants the index refreshed first.
    let items = all_plan_items(&plan_against(&["neovim"])).join("\n");
    assert!(
        items.contains("refresh brew index"),
        "a surviving install mints its manager's refresh, got:\n{items}"
    );

    // Every declared brew package is installed: the deploy still plans, but no
    // brew action survives, so the prerequisite refresh goes with them.
    let items = all_plan_items(&plan_against(&["neovim", "ripgrep"])).join("\n");
    assert!(
        items.contains("deploy"),
        "the changed file still plans, got:\n{items}"
    );
    assert!(
        !items.contains("refresh brew index"),
        "a refresh with no surviving family install must elide, got:\n{items}"
    );
}

#[test]
fn module_tap_installs_order_before_formula_installs() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(crate::test_helpers::MockPackageManager::new(
        "brew",
    )));
    registry.add_package_manager(Box::new(
        crate::test_helpers::MockPackageManager::new("brew-tap").registering_family_sources(),
    ));
    let reconciler = Reconciler::new(&registry, &state);

    // Declared formula-first, so ordering cannot pass by declaration order.
    let mut module = make_resolved_module("dev");
    module.packages.push(ResolvedPackage {
        canonical_name: "acme/tools".to_string(),
        resolved_name: "acme/tools".to_string(),
        manager: "brew-tap".to_string(),
        manager_declared: false,
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
        min_version: None,
    });

    let plan = reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            vec![module],
            ReconcileContext::Apply,
        )
        .unwrap();

    let packages_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Packages)
        .unwrap();
    let managers: Vec<String> = packages_phase
        .actions()
        .filter_map(|a| match a {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::InstallPackages { resolved } => Some(resolved[0].manager.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        managers,
        vec!["brew-tap".to_string(), "brew".to_string()],
        "the tap registers the source a formula may come from, so it installs first"
    );
}

#[test]
fn a_profile_tap_installs_before_a_modules_formula_across_the_tier_barrier() {
    // The tier barrier dispatches module work before profile work, but a
    // profile-declared tap delivers the repositories a module's formulas
    // resolve from — the dispatcher offers it across the barrier and holds
    // the brew family behind it.
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let harness = crate::test_helpers::ReconcilerTestHarness::builder()
        .with_package_manager(
            crate::test_helpers::MockPackageManager::new("brew").recording_installs(log.clone()),
        )
        .with_package_manager(
            crate::test_helpers::MockPackageManager::new("brew-tap")
                .registering_family_sources()
                .recording_installs(log.clone()),
        )
        .build();

    let plan = harness
        .plan_with_actions(
            Vec::new(),
            vec![PackageAction::Install {
                manager: "brew-tap".to_string(),
                packages: vec!["acme/tools".to_string()],
                origin: "local".to_string(),
            }],
            vec![make_resolved_module("dev")],
        )
        .unwrap();

    let result = harness.apply(&plan, &test_printer()).unwrap();
    assert!(
        result.action_results.iter().all(|r| r.success),
        "every install lands: {:?}",
        result
            .action_results
            .iter()
            .map(|r| (&r.description, &r.error))
            .collect::<Vec<_>>()
    );
    let log = log.lock().unwrap();
    assert_eq!(
        log.first(),
        Some(&vec!["acme/tools".to_string()]),
        "the tap's repositories exist before any formula resolves from them, got {log:?}"
    );
}

#[test]
fn a_patch_entry_converges_at_plan_time_only_under_a_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("conf.yaml");
    std::fs::write(&target, "key: value\n").unwrap();

    let patch_module = |ensure: &str| {
        let mut file = deployable_file(&dir.path().join("unused-src"), &target);
        file.strategy = Some(FileStrategy::Patch);
        file.patch = Some(crate::config::PatchSpec {
            format: None,
            ensure: Some(serde_yaml::from_str(ensure).unwrap()),
            script: None,
            blocked_by: None,
        });
        hooked_file_module("patched", vec![file])
    };

    let plan_with = |module: ResolvedModule, config_dir: Option<&Path>| {
        let state = test_state();
        let apply_id = state
            .record_apply("test", "hash", ApplyStatus::Success, None)
            .unwrap();
        state
            .upsert_module_file(
                "patched",
                &crate::to_posix_fs_key(&target),
                "",
                "Patch",
                apply_id,
            )
            .unwrap();
        let registry = ProviderRegistry::new();
        let mut reconciler = Reconciler::new(&registry, &state);
        if let Some(config_dir) = config_dir {
            reconciler = reconciler.with_config_dir(config_dir);
        }
        reconciler
            .plan(
                &make_empty_resolved(),
                Vec::new(),
                Vec::new(),
                vec![module],
                ReconcileContext::Apply,
            )
            .unwrap()
    };

    // The merge is a function of the live target — the same evaluation diff,
    // verify and compliance already run — so an up-to-date Patch converges.
    let plan = plan_with(patch_module("key: value"), Some(dir.path()));
    assert!(
        plan.is_empty(),
        "an up-to-date Patch converges under a config dir, got:\n{:?}",
        all_plan_items(&plan)
    );

    let plan = plan_with(patch_module("key: value"), None);
    assert!(
        !plan.is_empty(),
        "with no config dir the merge is unanswerable and the deploy plans"
    );

    let plan = plan_with(patch_module("key: other"), Some(dir.path()));
    assert!(
        !plan.is_empty(),
        "a merge that would change the target plans"
    );
}

#[test]
fn an_env_declaring_module_with_converged_files_still_runs_hooks() {
    // The module's files converged, but it also declares env — a work surface
    // planned as a unit, later, by `plan_env`. The hooks defer to that answer
    // instead of failing closed: the env surface has work here (nothing is
    // deployed under this home), so the hooks run.
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(dir.path());
    let src = dir.path().join("src");
    let tgt = dir.path().join("tgt");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tgt).unwrap();
    std::fs::write(src.join("rc"), "settled").unwrap();
    std::fs::write(tgt.join("rc"), "settled").unwrap();

    let mut module = hooked_file_module(
        "shellrc",
        vec![deployable_file(&src.join("rc"), &tgt.join("rc"))],
    );
    module.env.push(crate::config::EnvVar {
        name: "FOO".to_string(),
        value: "bar".to_string(),
        platforms: vec![],
    });

    let plan = plan_modules_recorded(vec![module], &[("shellrc", &tgt.join("rc"))]);

    assert!(
        !plan.phases.iter().any(|p| p.name == PhaseName::Files),
        "the converged file itself stays elided, got:\n{:?}",
        all_plan_items(&plan)
    );
    for phase in [PhaseName::PreScripts, PhaseName::PostScripts] {
        assert!(
            plan.phases.iter().any(|p| p.name == phase && !p.is_empty()),
            "the env surface has work, so the module's {phase:?} hooks bracket it, got:\n{:?}",
            all_plan_items(&plan)
        );
    }
}

#[test]
fn a_converged_env_declaring_modules_hooks_are_deferred_not_dropped() {
    // `plan_modules` cannot answer the env question itself: the hooks land in
    // the gated vec for `plan_observed` to keep when the env plan has work
    // and drop when the surface converged — never silently discarded here.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let tgt = dir.path().join("tgt");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tgt).unwrap();
    std::fs::write(src.join("rc"), "settled").unwrap();
    std::fs::write(tgt.join("rc"), "settled").unwrap();

    let state = test_state();
    let apply_id = state
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();
    state
        .upsert_module_file(
            "shellrc",
            &crate::to_posix_fs_key(tgt.join("rc")),
            "",
            "Copy",
            apply_id,
        )
        .unwrap();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut module = hooked_file_module(
        "shellrc",
        vec![deployable_file(&src.join("rc"), &tgt.join("rc"))],
    );
    module.env.push(crate::config::EnvVar {
        name: "FOO".to_string(),
        value: "bar".to_string(),
        platforms: vec![],
    });
    let (actions, gated) = reconciler.plan_modules(
        std::slice::from_ref(&module),
        "test",
        ReconcileContext::Apply,
    );
    assert!(
        actions.is_empty(),
        "nothing survives elision, got: {actions:?}"
    );
    assert_eq!(
        gated.len(),
        1,
        "one module defers its hooks to the env answer, got: {gated:?}"
    );
    assert_eq!(
        gated[0].0, "shellrc",
        "the deferred hooks are grouped under the declaring module"
    );
    assert_eq!(
        gated[0].1.len(),
        2,
        "both hooks are deferred to the env answer, got: {gated:?}"
    );

    // The same module with no env declared is simply converged: no hooks
    // anywhere.
    module.env.clear();
    let (actions, gated) = reconciler.plan_modules(&[module], "test", ReconcileContext::Apply);
    assert!(
        actions.is_empty() && gated.is_empty(),
        "a fully converged module defers nothing, got: {actions:?} / {gated:?}"
    );
}

#[test]
fn a_converged_env_surface_plans_no_env_actions() {
    // The elision is at plan time, with the same reads the apply arm makes:
    // once the managed file and the rc source line hold what the plan would
    // write, a re-plan carries no env actions at all instead of re-planning
    // writes the apply would only report back as unchanged.
    let tmp = tempfile::tempdir().unwrap();
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    let aliases = vec![crate::config::ShellAlias {
        name: "v".into(),
        command: "nvim".into(),
        platforms: vec![],
    }];
    let plan = || {
        Reconciler::plan_env_with_home(
            &env,
            &aliases,
            &Default::default(),
            crate::config::EnvScope::Interactive,
            &[],
            &[],
            &[],
            &[],
            tmp.path(),
        )
    };

    let first = plan().actions;
    assert!(!first.is_empty(), "a fresh home has env work");
    let printer = test_printer();
    for action in &first {
        if let Action::Env(ea) = action {
            Reconciler::apply_env_action(ea, &printer, crate::providers::NoteSink::discarded())
                .unwrap();
        }
    }

    let second = plan().actions;
    assert!(
        second.is_empty(),
        "an applied env surface plans nothing, got: {second:?}"
    );
}

#[test]
fn a_converged_env_declaring_module_goes_fully_quiet_once_the_surface_converges() {
    // The env-gated hook fold keys off env actions EXISTING; with the env
    // surface elided at plan time, a module whose files and env are both
    // converged plans neither its hooks nor any env work — the run is
    // truthfully "nothing to do" instead of re-running every postApply hook
    // on every converged apply.
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(dir.path());
    let src = dir.path().join("src");
    let tgt = dir.path().join("tgt");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tgt).unwrap();
    std::fs::write(src.join("rc"), "settled").unwrap();
    std::fs::write(tgt.join("rc"), "settled").unwrap();

    let state = test_state();
    let apply_id = state
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();
    state
        .upsert_module_file(
            "shellrc",
            &crate::to_posix_fs_key(tgt.join("rc")),
            "",
            "Copy",
            apply_id,
        )
        .unwrap();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let module = || {
        let mut module = hooked_file_module(
            "shellrc",
            vec![deployable_file(&src.join("rc"), &tgt.join("rc"))],
        );
        module.env.push(crate::config::EnvVar {
            name: "FOO".to_string(),
            value: "bar".to_string(),
            platforms: vec![],
        });
        module
    };
    let mut resolved = make_empty_resolved();
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;

    let before = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![module()],
            ReconcileContext::Apply,
        )
        .unwrap();
    let env_actions: Vec<&Action> = before
        .phases
        .iter()
        .flat_map(|p| p.actions())
        .filter(|a| matches!(a, Action::Env(_)))
        .collect();
    assert!(
        !env_actions.is_empty(),
        "a fresh home still has env work to plan"
    );
    let printer = test_printer();
    for action in env_actions {
        if let Action::Env(ea) = action {
            Reconciler::apply_env_action(ea, &printer, crate::providers::NoteSink::discarded())
                .unwrap();
        }
    }

    let after = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![module()],
            ReconcileContext::Apply,
        )
        .unwrap();
    assert!(
        !after
            .phases
            .iter()
            .flat_map(|p| p.actions())
            .any(|a| matches!(a, Action::Env(_))),
        "the converged env surface plans no actions, got:\n{:?}",
        all_plan_items(&after)
    );
    for phase in [PhaseName::PreScripts, PhaseName::PostScripts] {
        assert!(
            !after
                .phases
                .iter()
                .any(|p| p.name == phase && !p.is_empty()),
            "a fully converged module runs no {phase:?} hooks, got:\n{:?}",
            all_plan_items(&after)
        );
    }
}

/// Scaffolding for the per-module env hook gate: a converged file module
/// (`shellrc`, manifest-owned target) declaring `FOO`, under a guarded home,
/// so each test converges the env surface once and then moves exactly one
/// layer's contribution.
struct EnvHookGateFixture {
    dir: tempfile::TempDir,
    _home: crate::TestHomeGuard,
    state: StateStore,
}

fn env_hook_gate_fixture() -> EnvHookGateFixture {
    let dir = tempfile::tempdir().unwrap();
    let home = crate::with_test_home_guard(dir.path());
    let src = dir.path().join("src");
    let tgt = dir.path().join("tgt");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&tgt).unwrap();
    std::fs::write(src.join("rc"), "settled").unwrap();
    std::fs::write(tgt.join("rc"), "settled").unwrap();
    let state = test_state();
    let apply_id = state
        .record_apply("test", "hash", ApplyStatus::Success, None)
        .unwrap();
    state
        .upsert_module_file(
            "shellrc",
            &crate::to_posix_fs_key(tgt.join("rc")),
            "",
            "Copy",
            apply_id,
        )
        .unwrap();
    EnvHookGateFixture {
        dir,
        _home: home,
        state,
    }
}

impl EnvHookGateFixture {
    fn module(&self, foo_value: &str) -> ResolvedModule {
        let src = self.dir.path().join("src");
        let tgt = self.dir.path().join("tgt");
        let mut module = hooked_file_module(
            "shellrc",
            vec![deployable_file(&src.join("rc"), &tgt.join("rc"))],
        );
        module.env.push(crate::config::EnvVar {
            name: "FOO".to_string(),
            value: foo_value.to_string(),
            platforms: vec![],
        });
        module
    }

    /// Plan under `resolved` and apply every env action, so the surface the
    /// next plan reads is converged for these inputs.
    fn converge_env(
        &self,
        reconciler: &Reconciler<'_>,
        resolved: &crate::config::ResolvedProfile,
        module: ResolvedModule,
    ) {
        let plan = reconciler
            .plan(
                resolved,
                Vec::new(),
                Vec::new(),
                vec![module],
                ReconcileContext::Apply,
            )
            .unwrap();
        let printer = test_printer();
        for action in plan.phases.iter().flat_map(|p| p.actions()) {
            if let Action::Env(ea) = action {
                Reconciler::apply_env_action(ea, &printer, crate::providers::NoteSink::discarded())
                    .unwrap();
            }
        }
    }
}

fn plan_has_env_action(plan: &Plan) -> bool {
    plan.phases
        .iter()
        .flat_map(|p| p.actions())
        .any(|a| matches!(a, Action::Env(_)))
}

fn plan_has_hook_work(plan: &Plan) -> bool {
    plan.phases
        .iter()
        .any(|p| matches!(p.name, PhaseName::PreScripts | PhaseName::PostScripts) && !p.is_empty())
}

#[test]
fn another_layers_env_change_does_not_revive_a_converged_modules_hooks() {
    // The demo bug: the module's packages, files and its own env entries are
    // all converged, and the profile layer then adds an alias. The shared
    // env file legitimately gets a planned rewrite — owned by the layer that
    // changed it — but the module's own contribution is byte-identical
    // between the deployed file and the planned content, so its hooks stay
    // dropped instead of re-running on every other layer's env edit.
    let fx = env_hook_gate_fixture();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &fx.state);
    let mut resolved = make_empty_resolved();
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;
    fx.converge_env(&reconciler, &resolved, fx.module("bar"));

    resolved.merged.aliases.push(crate::config::ShellAlias {
        name: "catn".to_string(),
        command: "cat -n".to_string(),
        platforms: vec![],
    });
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![fx.module("bar")],
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(
        plan_has_env_action(&plan),
        "the other layer's alias still plans its env rewrite, got:\n{:?}",
        all_plan_items(&plan)
    );
    assert!(
        !plan_has_hook_work(&plan),
        "another layer's contribution must not revive this module's hooks, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn a_modules_own_env_change_revives_its_hooks() {
    // The counterweight to the test above: when the module's OWN entry is
    // what moves the shared surface, its hooks bracket the rewrite.
    let fx = env_hook_gate_fixture();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &fx.state);
    let mut resolved = make_empty_resolved();
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;
    fx.converge_env(&reconciler, &resolved, fx.module("bar"));

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![fx.module("baz")],
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(
        plan_has_env_action(&plan),
        "the changed entry plans its env rewrite, got:\n{:?}",
        all_plan_items(&plan)
    );
    assert!(
        plan_has_hook_work(&plan),
        "the module's own env change revives its hooks, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn an_unanswerable_env_baseline_fails_open_and_revives_hooks() {
    // The deployed primary env file cannot be read back (here: not valid
    // UTF-8, the same damage `read_managed_baseline` regenerates over), so
    // the module's own contribution cannot be compared. Uncertainty fails
    // OPEN: the hooks revive rather than silently skip.
    let fx = env_hook_gate_fixture();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &fx.state);
    let mut resolved = make_empty_resolved();
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;
    fx.converge_env(&reconciler, &resolved, fx.module("bar"));

    // Damage the PLATFORM's primary file: the Windows engine reads
    // `.cfgd-env.ps1`, and a damaged `.cfgd.env` there is a file it never opens.
    let primary = if cfg!(windows) {
        ".cfgd-env.ps1"
    } else {
        ".cfgd.env"
    };
    std::fs::write(fx.dir.path().join(primary), b"\xff\xfe broken").unwrap();
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![fx.module("bar")],
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(
        plan_has_env_action(&plan),
        "the damaged file plans its regeneration, got:\n{:?}",
        all_plan_items(&plan)
    );
    assert!(
        plan_has_hook_work(&plan),
        "an unanswerable baseline revives the hooks, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn a_modules_own_env_entry_deletion_revives_its_hooks() {
    // The module deletes ONE of its several declared entries. The deleted
    // entry is absent from every current declaration, so the per-entry
    // comparison cannot see it — its deployed line goes unclaimed, the
    // attribution is unanswerable, and the gate fails OPEN: hooks revive.
    let fx = env_hook_gate_fixture();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &fx.state);
    let mut resolved = make_empty_resolved();
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;
    let mut with_bar = fx.module("bar");
    with_bar.env.push(crate::config::EnvVar {
        name: "BAR".to_string(),
        value: "baz".to_string(),
        platforms: vec![],
    });
    fx.converge_env(&reconciler, &resolved, with_bar);

    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![fx.module("bar")],
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(
        plan_has_env_action(&plan),
        "the shrunk declaration plans its env rewrite, got:\n{:?}",
        all_plan_items(&plan)
    );
    assert!(
        plan_has_hook_work(&plan),
        "the module's own entry deletion revives its hooks, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn a_profile_layer_deleting_its_alias_revives_converged_modules_hooks() {
    // Documented over-trigger, pinned as the chosen semantics: the deleted
    // alias belonged to the PROFILE, but a deleted declaration is absent
    // from every current layer, so plan-time attribution cannot say whose
    // its deployed line was. Per the fail-open ruling, uncertainty revives
    // — a converged module's hooks running once too often on the rare
    // deletion beats them silently not running when the deletion was the
    // module's own. Contrast with the ADDITION case
    // (`another_layers_env_change_does_not_revive_a_converged_modules_hooks`),
    // which leaves no orphaned line behind and stays cleanly attributed.
    let fx = env_hook_gate_fixture();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &fx.state);
    let mut resolved = make_empty_resolved();
    resolved.merged.env_scope = crate::config::EnvScope::Interactive;
    resolved.merged.aliases.push(crate::config::ShellAlias {
        name: "catn".to_string(),
        command: "cat -n".to_string(),
        platforms: vec![],
    });
    fx.converge_env(&reconciler, &resolved, fx.module("bar"));

    resolved.merged.aliases.clear();
    let plan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            vec![fx.module("bar")],
            ReconcileContext::Apply,
        )
        .unwrap();

    assert!(
        plan_has_env_action(&plan),
        "the deletion plans its env rewrite, got:\n{:?}",
        all_plan_items(&plan)
    );
    assert!(
        plan_has_hook_work(&plan),
        "an unclaimed deleted line fails open and revives the hooks, got:\n{:?}",
        all_plan_items(&plan)
    );
}

#[test]
fn a_script_and_env_module_keeps_its_hooks_unconditionally() {
    // A module with no packages and no files is a script module whatever else
    // it declares: the scripts are the whole of its content, and there is
    // nothing for it to converge against. Routing its hooks through the env
    // gate would make the module inert on every run whose env surface
    // happened to converge.
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);

    let mut module = hooked_file_module("greeter", Vec::new());
    module.env.push(crate::config::EnvVar {
        name: "FOO".to_string(),
        value: "bar".to_string(),
        platforms: vec![],
    });

    let (actions, gated) = reconciler.plan_modules(
        std::slice::from_ref(&module),
        "test",
        ReconcileContext::Apply,
    );
    assert_eq!(
        actions.len(),
        2,
        "both hooks are planned outright, got: {actions:?}"
    );
    assert!(
        gated.is_empty(),
        "a script module's hooks are never deferred to the env answer, got: {gated:?}"
    );
}

#[test]
fn a_source_that_vanishes_between_plan_and_execute_fails_the_deploy() {
    // Planning already refused a missing source, so one absent at execute
    // vanished in between. The declaration is broken, not the machine: the
    // action FAILS (a `∅ unchanged` here is a false convergence — the run
    // once settled exit 0 over files it never wrote), the target keeps its
    // bytes (removing it would trade a broken declaration for lost data),
    // the manifest gains no row for a file cfgd never wrote, and no empty
    // directories are left behind as the one trace of the deploy.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("gone");
    std::fs::write(&src, "payload").unwrap();
    let src2 = dir.path().join("gone2");
    std::fs::write(&src2, "payload").unwrap();
    let target = dir.path().join("kept");
    std::fs::write(&target, "the user's own bytes").unwrap();
    let nested_target = dir.path().join("never").join("made");

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let module = hooked_file_module(
        "broken",
        vec![
            deployable_file(&src, &target),
            deployable_file(&src2, &nested_target),
        ],
    );
    let plan = reconciler
        .plan(
            &make_empty_resolved(),
            Vec::new(),
            Vec::new(),
            vec![module.clone()],
            ReconcileContext::Apply,
        )
        .unwrap();

    std::fs::remove_file(&src).unwrap();
    std::fs::remove_file(&src2).unwrap();

    let result = reconciler
        .apply(
            &plan,
            &make_empty_resolved(),
            Path::new("."),
            &test_printer(),
            None,
            std::slice::from_ref(&module),
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let deploy = result
        .action_results
        .iter()
        .find(|r| r.description.contains("files"))
        .expect("the deploy action is reported");
    assert!(
        !deploy.success && !deploy.skipped,
        "a vanished source fails the deploy instead of settling a skip: {deploy:?}"
    );
    assert!(
        deploy
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("source file not found"),
        "the failure names the missing source: {deploy:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "the user's own bytes",
        "the target is left alone"
    );
    assert!(
        state.module_deployed_files("broken").unwrap().is_empty(),
        "the manifest never owns a file cfgd never wrote"
    );
    assert!(
        !dir.path().join("never").exists(),
        "a broken declaration leaves no empty directories behind"
    );
}

/// The sidecar copy of an adopted file belongs to the action that overwrites
/// it, not to planning: it runs inside the Files phase, so the run reports it
/// while it happens and a plan that is never applied leaves the disk alone.
///
/// The tmp dir stands in as home, so the row is read the way an operator reads
/// it: the subject folds `$HOME` and the sidecar detail beside it has to fold
/// the same directory the same way, or one row spells one path two ways.
#[test]
fn a_reserved_target_is_copied_aside_by_the_file_action_that_replaces_it() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp.path());
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    std::fs::write(&target, "years of hand edits\n").unwrap();

    let harness = crate::test_helpers::ReconcilerTestHarness::builder().build();
    let plan = harness
        .plan_with_actions(
            vec![FileAction::Update {
                source,
                target: target.clone(),
                diff: String::new(),
                origin: "local".to_string(),
                strategy: crate::config::FileStrategy::Copy,
                source_hash: None,
                patch: None,
            }],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();

    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    Reconciler::new(&harness.registry, &harness.state)
        .backing_up(std::collections::HashSet::from([target.clone()]))
        .apply(
            &plan,
            &harness.resolved,
            std::path::Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();
    drop(printer);

    assert_eq!(
        std::fs::read_to_string(tmp.path().join("live.conf.cfgd-backup")).unwrap(),
        "years of hand edits\n",
        "the action that replaces the file copies it aside first"
    );
    let out = crate::test_helpers::captured_text(&buf);
    // One row per action: the copy is the action row's own detail, never a
    // status line of its own above it.
    let row = out
        .lines()
        .find(|l| l.contains("live.conf") && l.contains("backed up to"))
        .unwrap_or_else(|| panic!("the copy is reported on the row that made it, got: {out}"));
    assert!(
        row.contains("backed up to ~/live.conf.cfgd-backup"),
        "the row names where the copy landed, folded the way its own subject is, got: {row}"
    );
    assert!(
        !row.contains(&crate::to_posix_string(tmp.path())),
        "the detail spells the home directory absolutely beside a subject that folds it, got: {row}"
    );
    assert!(
        !out.contains("Backed up to"),
        "the standalone backup line is gone, got: {out}"
    );
}

/// A target nobody reserved is never copied aside: the reservation is what the
/// conflict decision produced, and the file action asks for nothing else.
#[test]
fn an_unreserved_target_is_not_copied_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    std::fs::write(&target, "years of hand edits\n").unwrap();

    let harness = crate::test_helpers::ReconcilerTestHarness::builder().build();
    let plan = harness
        .plan_with_actions(
            vec![FileAction::Update {
                source,
                target: target.clone(),
                diff: String::new(),
                origin: "local".to_string(),
                strategy: crate::config::FileStrategy::Copy,
                source_hash: None,
                patch: None,
            }],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();

    Reconciler::new(&harness.registry, &harness.state)
        .apply(
            &plan,
            &harness.resolved,
            std::path::Path::new("."),
            &test_printer(),
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    assert!(
        !tmp.path().join("live.conf.cfgd-backup").exists(),
        "an unreserved target gets no sidecar"
    );
}

/// EVERY line of a generated env file names its owner: a module-declared entry
/// names its module, a profile-declared one names the LAYER that declared it,
/// and the bootstrapped PATH line names the manager (or managers) whose
/// directories it holds. A file that is the merge of N layers has no default
/// owner, so an uncommented line would be the one line nobody can attribute.
#[test]
fn every_generated_env_line_names_the_owner_that_declared_it() {
    let layer = |name: &str, env: Vec<(&str, &str)>, aliases: Vec<(&str, &str)>| {
        crate::config::ProfileLayer {
            source: crate::config::LOCAL_LAYER.to_string(),
            profile_name: name.to_string(),
            priority: 1000,
            policy: crate::config::LayerPolicy::Local,
            spec: crate::config::ProfileSpec {
                env: env
                    .into_iter()
                    .map(|(n, v)| crate::config::EnvVar {
                        name: n.into(),
                        value: v.into(),
                        platforms: vec![],
                    })
                    .collect(),
                aliases: aliases
                    .into_iter()
                    .map(|(n, c)| crate::config::ShellAlias {
                        name: n.into(),
                        command: c.into(),
                        platforms: vec![],
                    })
                    .collect(),
                ..Default::default()
            },
        }
    };
    // Two layers, and `work` overrides `base`'s PAGER: the comment has to name
    // the layer whose VALUE survived, which is what recording owners inside the
    // merge (rather than re-deriving them afterwards) buys.
    let merged = crate::config::merge_layers(&[
        layer("base", vec![("PAGER", "less")], vec![("catn", "cat -n")]),
        layer("work", vec![("PAGER", "bat")], vec![]),
    ]);

    let mut module = crate::test_helpers::make_resolved_module("nvim");
    module.env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    module.aliases = vec![crate::config::ShellAlias {
        name: "v".into(),
        command: "nvim".into(),
        platforms: vec![],
    }];

    let (env, aliases, origins) = super::merge_module_env_aliases(
        &merged.env,
        &merged.aliases,
        &merged.entry_owners,
        std::slice::from_ref(&module),
    );
    let path_dirs = vec![
        ManagerPathDir::new("brew", "/home/linuxbrew/.linuxbrew/bin"),
        ManagerPathDir::new("brew", "/home/linuxbrew/.linuxbrew/sbin"),
        ManagerPathDir::new("cargo", "/home/u/.cargo/bin"),
    ];
    let content = super::generate_env_file_content(
        &env,
        &aliases,
        Some(&FoldedPath::derived(&path_dirs)),
        &origins,
    );

    assert!(
        content.contains("export EDITOR=\"nvim\" # module:nvim"),
        "a module-declared var names its module: {content}"
    );
    assert!(
        content.contains("alias v=\"nvim\" # module:nvim"),
        "a module-declared alias names its module: {content}"
    );
    assert!(
        content.contains("export PAGER=\"bat\" # profile:work"),
        "an overridden var names the layer whose value won: {content}"
    );
    assert!(
        content.contains("alias catn=\"cat -n\" # profile:base"),
        "an alias names the layer that declared it: {content}"
    );
    // One comment for the whole line, managers in directory order, deduped —
    // one per directory would repeat `brew` twice and say nothing extra.
    assert!(
        content.contains(
            "export PATH=\"/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:\
             /home/u/.cargo/bin:$PATH\" # manager:brew,cargo"
        ),
        "the bootstrapped PATH line names its managers once each: {content}"
    );
    // Every DIALECT, not just the one whose exact strings are pinned above:
    // each generator appends its own comments and dropping any one of those
    // calls has to fail here. The assertion is on the line's TAIL so it says
    // nothing about a dialect's own assignment syntax.
    for (dialect, content) in [
        ("bash/zsh", content),
        (
            "fish",
            super::generate_fish_env_content(
                &env,
                &aliases,
                Some(&FoldedPath::derived(&path_dirs)),
                &origins,
            ),
        ),
        (
            "powershell",
            super::env_files::generate_powershell_env_content(
                &env,
                &aliases,
                Some(&FoldedPath::derived(&path_dirs)),
                &origins,
            ),
        ),
    ] {
        let body: Vec<&str> = content
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert!(
            body.iter().all(|l| l.contains(" # ")),
            "{dialect}: every generated line below the header names an owner: {content}"
        );
        for (needle, owner) in [
            ("EDITOR", "# module:nvim"),
            ("PAGER", "# profile:work"),
            ("catn", "# profile:base"),
            ("PATH", "# manager:brew,cargo"),
        ] {
            let line = body
                .iter()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{dialect}: no line names {needle}: {content}"));
            assert!(
                line.ends_with(owner),
                "{dialect}: `{line}` must end with `{owner}`"
            );
        }
    }
}

/// The `PATH` declarations that survive on a host CONCATENATE, and the one line
/// they fold into names every layer that contributed to it. Keeping only the
/// last one is what made a per-platform `PATH` entry unusable: the gated
/// declaration and the common one both apply on the machine that matches both,
/// and last-writer-wins silently discarded whichever came first.
#[test]
#[serial_test::serial]
fn every_surviving_path_declaration_reaches_the_one_generated_line() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let layer = |name: &str, path: &str| crate::config::ProfileLayer {
        source: crate::config::LOCAL_LAYER.to_string(),
        profile_name: name.to_string(),
        priority: 1000,
        policy: crate::config::LayerPolicy::Local,
        spec: crate::config::ProfileSpec {
            env: vec![crate::config::EnvVar {
                name: "PATH".into(),
                value: path.into(),
                platforms: vec![],
            }],
            ..Default::default()
        },
    };
    // The layer fold joins on the host's separator, and the generated line is
    // read back with the dialect that matches it — a `:`-joined declaration on
    // Windows is one entry to both halves.
    let sep = crate::PATH_LIST_SEPARATOR;
    let merged = crate::config::merge_layers(&[
        layer("base", &format!("$HOME/.local/bin{sep}$PATH")),
        layer("work", &format!("$HOME/go/bin{sep}$PATH")),
    ]);
    assert_eq!(
        merged.env.iter().filter(|e| e.name == "PATH").count(),
        1,
        "the fold leaves one PATH entry, which is what `fold_path_line`'s single lookup reads"
    );

    let mut module = crate::test_helpers::make_resolved_module("nvim");
    module.env = vec![crate::config::EnvVar {
        name: "PATH".into(),
        value: format!("$HOME/.cargo/bin{sep}$PATH"),
        platforms: vec![],
    }];

    let (env, aliases, origins) = super::merge_module_env_aliases(
        &merged.env,
        &merged.aliases,
        &merged.entry_owners,
        std::slice::from_ref(&module),
    );
    assert_eq!(
        env.iter().filter(|e| e.name == "PATH").count(),
        1,
        "a module's PATH joins the profile's rather than replacing it"
    );

    let path_dirs = vec![ManagerPathDir::new(
        "brew",
        "/home/linuxbrew/.linuxbrew/bin",
    )];
    let folded = super::env_engine::primary_folded_path(
        &env,
        &path_dirs,
        &origins,
        tmp_home.path(),
        if cfg!(windows) {
            super::env_engine::EnvPlatform::Windows
        } else {
            super::env_engine::EnvPlatform::Linux
        },
    )
    .expect("a declared PATH folds into a line");
    let content = super::generate_env_file_content(&env, &aliases, Some(&folded), &origins);

    let line = content
        .lines()
        .find(|l| l.contains("PATH="))
        .unwrap_or_else(|| panic!("one PATH line: {content}"));
    assert_eq!(
        content.lines().filter(|l| l.contains("PATH=")).count(),
        1,
        "one variable, one assignment: {content}"
    );
    // The generated file is the POSIX one whatever the host, so the line it
    // writes joins on `:` and names the ambient value `$PATH` — only the
    // SPLIT of the declared value follows the host's own separator.
    assert!(
        line.contains("$HOME/.local/bin:$HOME/go/bin:$HOME/.cargo/bin:"),
        "every declaration contributes, in declaration order: {line}"
    );
    assert_eq!(
        line.matches("$PATH").count(),
        1,
        "the ambient reference is written once: {line}"
    );
    // The comment names every contributing LAYER, not just the last writer —
    // a single owner on a value three layers built would credit one of them
    // for directories the others put there.
    for owner in [
        "profile:base",
        "profile:work",
        "module:nvim",
        "manager:brew",
    ] {
        assert!(line.contains(owner), "the comment names {owner}: {line}");
    }
}

/// The provenance comment is part of the line `verify` matches, so a file
/// written with it must read back as current rather than as permanent drift.
/// Every owner kind is on the file at once — profile layer, module and the
/// bootstrapped PATH line's manager — because the planner and the verifier
/// share ONE merge and a comment either side rendered differently would be
/// drift nothing can fix.
#[test]
#[serial_test::serial]
fn an_owner_commented_env_line_written_by_the_planner_verifies_as_current() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let profile_env = vec![crate::config::EnvVar {
        name: "PAGER".into(),
        value: "less".into(),
        platforms: vec![],
    }];
    let layer_owners = {
        let mut o = crate::config::EntryOwners::default();
        o.claim("profile:base", &profile_env, &[]);
        o
    };
    let path_dirs = vec![ManagerPathDir::new("brew", "/opt/homebrew/bin")];

    let mut module = crate::test_helpers::make_resolved_module("nvim");
    module.env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    }];
    module.aliases = vec![crate::config::ShellAlias {
        name: "v".into(),
        command: "nvim".into(),
        platforms: vec![],
    }];
    let modules = vec![module];

    // Seed the file from the planner itself, so the baseline is the bytes a
    // real apply writes rather than a literal that can drift from them.
    let mut primary: Option<std::path::PathBuf> = None;
    for action in Reconciler::plan_env_with_home(
        &profile_env,
        &[],
        &layer_owners,
        crate::config::EnvScope::Interactive,
        &modules,
        &[],
        &path_dirs,
        &[],
        tmp_home.path(),
    )
    .actions
    {
        if let Action::Env(EnvAction::WriteEnvFile { path, content, .. }) = action {
            // The primary managed file is the first one planned, and the one
            // the per-item verify (and its display recompute) reads.
            primary.get_or_insert_with(|| path.clone());
            std::fs::write(path, content).unwrap();
        }
    }
    let primary = primary.expect("the planner writes a primary managed env file");

    let results = super::verify::env_verify_results(
        &profile_env,
        &[],
        &layer_owners,
        crate::config::EnvScope::Interactive,
        &modules,
        &path_dirs,
    );
    // Only the seeded managed file is under test; the rc source line the test
    // deliberately never injected is a different surface.
    let drifted: Vec<_> = results
        .iter()
        .filter(|r| !r.matches && r.resource_type != "env-rc")
        .collect();
    assert!(
        drifted.is_empty(),
        "a module-owned entry must not report drift against its own written line: {drifted:?}"
    );
    assert!(
        results
            .iter()
            .any(|r| r.resource_type == "env-var" && r.resource_id == "EDITOR" && r.matches),
        "the module-owned var is checked and current: {results:?}"
    );

    // The display recompute must show the same line: a drift row quoting an
    // expected line the file is not required to hold sends the reader to fix a
    // difference that is not the difference.
    let written = std::fs::read_to_string(&primary).unwrap();
    for (id, owner) in [("EDITOR", "# module:nvim"), ("PAGER", "# profile:base")] {
        let shown =
            super::verify::MergedEnvItems::new(&profile_env, &[], &layer_owners, &modules, &[])
                .declared_line("env-var", id)
                .expect("a declared var renders its declared line");
        assert!(
            shown.contains(owner),
            "the shown line carries the provenance comment verify matched on: {shown}"
        );
        assert!(
            written.lines().any(|line| line == shown),
            "the shown line is a line the file actually holds: {shown} in {written}"
        );
    }
    // `written` is the file the planner wrote for THIS host, so the assertion
    // is on the line's directory and its TAIL: the assignment syntax and the
    // separator belong to the dialect (`export PATH="…:$PATH"` on a POSIX
    // shell, `$env:PATH = "…;$env:PATH"` in PowerShell), and pinning one of
    // them here asserts which platform ran the test.
    assert!(
        written
            .lines()
            .any(|line| line.contains("/opt/homebrew/bin") && line.ends_with("# manager:brew")),
        "the written file carries the manager-named PATH line: {written}"
    );
}

/// `--module` isolates the run from the active profile, so the apply record
/// names what the run was scoped to instead of a profile it never resolved.
#[test]
fn a_module_scoped_apply_records_the_scope_it_ran_under() {
    let harness = crate::test_helpers::ReconcilerTestHarness::builder().build();
    let plan = harness.plan().unwrap();

    Reconciler::new(&harness.registry, &harness.state)
        .recording_scope("module:nvim")
        .apply(
            &plan,
            &harness.resolved,
            std::path::Path::new("."),
            &test_printer(),
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        )
        .unwrap();

    let history = harness.state.history(1).unwrap();
    assert_eq!(
        history[0].profile, "module:nvim",
        "the record names the scope the run was given"
    );
}

/// The env write is one action for a whole file, so its line says what went
/// into the file instead of leaving the reader to open it.
#[test]
#[serial_test::serial]
fn an_env_file_write_reports_what_it_wrote() {
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(tmp_home.path());

    let harness = crate::test_helpers::ReconcilerTestHarness::builder()
        .profile_yaml(
            "envScope: Interactive\nenv:\n  - name: EDITOR\n    value: nvim\n  - name: PAGER\n    value: less\n  - name: VISUAL\n    value: nvim\naliases:\n  - name: v\n    command: nvim\n  - name: ll\n    command: ls -la\n",
        )
        .build();
    let plan = harness.plan().unwrap();

    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    harness.apply(&plan, &printer).unwrap();
    drop(printer);

    let out = crate::test_helpers::captured_text(&buf);
    assert!(
        out.contains("3 vars, 2 aliases"),
        "the env write states its own contents: {out}"
    );
}

/// A `Patch` entry's target is nobody's conflict: the strategy merges into
/// whatever the target holds. The marker must leave such a finding alone, or a
/// patch failure's own reason (a barred filter, an unparseable target) is
/// overwritten with a cause that names a different problem.
#[test]
fn a_patch_finding_keeps_its_own_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("settings.json");
    std::fs::write(&target, "the user's own file\n").unwrap();
    let state = crate::state::StateStore::open_in_memory().unwrap();

    let blocked = crate::providers::FileDriftResult {
        target: target.to_string_lossy().into_owned(),
        matches: false,
        expected: "content satisfies patch spec".to_string(),
        actual: "patch script is blocked: source 'acme' is not allowed to run scripts".to_string(),
        unmanaged: false,
    };

    let mut patched = blocked.clone();
    crate::reconciler::mark_unmanaged_drift(
        &mut patched,
        crate::config::FileStrategy::Patch,
        tmp.path(),
        &state,
    );
    assert_eq!(
        patched.actual, blocked.actual,
        "a patch finding keeps the reason it was given"
    );
    assert!(!patched.unmanaged, "a patch target is never a conflict");

    // The same target under a replacing strategy IS a stranger's file: the
    // exclusion is about the strategy, not about this fixture.
    let mut copied = blocked.clone();
    crate::reconciler::mark_unmanaged_drift(
        &mut copied,
        crate::config::FileStrategy::Copy,
        tmp.path(),
        &state,
    );
    assert_eq!(copied.actual, crate::reconciler::UNMANAGED_DRIFT_CAUSE);
    assert!(copied.unmanaged);
}

/// A finding whose desired content could not be determined at all says why cfgd
/// could not look. Re-wording it as `unmanaged file at target` would claim a
/// fact about the target that nobody established — the source is what is
/// missing, and the target was never compared against anything.
#[test]
fn a_source_not_found_finding_keeps_its_own_reason_over_an_untracked_target() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("app.conf");
    std::fs::write(&target, "the user's own file\n").unwrap();
    let state = crate::state::StateStore::open_in_memory().unwrap();

    let missing_source = format!(
        "source not found: {}",
        tmp.path().join("app.conf.src").display()
    );
    let mut record = crate::providers::FileDriftResult {
        target: target.to_string_lossy().into_owned(),
        matches: false,
        expected: crate::providers::SOURCE_MISSING_EXPECTED.to_string(),
        actual: missing_source.clone(),
        unmanaged: false,
    };
    crate::reconciler::mark_unmanaged_drift(
        &mut record,
        crate::config::FileStrategy::Copy,
        tmp.path(),
        &state,
    );
    assert_eq!(
        record.actual, missing_source,
        "a source-missing finding keeps the reason it was given"
    );
    assert!(
        !record.unmanaged,
        "cfgd never looked at the target, so it cannot report it as a stranger's file"
    );
}

/// The two sentences an operator reads when a conflict is not settled by a
/// copy. Both are byte-pinned because they are the whole of what `--on-conflict
/// fail` and a Ctrl-C at the prompt say: a reword that drops the flag name, the
/// module, or the "nothing was applied" guarantee changes what the operator
/// believes happened to their file, and no golden covers either one.
#[test]
fn the_conflict_refusal_and_interrupt_messages_are_pinned() {
    let path = std::path::Path::new("/home/u/.gitconfig");

    assert_eq!(
        crate::reconciler::unmanaged_conflict_error(path, None).to_string(),
        "target exists as unmanaged file: /home/u/.gitconfig (--on-conflict fail)"
    );
    assert_eq!(
        crate::reconciler::unmanaged_conflict_error(path, Some("nvim")).to_string(),
        "module 'nvim': target exists as unmanaged file: /home/u/.gitconfig (--on-conflict fail)"
    );
    assert_eq!(
        crate::errors::FileError::AdoptionPromptInterrupted {
            path: path.to_path_buf(),
        }
        .to_string(),
        "interrupted at the unmanaged-file prompt for /home/u/.gitconfig; nothing was applied"
    );
    assert_eq!(
        crate::reconciler::UNMANAGED_SKIP_REASON,
        "skipped: target exists as unmanaged file"
    );
}

/// A sidecar is taken BEFORE the write it protects, so a write that then fails
/// still left a copy of the user's file on disk. Reported on the failure row
/// beside the error, or it is reported nowhere and the operator has a
/// `.cfgd-backup` nobody told them about.
#[test]
fn a_failing_write_over_an_adopted_target_still_names_the_copy_it_took() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    let target = tmp.path().join("live.conf");
    std::fs::write(&target, "years of hand edits\n").unwrap();

    // The copy aside is taken by the reconciler itself and succeeds; the write
    // it protects is the file manager's, and this one refuses.
    let fm = crate::test_helpers::MockFileManager::new();
    fm.set_fail_apply(true);
    let harness = crate::test_helpers::ReconcilerTestHarness::builder()
        .file_manager(fm)
        .build();
    let plan = harness
        .plan_with_actions(
            vec![FileAction::Update {
                source,
                target: target.clone(),
                diff: String::new(),
                origin: "local".to_string(),
                strategy: crate::config::FileStrategy::Copy,
                source_hash: None,
                patch: None,
            }],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();

    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    let _ = Reconciler::new(&harness.registry, &harness.state)
        .backing_up(std::collections::HashSet::from([target.clone()]))
        .apply(
            &plan,
            &harness.resolved,
            std::path::Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
            None,
            &crate::AbortFlag::new(),
        );
    drop(printer);

    assert_eq!(
        std::fs::read_to_string(tmp.path().join("live.conf.cfgd-backup")).unwrap(),
        "years of hand edits\n",
        "the copy was taken before the write that failed"
    );
    let out = crate::test_helpers::captured_text(&buf);
    let row = out
        .lines()
        .find(|l| l.contains("live.conf") && l.contains("backed up to"))
        .unwrap_or_else(|| panic!("a failed write still names its copy, got: {out}"));
    assert!(
        row.contains("live.conf.cfgd-backup"),
        "the failure row names where the copy landed, got: {row}"
    );
    assert!(
        row.contains('✗'),
        "the copy rides on the FAILURE row, not a success one, got: {row}"
    );
    // The error first, the copy after: both facts on one row, in the order
    // they happened, rather than either replacing the other.
    let (before, after) = row.split_once(", backed up to ").unwrap();
    assert!(
        before.contains('—') && before.len() > before.find('—').unwrap() + 3,
        "the error keeps its own place ahead of the copy, got: {row}"
    );
    // A failed action that RAN is timed like a successful one, so the row's
    // own elapsed follows the copy's path.
    assert!(
        after
            .split_once(" (")
            .map_or(after, |(path, _)| path)
            .ends_with(".cfgd-backup"),
        "got: {row}"
    );
}

/// A module deploying a whole directory by symlink — the nvim shape — records
/// one aggregate row over every file the link exposes. An edit two levels
/// down is the deployed content moving, so the refresh must see it; skipping
/// every non-file source left the aggregate pinned to the entries that
/// happened to be file-shaped and the sync tick blind to the pull it carried.
#[test]
#[cfg(unix)]
fn a_module_deploying_a_directory_by_symlink_reports_the_file_that_moved_inside_it() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("lua");
    std::fs::create_dir_all(source.join("config")).unwrap();
    std::fs::write(source.join("config/options.lua"), "opt.number = true\n").unwrap();
    std::fs::write(source.join("init.lua"), "require('config')\n").unwrap();
    let target = dir.path().join("home/.config/nvim/lua");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&source, &target).unwrap();

    let mut module = make_resolved_module("nvim");
    module.packages.clear();
    module.files = vec![ResolvedFile {
        source: source.clone(),
        target: target.clone(),
        is_git_source: false,
        strategy: Some(FileStrategy::Symlink),
        encryption: None,
        permissions: None,
        patch: None,
    }];

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let (rtype, rid) = super::format::parse_resource_from_description(
        &super::format::module_files_description("nvim", 1),
    );
    state
        .upsert_managed_resource(&rtype, &rid, "local", None, None)
        .unwrap();

    let modules = vec![module];
    assert_eq!(
        reconciler
            .refresh_link_deployed_hashes(None, &make_empty_resolved(), &modules)
            .unwrap()
            .rows,
        1,
        "a directory the module deploys by symlink contributes its files to the aggregate"
    );
    assert_eq!(
        reconciler
            .refresh_link_deployed_hashes(None, &make_empty_resolved(), &modules)
            .unwrap()
            .rows,
        0,
        "nothing moved, so the aggregate stands"
    );

    std::fs::write(
        source.join("config/options.lua"),
        "opt.number = true\nopt.relativenumber = true\n",
    )
    .unwrap();
    assert_eq!(
        reconciler
            .refresh_link_deployed_hashes(None, &make_empty_resolved(), &modules)
            .unwrap()
            .rows,
        1,
        "an edit two levels under the directory entry moves the module's aggregate"
    );
}

/// The count the daemon words is over FILES THAT MOVED: a module's row is
/// one aggregate over every file its entries deploy, so `1 deployed file
/// refreshed` for a six-entry tree named a unit the number was never in —
/// and `52 deployed files refreshed` for a one-line edit under a 52-file tree
/// was the same row's coverage printed as movement. A count of what moved is
/// never the coverage of the record that moved.
#[test]
#[cfg(unix)]
fn a_one_file_edit_inside_a_module_tree_is_counted_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("lua");
    std::fs::create_dir_all(source.join("config")).unwrap();
    std::fs::write(source.join("config/options.lua"), "a\n").unwrap();
    std::fs::write(source.join("init.lua"), "b\n").unwrap();
    let init = dir.path().join("init.lua");
    std::fs::write(&init, "c\n").unwrap();
    let home = dir.path().join("home/.config/nvim");
    std::fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(&source, home.join("lua")).unwrap();
    std::os::unix::fs::symlink(&init, home.join("init.lua")).unwrap();

    let entry = |src: &std::path::Path, dst: std::path::PathBuf| ResolvedFile {
        source: src.to_path_buf(),
        target: dst,
        is_git_source: false,
        strategy: Some(FileStrategy::Symlink),
        encryption: None,
        permissions: None,
        patch: None,
    };
    let mut module = make_resolved_module("nvim");
    module.packages.clear();
    module.files = vec![
        entry(&source, home.join("lua")),
        entry(&init, home.join("init.lua")),
    ];

    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let (rtype, rid) = super::format::parse_resource_from_description(
        &super::format::module_files_description("nvim", 2),
    );
    state
        .upsert_managed_resource(&rtype, &rid, "local", None, None)
        .unwrap();

    let refreshed = reconciler
        .refresh_link_deployed_hashes(None, &make_empty_resolved(), &[module.clone()])
        .unwrap();
    assert_eq!(
        refreshed,
        RefreshedHashes {
            rows: 1,
            files: None
        },
        "the row had no breakdown to count against, so no number is claimed"
    );

    std::fs::write(
        source.join("config/options.lua"),
        "a\nopt.relativenumber = true\n",
    )
    .unwrap();
    let refreshed = reconciler
        .refresh_link_deployed_hashes(None, &make_empty_resolved(), &[module.clone()])
        .unwrap();
    assert_eq!(
        refreshed,
        RefreshedHashes {
            rows: 1,
            files: Some(1)
        },
        "one file moved under a three-file aggregate: one, not three"
    );

    std::fs::write(source.join("config/keymaps.lua"), "d\n").unwrap();
    std::fs::write(&init, "c2\n").unwrap();
    let refreshed = reconciler
        .refresh_link_deployed_hashes(None, &make_empty_resolved(), &[module])
        .unwrap();
    assert_eq!(
        refreshed,
        RefreshedHashes {
            rows: 1,
            files: Some(2)
        },
        "a file that appeared and one whose bytes moved are each counted once"
    );
}

/// A shortfall names WHY the work was already done when this run is the
/// reason: `already installed` is reserved for state the run did not create,
/// and an entry the run's own provision delivered reads `provisioned by this
/// run`. Both clauses may stand on one row. The predicate folds the installer
/// to its family, the unit every exclusion check agrees on.
#[test]
fn a_shortfall_this_runs_provisions_delivered_is_worded_as_delivered() {
    let brew = Action::Package(PackageAction::Install {
        manager: "brew".to_string(),
        packages: ["neovim", "node", "pipx"]
            .iter()
            .map(|p| p.to_string())
            .collect(),
        origin: "local".to_string(),
    });
    assert_eq!(
        super::action_produced_detail(&brew, Some(1), 2, &[]).as_deref(),
        Some("2 provisioned by this run")
    );
    assert_eq!(
        super::action_produced_detail(&brew, Some(1), 1, &[]).as_deref(),
        Some("1 already installed, 1 provisioned by this run")
    );
    assert_eq!(
        super::action_produced_detail(&brew, Some(1), 0, &[]).as_deref(),
        Some("2 already installed")
    );
    assert_eq!(
        super::action_produced_detail(&brew, Some(3), 3, &[]),
        None,
        "a run that landed everything it named states no shortfall"
    );
    // The report prices the WIDEST settlement, so the column never learns of
    // a shortfall after it is claimed.
    assert_eq!(
        super::widest_produced_detail(&brew).as_deref(),
        Some("1 already installed, 2 provisioned by this run")
    );

    let delivered = vec![("brew".to_string(), "node".to_string())];
    assert!(Reconciler::delivered_by_this_run(
        &delivered, "brew", "node"
    ));
    assert!(
        Reconciler::delivered_by_this_run(&delivered, "brew-cask", "node"),
        "the installer is judged by family"
    );
    assert!(!Reconciler::delivered_by_this_run(
        &delivered, "apt", "node"
    ));
    assert!(!Reconciler::delivered_by_this_run(
        &delivered, "brew", "neovim"
    ));
}
