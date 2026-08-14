use super::*;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use crate::PathDisplayExt;
use crate::config::*;
use crate::providers::{PackageContext, PackageManager};

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

    registry.package_managers.push(Box::new(
        MockPackageManager::new("cargo").with_installed(&["ripgrep"]),
    ));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.cargo = Some(crate::config::CargoSpec {
        file: None,
        packages: vec!["ripgrep".to_string(), "bat".to_string()],
    });

    let printer = test_printer();
    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

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
            },
            ActionResult {
                phase: "files".to_string(),
                description: "test2".to_string(),
                success: false,
                error: Some("failed".to_string()),
                changed: false,
            },
        ],
        status: ApplyStatus::Partial,
        apply_id: 0,
        aborted: None,
        planned_total: 2,
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

    let modules = vec![ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: PathBuf::from("/tmp/nvim-config"),
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
            ModuleActionKind::DeployFiles { files } => {
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
                version: Some("18.19.0".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
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
                version: Some("0.10.2".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
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
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
            },
            ResolvedPackage {
                canonical_name: "ripgrep".to_string(),
                resolved_name: "ripgrep".to_string(),
                manager: "cargo".to_string(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
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

    let modules = vec![ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "neovim".to_string(),
            resolved_name: "neovim".to_string(),
            manager: "brew".to_string(),
            version: Some("0.10.2".to_string()),
            script: None,
            creates: None,
            only_if: None,
            unless: None,
        }],
        files: vec![ResolvedFile {
            source: PathBuf::from("/tmp/nvim-config"),
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
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
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
                        version: Some("0.10.2".to_string()),
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                    },
                    ResolvedPackage {
                        canonical_name: "fd".to_string(),
                        resolved_name: "fd-find".to_string(),
                        manager: "apt".to_string(),
                        version: Some("8.7.0".to_string()),
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
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
            kind: ModuleActionKind::DeployFiles {
                files: vec![ResolvedFile {
                    source: PathBuf::from("/cache/nvim/config"),
                    target: PathBuf::from("/home/user/.config/nvim"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                }],
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
                version: Some("0.10.2".to_string()),
                script: None,
                creates: None,
                only_if: None,
                unless: None,
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

    registry.package_managers.push(Box::new(
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
    registry.package_managers.push(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim"]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![make_resolved_module("nvim")];
    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    // Should have a drift result for ripgrep
    let drift = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "nvim/ripgrep");
    assert!(drift.is_some());
    assert!(!drift.unwrap().matches);

    // nvim/neovim is installed → a passing per-package row (not absent, and not drift).
    let ok = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "nvim/neovim");
    assert!(
        ok.is_some(),
        "installed module package must emit a pass row"
    );
    assert!(ok.unwrap().matches);

    // The missing package must also be recorded as drift in the state store.
    let recorded = state.unresolved_drift().unwrap();
    assert!(
        recorded
            .iter()
            .any(|d| d.resource_type == "module" && d.resource_id == "nvim/ripgrep"),
        "missing module package must record drift: {recorded:?}"
    );
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
                        version: Some("0.10.2".to_string()),
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
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

    registry.package_managers.push(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim", "ripgrep"]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![make_resolved_module("nvim")];
    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    // All packages installed → a passing per-package row each (no blanket
    // "module healthy" row, which would contradict folded-in file-drift rows).
    for pkg in ["nvim/neovim", "nvim/ripgrep"] {
        let row = results
            .iter()
            .find(|r| r.resource_type == "module" && r.resource_id == pkg);
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
    registry.package_managers.push(Box::new(go));

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![ResolvedModule {
        name: "gotools".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "2fa".to_string(),
            resolved_name: "rsc.io/2fa".to_string(),
            manager: "go".to_string(),
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
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

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();
    let row = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "gotools/rsc.io/2fa")
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
            version: None,
            script: Some("curl -sSf https://sh.rustup.rs | sh".into()),
            creates: None,
            only_if: None,
            unless: None,
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

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    // Script packages are skipped in verification — they produce no row at all
    // (neither pass nor drift), so a script-only module yields no module rows.
    let module_rows: Vec<_> = results
        .iter()
        .filter(|r| r.resource_type == "module")
        .collect();
    assert!(
        module_rows.is_empty(),
        "script-only module must not produce verify rows: {module_rows:?}"
    );
}

/// Build a single-package `ResolvedModule` (no defaults) for verify tests.
fn module_one_pkg(name: &str, manager: &str, pkg: &str) -> ResolvedModule {
    let mut m = make_resolved_module(name);
    m.packages = vec![ResolvedPackage {
        canonical_name: pkg.to_string(),
        resolved_name: pkg.to_string(),
        manager: manager.to_string(),
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
    }];
    m
}

#[test]
fn verify_module_package_not_installed_is_module_drift() {
    // A module-only package the host lacks must surface as a `module` non-match,
    // recorded in the state store under `<module>/<name>`.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        MockPackageManager::new("brew").with_installed(&[]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pkg("dev", "brew", "ripgrep")];

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    let row = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "dev/ripgrep")
        .expect("module package must emit a module row");
    assert!(!row.matches, "uninstalled module package must be drift");

    let recorded = state.unresolved_drift().unwrap();
    assert!(
        recorded
            .iter()
            .any(|d| d.resource_type == "module" && d.resource_id == "dev/ripgrep"),
        "module package drift must be recorded: {recorded:?}"
    );
}

#[test]
fn verify_package_in_profile_and_module_appears_once() {
    // The same (manager, name) declared by both the profile and a module must
    // verify once, attributed to the module (module wins), with no duplicate
    // `package:` row for the profile scope.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.package_managers.push(Box::new(
        MockPackageManager::new("brew").with_installed(&[]),
    ));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.brew = Some(crate::config::BrewSpec {
        formulae: vec!["ripgrep".to_string()],
        ..Default::default()
    });
    let printer = test_printer();
    let modules = vec![module_one_pkg("dev", "brew", "ripgrep")];

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    let rows: Vec<_> = results
        .iter()
        .filter(|r| {
            (r.resource_type == "module" && r.resource_id == "dev/ripgrep")
                || (r.resource_type == "package" && r.resource_id == "brew:ripgrep")
        })
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "duplicate profile+module package must verify once: {rows:?}"
    );
    assert_eq!(
        rows[0].resource_type, "module",
        "module wins the dedup: {rows:?}"
    );
}

#[test]
fn verify_module_package_on_unavailable_manager_is_skipped() {
    // CONSISTENCY: a module package whose manager is unavailable on this host
    // cannot be installed or probed here, so it must NOT be reported missing —
    // matching how profile packages on unavailable managers are already skipped.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(MockPackageManager::new("brew").unavailable()));

    let resolved = make_empty_resolved();
    let printer = test_printer();
    let modules = vec![module_one_pkg("dev", "brew", "ripgrep")];

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    assert!(
        !results
            .iter()
            .any(|r| r.resource_type == "module" && r.resource_id == "dev/ripgrep"),
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
    registry
        .package_managers
        .push(Box::new(MockPackageManager::new("brew").unavailable()));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.brew = Some(crate::config::BrewSpec {
        formulae: vec!["ripgrep".to_string()],
        ..Default::default()
    });
    let printer = test_printer();

    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

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
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
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

    let results = verify(&resolved, &registry, &state, &printer, &[module]).unwrap();

    let row = results
        .iter()
        .find(|r| r.resource_type == "system" && r.resource_id == "sysctl.vm.swappiness")
        .expect("module system config must be verified via the effective map");
    assert!(!row.matches);
    assert_eq!(row.expected, "10");
    assert_eq!(row.actual, "60");
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
            version: None,
            script: Some("curl -sSf https://sh.rustup.rs | sh".into()),
            creates: None,
            only_if: None,
            unless: None,
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
                    version: None,
                    script: Some("install-rustup.sh".into()),
                    creates: None,
                    only_if: None,
                    unless: None,
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

#[test]
fn generate_env_file_quoted_and_unquoted() {
    let env = vec![
        crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        },
        crate::config::EnvVar {
            name: "PATH".into(),
            value: "/usr/local/bin:$PATH".into(),
        },
    ];
    let content = super::generate_env_file_content(&env, &[], &[]);
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
        },
        crate::config::EnvVar {
            name: "PATH".into(),
            value: "/usr/local/bin:/home/user/.cargo/bin:$PATH".into(),
        },
    ];
    let content = super::generate_fish_env_content(&env, &[], &[]);
    assert!(content.starts_with("# managed by cfgd"));
    assert!(content.contains("set -gx EDITOR 'nvim'"));
    assert!(content.contains("set -gx PATH '/usr/local/bin' '/home/user/.cargo/bin' '$PATH'"));
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
            },
            crate::config::EnvVar {
                name: "PATH".into(),
                value: "~/bin:/usr/bin".into(),
            },
        ];
        let bash = super::generate_env_file_content(&env, &[], &[]);
        assert!(bash.contains(&format!("export CLIFT_DIR=\"{h}/.local/share/clift\"")));
        assert!(bash.contains(&format!("export PATH=\"{h}/bin:/usr/bin\"")));

        let fish = super::generate_fish_env_content(&env, &[], &[]);
        assert!(fish.contains(&format!("set -gx CLIFT_DIR '{h}/.local/share/clift'")));
        assert!(fish.contains(&format!("set -gx PATH '{h}/bin' '/usr/bin'")));

        let ps = super::generate_powershell_env_content(&env, &[], &[]);
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
        }];
        let fish = super::generate_fish_env_content(&env, &[], &[]);
        assert!(
            fish.contains(&format!("set -gx PATH '{h}/bin' '/usr/bin'")),
            "drive/colon-containing home must stay one PATH part, got: {fish}"
        );
    });
}

#[test]
fn plan_env_empty_when_no_env() {
    let tmp = tempfile::tempdir().unwrap();
    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &[],
        &[],
        crate::config::EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    );
    assert!(actions.is_empty());
}

#[test]
fn plan_env_module_wins_on_conflict() {
    let profile_env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "vim".into(),
    }];
    let modules = vec![ResolvedModule {
        name: "nvim".into(),
        packages: vec![],
        files: vec![],
        env: vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
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
    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &profile_env,
        &[],
        crate::config::EnvScope::Interactive,
        &modules,
        &[],
        &[],
        &[],
        tmp.path(),
    );
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
    }];

    // Write the expected content to a temp file to simulate "already applied"
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join(".cfgd.env");
    let expected = super::generate_env_file_content(&env, &[], &[]);
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
    }];
    let aliases = vec![
        crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        },
        crate::config::ShellAlias {
            name: "ll".into(),
            command: "ls -la".into(),
        },
    ];
    let content = super::generate_env_file_content(&env, &aliases, &[]);
    assert!(content.contains("export EDITOR=\"nvim\""));
    assert!(content.contains("alias vim=\"nvim\""));
    assert!(content.contains("alias ll=\"ls -la\""));
}

#[test]
fn generate_fish_env_with_aliases() {
    let env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
    }];
    let aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
    }];
    let content = super::generate_fish_env_content(&env, &aliases, &[]);
    assert!(content.contains("set -gx EDITOR 'nvim'"));
    assert!(content.contains("abbr -a vim 'nvim'"));
}

#[test]
fn plan_env_aliases_only() {
    let aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
    }];
    let tmp = tempfile::tempdir().unwrap();
    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &[],
        &aliases,
        crate::config::EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    );
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
    }];
    let modules = vec![ResolvedModule {
        name: "nvim".into(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
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
    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &[],
        &profile_aliases,
        crate::config::EnvScope::Interactive,
        &modules,
        &[],
        &[],
        &[],
        tmp.path(),
    );
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
    }];
    let content = super::generate_env_file_content(&[], &aliases, &[]);
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
    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &[],
        &[],
        crate::config::EnvScope::Interactive,
        &[],
        &secret_envs,
        &[],
        &[],
        tmp.path(),
    );
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
    }];
    let secret_envs = vec![("GITHUB_TOKEN".to_string(), "ghp_abc123".to_string())];
    let tmp = tempfile::tempdir().unwrap();
    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &regular_env,
        &[],
        crate::config::EnvScope::Interactive,
        &[],
        &secret_envs,
        &[],
        &[],
        tmp.path(),
    );

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
    }];
    let aliases = vec![crate::config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
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
        },
        crate::config::EnvVar {
            name: "PATH".into(),
            value: r"C:\Users\user\.cargo\bin;$env:PATH".into(),
        },
    ];
    let content = super::generate_powershell_env_content(&env, &[], &[]);
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
        },
        crate::config::ShellAlias {
            name: "ll".into(),
            command: "Get-ChildItem -Force".into(),
        },
    ];
    let content = super::generate_powershell_env_content(&[], &aliases, &[]);
    assert!(content.contains("Set-Alias -Name g -Value 'git'"));
    assert!(content.contains("function ll {"));
    assert!(content.contains("Get-ChildItem -Force @args"));
}

#[test]
fn generate_powershell_env_escapes_quotes() {
    let env = vec![crate::config::EnvVar {
        name: "GREETING".into(),
        value: r#"say "hello""#.into(),
    }];
    let content = super::generate_powershell_env_content(&env, &[], &[]);
    // No $env: reference, so single-quoted (PS single quotes don't need escaping except ')
    assert!(content.contains("$env:GREETING = 'say \"hello\"'"));
}

#[test]
fn generate_powershell_env_empty() {
    let content = super::generate_powershell_env_content(&[], &[], &[]);
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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
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
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::go_like("go")));

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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));

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
    let pm = registry.package_managers[0].as_ref();
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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
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
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
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
    registry
        .package_managers
        .push(Box::new(ScriptedLikeManager {
            name: "widgetmgr".to_string(),
            uninstall_cmd: "widgetmgr rm {package}".to_string(),
        }));
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("cargo")));

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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::with_installed(
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

    let pm = registry.package_managers[0].as_ref();
    let cx = test_package_context(&printer, &state);
    let installed = pm.installed_packages(&cx).unwrap();
    assert!(!installed.contains("ripgrep"));
    assert!(installed.contains("fd"));
}

#[test]
fn apply_package_install_tracks_per_package_managed_resource() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));

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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::with_installed(
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
        },
        crate::config::EnvVar {
            name: "CARGO_HOME".into(),
            value: "/home/user/.cargo".into(),
        },
    ];
    let content = super::generate_env_file_content(&env, &[], &[]);

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content: content.clone(),
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
    }];
    let content = super::generate_env_file_content(&env, &[], &[]);

    // Pre-write identical content
    std::fs::write(&env_path, &content).unwrap();

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content,
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::with_installed(
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));

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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));

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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));

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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("cargo")));

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
    let brew = registry.package_managers[0].as_ref();
    let cargo = registry.package_managers[1].as_ref();
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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
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
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
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
    registry
        .package_managers
        .push(Box::new(UpdateCountingPackageManager::new(
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("apt")));

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
    }];
    let aliases = vec![crate::config::ShellAlias {
        name: "ll".into(),
        command: "ls -la".into(),
    }];
    let content = super::generate_env_file_content(&env, &aliases, &[]);

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content: content.clone(),
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
    let entry = ScriptEntry::Full {
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
    };
    // Should be true even for pre-apply (which defaults to false)
    assert!(super::effective_continue_on_error(
        &entry,
        &ScriptPhase::PreApply
    ));

    let entry_false = ScriptEntry::Full {
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
    };
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

    let full_no_override = ScriptEntry::Full {
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
    };
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
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full {
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
    }];

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
            ScriptEntry::Full {
                run,
                timeout,
                continue_on_error,
                ..
            } => {
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
    let entry = ScriptEntry::Full {
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
    };
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
    let entry = ScriptEntry::Full {
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
    };
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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
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
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));
    registry
        .package_managers
        .push(Box::new(FailingPackageManager::new("apt")));

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

#[test]
fn apply_failed_when_all_actions_fail() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    registry
        .package_managers
        .push(Box::new(FailingPackageManager::new("apt")));

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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
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
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
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
    registry
        .package_managers
        .push(Box::new(PanickingPackageManager {
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let mut resolved = make_empty_resolved();

    // Post-apply script that fails but has continueOnError=true
    resolved.merged.scripts.post_apply = vec![ScriptEntry::Full {
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
    }];

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
    resolved.merged.scripts.post_apply = vec![ScriptEntry::Full {
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
    }];

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

    let output = buf.lock().unwrap();
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
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full {
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
    }];

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
    resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full {
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
    }];

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
    let entry = ScriptEntry::Full {
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
    };

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
    let guarded = ScriptEntry::Full {
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
    };

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
    let guarded = ScriptEntry::Full {
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
    };

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
        // id would be recorded under a row nothing can find again.
        assert_eq!(
            action_resource_info(action),
            super::parse_resource_from_description(&desc),
            "the recorded id and the id parsed back out of {desc:?} must agree"
        );
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
    // bootstrap/skip reach this parser — install/uninstall are split
    // per-package by `parse_package_description` first — and both of those
    // collapsed onto the bare verb, so every manager shared one row.
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
        kind: ModuleActionKind::DeployFiles {
            files: vec![
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
            ],
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
    let actions = reconciler.plan_modules(&modules, ReconcileContext::Reconcile);
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
                origin: "local".into(),
            }),
            Action::Secret(SecretAction::ResolveEnv {
                provider: "1password".into(),
                reference: "Vault/Secret".into(),
                envs: vec!["TOKEN".into()],
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
fn format_module_action_item_deploy_truncates_many_files() {
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
        kind: ModuleActionKind::DeployFiles { files },
        origin: None,
    };
    let item = super::format_module_action_item(&action);
    assert!(item.starts_with("deploy "));
    assert!(item.contains("5 files"));
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
    }];
    let profile_aliases = vec![crate::config::ShellAlias {
        name: "g".into(),
        command: "git".into(),
    }];
    let modules = vec![ResolvedModule {
        name: "mod1".into(),
        packages: vec![],
        files: vec![],
        env: vec![
            crate::config::EnvVar {
                name: "A".into(),
                value: "2".into(),
            },
            crate::config::EnvVar {
                name: "B".into(),
                value: "3".into(),
            },
        ],
        aliases: vec![crate::config::ShellAlias {
            name: "g".into(),
            command: "git status".into(),
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

    let (env, aliases) = super::merge_module_env_aliases(&profile_env, &profile_aliases, &modules);
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
    }];
    let content = super::generate_powershell_env_content(&env, &[], &[]);
    // Single quotes in values are doubled in PS
    assert!(content.contains("$env:MSG = 'it''s a test'"));
}

#[test]
fn generate_fish_env_escapes_single_quotes() {
    let env = vec![crate::config::EnvVar {
        name: "MSG".into(),
        value: "it's a test".into(),
    }];
    let content = super::generate_fish_env_content(&env, &[], &[]);
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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
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
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[test]
fn apply_package_bootstrap_makes_manager_available() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(BootstrappablePackageManager::new("snap")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Package(PackageAction::Bootstrap {
                manager: "snap".to_string(),
                method: "auto".to_string(),
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

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);
    assert!(
        result.action_results[0].description.contains("bootstrap"),
        "desc: {}",
        result.action_results[0].description
    );

    // Manager should now be available
    assert!(registry.package_managers[0].is_available());
}

#[test]
fn apply_package_bootstrap_unknown_manager_errors() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no managers
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &Owner::profile("test"),
            vec![Action::Package(PackageAction::Bootstrap {
                manager: "nonexistent".to_string(),
                method: "auto".to_string(),
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

    // Should fail — unknown manager
    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.failed(), 1);
    assert!(result.action_results[0].error.is_some());
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
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
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
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
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
    // Byte-identical, not a substring: the sentence R3 moved out of
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
    registry
        .system_configurators
        .push(Box::new(MockSystemConfigurator::new("sysctl").with_drift(
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
    registry.system_configurators.push(Box::new(
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "nvim".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "neovim".to_string(),
            resolved_name: "neovim".to_string(),
            manager: "brew".to_string(),
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
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
                        version: None,
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
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
    let installed = registry.package_managers[0]
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
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
                kind: ModuleActionKind::DeployFiles { files: vec![file] },
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
                kind: ModuleActionKind::DeployFiles { files: vec![file] },
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                        permissions: None,
                        patch: None,
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
fn apply_module_install_packages_bootstraps_when_needed() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(BootstrappablePackageManager::new("brew")));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "jq".to_string(),
            resolved_name: "jq".to_string(),
            manager: "brew".to_string(),
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
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
                module_name: "tools".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "jq".to_string(),
                        resolved_name: "jq".to_string(),
                        manager: "brew".to_string(),
                        version: None,
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
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

    // Manager should have been bootstrapped and package installed
    assert!(registry.package_managers[0].is_available());
    let cx = test_package_context(&printer, &state);
    assert!(
        registry.package_managers[0]
            .installed_packages(&cx)
            .unwrap()
            .contains("jq")
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

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
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
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
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

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
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

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
    // Should produce DeployFiles (encryption=Always + copy is OK, and file has sops marker)
    assert_eq!(actions.len(), 1);
    match &actions[0].1 {
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::DeployFiles { files } => {
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

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
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

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
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

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
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
        },
        crate::config::EnvVar {
            name: "CARGO_HOME".into(),
            value: "/home/user/.cargo".into(),
        },
    ];
    let aliases = vec![crate::config::ShellAlias {
        name: "g".into(),
        command: "git".into(),
    }];
    let content = super::generate_fish_env_content(&env, &aliases, &[]);
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
    }];
    let content = super::generate_powershell_env_content(&env, &[], &[]);
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
    }];
    let content = super::generate_powershell_env_content(&[], &aliases, &[]);
    assert!(content.contains("function ll {"));
    assert!(content.contains("Get-ChildItem -Force @args"));
}

#[test]
fn generate_fish_env_path_splitting() {
    // Fish should split PATH values on :
    let env = vec![crate::config::EnvVar {
        name: "PATH".into(),
        value: "/usr/bin:/usr/local/bin:$PATH".into(),
    }];
    let content = super::generate_fish_env_content(&env, &[], &[]);
    assert!(
        content.contains("set -gx PATH '/usr/bin' '/usr/local/bin' '$PATH'"),
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

    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();
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

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

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
    registry.package_managers.push(Box::new(
        MockPackageManager::new("apt").with_installed(&["git"]),
    ));

    let mut resolved = make_empty_resolved();
    resolved.merged.packages.apt = Some(crate::config::AptSpec {
        file: None,
        packages: vec!["git".to_string(), "tmux".to_string()],
    });

    let printer = test_printer();
    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

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
    assert_eq!(tmux_result.actual, "missing");
}

// --- format_action_description additional tests ---

#[test]
fn format_action_description_env_write_file() {
    let action = Action::Env(EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/user/.cfgd.env"),
        content: "export FOO=bar\n".to_string(),
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
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
                },
                ResolvedPackage {
                    canonical_name: "ripgrep".to_string(),
                    resolved_name: "ripgrep".to_string(),
                    manager: "brew".to_string(),
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
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
        kind: ModuleActionKind::DeployFiles {
            files: vec![
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
            ],
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
fn format_action_description_package_bootstrap() {
    let action = Action::Package(PackageAction::Bootstrap {
        manager: "brew".to_string(),
        method: "curl".to_string(),
        origin: "local".to_string(),
    });
    let desc = format_action_description(&action);
    assert_eq!(desc, "package:brew:bootstrap");
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
    let state = test_state();
    let tmp = tempfile::tempdir().unwrap();
    let env_path = tmp.path().join("test.env");
    let expected = "export FOO=\"bar\"\n";
    std::fs::write(&env_path, expected).unwrap();

    let mut results = Vec::new();
    super::verify_env_file(&env_path, expected, &state, &mut results);

    assert_eq!(results.len(), 1);
    assert!(results[0].matches);
    assert_eq!(results[0].resource_type, "env");
    assert_eq!(results[0].expected, "current");
    assert_eq!(results[0].actual, "current");
}

#[test]
fn verify_env_file_stale_when_content_differs() {
    let state = test_state();
    let tmp = tempfile::tempdir().unwrap();
    let env_path = tmp.path().join("test.env");
    std::fs::write(&env_path, "old content").unwrap();

    let mut results = Vec::new();
    super::verify_env_file(&env_path, "new content", &state, &mut results);

    assert_eq!(results.len(), 1);
    assert!(!results[0].matches);
    assert_eq!(results[0].expected, "current");
    assert_eq!(results[0].actual, "stale");
}

#[test]
fn verify_env_file_missing_when_file_absent() {
    let state = test_state();
    let tmp = tempfile::tempdir().unwrap();
    let env_path = tmp.path().join("nonexistent.env");

    let mut results = Vec::new();
    super::verify_env_file(&env_path, "expected content", &state, &mut results);

    assert_eq!(results.len(), 1);
    assert!(!results[0].matches);
    assert_eq!(results[0].expected, "present");
    assert_eq!(results[0].actual, "missing");
}

// --- merge_module_env_aliases tests ---

#[test]
fn merge_module_env_aliases_empty() {
    let (env, aliases) = super::merge_module_env_aliases(&[], &[], &[]);
    assert!(env.is_empty());
    assert!(aliases.is_empty());
}

#[test]
fn merge_module_env_aliases_combines_profile_and_modules() {
    let profile_env = vec![crate::config::EnvVar {
        name: "EDITOR".into(),
        value: "vim".into(),
    }];
    let profile_aliases = vec![crate::config::ShellAlias {
        name: "g".into(),
        command: "git".into(),
    }];
    let modules = vec![ResolvedModule {
        name: "test".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![crate::config::EnvVar {
            name: "PAGER".into(),
            value: "less".into(),
        }],
        aliases: vec![crate::config::ShellAlias {
            name: "ll".into(),
            command: "ls -la".into(),
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

    let (env, aliases) = super::merge_module_env_aliases(&profile_env, &profile_aliases, &modules);
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
    }];
    let modules = vec![ResolvedModule {
        name: "test".to_string(),
        packages: vec![],
        files: vec![],
        env: vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
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

    let (env, _) = super::merge_module_env_aliases(&profile_env, &[], &modules);
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Hardlink),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }],
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }],
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![file.clone()],
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_dir.clone(),
                        target: target_dir.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }],
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }],
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }],
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
    registry
        .system_configurators
        .push(Box::new(DriftingConfigurator));

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
    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

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
    registry
        .system_configurators
        .push(Box::new(HealthyConfigurator));

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
    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

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
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));
        registry
            .package_managers
            .push(Box::new(FailingPackageManager::new("apt")));

        let reconciler = Reconciler::new(&registry, &state);
        let mut resolved = make_empty_resolved();

        // Post-apply script that fails with continueOnError=true → exercises
        // the Role::Warn branch in apply.rs (continueOnError-warning).
        resolved.merged.scripts.post_apply = vec![ScriptEntry::Full {
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
        }];

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
fn format_plan_items_package_bootstrap() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("test"),
        vec![Action::Package(PackageAction::Bootstrap {
            manager: "brew".into(),
            method: "curl | bash".into(),
            origin: "corp".into(),
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("bootstrap brew"), "got: {}", items[0]);
    assert!(items[0].contains("curl | bash"), "got: {}", items[0]);
    assert!(items[0].contains("<- corp"), "got: {}", items[0]);
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
            ModuleActionKind::DeployFiles {
                files: vec![ResolvedFile {
                    source: PathBuf::from("/cache/nvim/config"),
                    target: PathBuf::from("/home/user/.config/nvim"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                }],
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
        vec![Action::Module(ModuleAction::local(
            "nvim",
            ModuleActionKind::DeployFiles {
                files: vec![ResolvedFile {
                    source: PathBuf::from("/cache/nvim/config"),
                    target: PathBuf::from("/home/user/.config/nvim"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                    patch: None,
                }],
            },
        ))],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(!items[0].contains(" <- "), "got: {}", items[0]);
}

#[test]
fn format_module_action_item_deploy_many_files_truncates() {
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
            kind: ModuleActionKind::DeployFiles { files },
            origin: None,
        })],
    );
    let items = plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("5 files"), "got: {}", items[0]);
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
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
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
}

impl BootstrappingPackageManager {
    fn new(name: &str, path_dirs: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            available: std::sync::Mutex::new(false),
            bootstrap_called: std::sync::Mutex::new(false),
            install_calls: std::sync::Mutex::new(Vec::new()),
            path_dirs_after: path_dirs.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl PackageManager for BootstrappingPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        *self.available.lock().unwrap()
    }
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
        Some(crate::providers::BootstrapPlan::new("stub"))
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
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn path_dirs(&self, _: &PackageContext<'_>) -> Vec<String> {
        self.path_dirs_after.clone()
    }
}

/// Build the single-module fixture both out-of-band-write tests drive:
/// one `brew` package, and the `InstallPackages` action the Modules phase
/// would run for it.
fn brew_install_fixture() -> (Vec<ResolvedModule>, ModuleAction) {
    let package = ResolvedPackage {
        canonical_name: "ripgrep".to_string(),
        resolved_name: "ripgrep".to_string(),
        manager: "brew".to_string(),
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
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
    registry
        .package_managers
        .push(Box::new(BootstrappingPackageManager::new(
            "brew", path_dirs,
        )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();
    let (modules, action) = brew_install_fixture();
    let printer = test_printer();

    let (desc, changed) = reconciler
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
        )
        .expect("module action must succeed");
    assert!(
        changed,
        "a manager-backed install counts as changed: {desc}"
    );
    state
}

#[test]
#[serial_test::serial]
fn apply_module_install_packages_bootstraps_without_writing_env_out_of_band() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    let state = run_brew_module_action(&["/opt/homebrew/bin", "/opt/homebrew/sbin"]);

    // The generated env file has exactly one writer — the Env phase. An
    // out-of-band append here would be erased by the next plan's wholesale
    // rewrite, so the bootstrapped PATH would vanish on the second apply.
    let env_path = tmp_home.path().join(".cfgd.env");
    assert!(
        !env_path.exists(),
        "the Modules phase must not write {}",
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
        "a successful bootstrap must record the manager's PATH directories in order"
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
    registry
        .package_managers
        .push(Box::new(BootstrappingPackageManager::new(
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
            Action::Env(EnvAction::WriteEnvFile { path, content })
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

    // The record is what a later plan reads back, so a second apply is a no-op
    // rather than a rewrite.
    let replan = reconciler
        .plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            modules,
            ReconcileContext::Apply,
        )
        .unwrap();
    assert_eq!(
        planned_env_file_content(&replan).as_deref(),
        Some(contents.as_str()),
        "the next plan must re-derive byte-identical content from the record"
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
            version: None,
            script: None,
            creates: None,
            only_if: None,
            unless: None,
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
                        version: None,
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
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
                            version: None,
                            script: Some(format!("touch {}", marker_a.display())),
                            creates: None,
                            only_if: None,
                            unless: None,
                        },
                        ResolvedPackage {
                            canonical_name: "pkg-b".to_string(),
                            resolved_name: "pkg-b".to_string(),
                            manager: "script".to_string(),
                            version: None,
                            script: Some(format!("touch {}", marker_b.display())),
                            creates: None,
                            only_if: None,
                            unless: None,
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
                        version: None,
                        script: Some("exit 3".to_string()),
                        creates: None,
                        only_if: None,
                        unless: None,
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
                        version: None,
                        script: Some(format!("touch {}", marker.display())),
                        creates,
                        only_if,
                        unless,
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }],
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
    registry
        .package_managers
        .push(Box::new(MockPackageManager::new("apt")));
    registry
        .package_managers
        .push(Box::new(BootstrappingPackageManager::new("brew", &[])));
    let reconciler = Reconciler::new(&registry, &state);

    let module = ResolvedModule {
        name: "multimgr".to_string(),
        packages: vec![
            crate::modules::ResolvedPackage {
                canonical_name: "p1".to_string(),
                resolved_name: "p1".to_string(),
                manager: "unknown-mgr".to_string(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
            },
            crate::modules::ResolvedPackage {
                canonical_name: "p2".to_string(),
                resolved_name: "p2".to_string(),
                manager: "brew".to_string(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
            },
            crate::modules::ResolvedPackage {
                canonical_name: "p3".to_string(),
                resolved_name: "p3".to_string(),
                manager: "apt".to_string(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
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

    let actions = reconciler.plan_modules(&[module], ReconcileContext::Apply);
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: true,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
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
        on_change_scripts: vec![ScriptEntry::Full {
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
        }],
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
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                        permissions: None,
                        patch: None,
                    }],
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
    resolved.merged.scripts.on_change = vec![ScriptEntry::Full {
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
    }];

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
    }]
}

#[test]
fn env_targets_empty_yields_nothing() {
    let home = Path::new("/h");
    let t = env_targets(
        &[],
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
        EnvScope::All,
        home,
        &probe,
        EnvPlatform::Linux,
    );
    let b = env_targets(
        &one_env(),
        &[],
        &[],
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
        },
        EnvVar {
            name: "PATH".into(),
            value: "/usr/bin:/bin".into(),
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
    let (actions, _w) = Reconciler::plan_env_with_home(
        &one_env(),
        &[],
        EnvScope::All,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    );
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
    let (actions, _w) = Reconciler::plan_env_with_home(
        &one_env(),
        &[],
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        tmp.path(),
    );
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
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
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
        PackageAction::Bootstrap {
            manager: "brew".to_string(),
            method: "curl".to_string(),
            origin: "profile".to_string(),
        },
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
        3,
        "Bootstrap/Uninstall/Skip must pass through untouched"
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
        &one_env(),
        &[],
        &[],
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
        &one_env(),
        &[],
        &[],
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
    ScriptEntry::Full {
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
    }
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

    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &[],
        &[],
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &managed,
        home.path(),
    );

    assert_eq!(actions.len(), 1, "{actions:?}");
    match &actions[0] {
        Action::Env(EnvAction::WriteEnvFile { path, content }) => {
            assert_eq!(path, &env_file);
            assert_eq!(content, neutral);
        }
        other => panic!("expected a managed-file rewrite, got {other:?}"),
    }

    // Already neutral: nothing left to strip.
    std::fs::write(&env_file, neutral).unwrap();
    let (actions, _) = Reconciler::plan_env_with_home(
        &[],
        &[],
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &managed,
        home.path(),
    );
    assert!(actions.is_empty(), "{actions:?}");

    // A file cfgd's generator did not write is not cfgd's to strip.
    std::fs::write(&env_file, "export FOO=\"user-authored\"\n").unwrap();
    let (actions, _) = Reconciler::plan_env_with_home(
        &[],
        &[],
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &managed,
        home.path(),
    );
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

    let (actions, _warnings) = Reconciler::plan_env_with_home(
        &[],
        &[],
        EnvScope::Interactive,
        &[],
        &[],
        &[],
        &[],
        home.path(),
    );

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
    }];

    let (actions, _warnings) =
        reconciler.plan_env(&env, &[], EnvScope::Interactive, &[], &[], &[], &[]);

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

const LANE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
        Some(crate::providers::BootstrapPlan::new("stub"))
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        let label = format!("bootstrap:{}", self.name);
        self.record(label.clone());
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
    fn update(&self, cx: &PackageContext<'_>) -> Result<()> {
        // npm's `update` resolves its global prefix from `cx.state`, and an
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

fn bootstrap_action(manager: &str) -> Action {
    Action::Package(PackageAction::Bootstrap {
        manager: manager.to_string(),
        method: "auto".to_string(),
        origin: "local".to_string(),
    })
}

fn owner_resolved_package(manager: &str, package: &str) -> ResolvedPackage {
    ResolvedPackage {
        canonical_name: package.to_string(),
        resolved_name: package.to_string(),
        manager: manager.to_string(),
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
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
                    kind: ModuleActionKind::DeployFiles { files: vec![] },
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
fn bootstrap_group_is_built_at_rank_one() {
    let phase = Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("work"),
        vec![
            install_action("brew", &["ripgrep"]),
            bootstrap_action("brew"),
            module_install_action("nvim", "brew", "neovim"),
        ],
    );

    assert_eq!(
        owner_tokens(&phase),
        vec!["profile:work", "cfgd:managers", "module:nvim"],
        "a bootstrap is cfgd's, not the profile's whose planner emitted it"
    );
    assert_eq!(phase.groups()[1].actions.len(), 1);
}

#[test]
fn no_bootstrap_builds_no_managers_group() {
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
fn bootstrap_dispatches_before_the_install_it_enables() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![
        install_action("brew", &["ripgrep"]),
        bootstrap_action("brew"),
    ]);
    assert_eq!(
        owner_tokens(&plan.phases[0]),
        vec!["profile:work", "cfgd:managers"],
        "the display order this test exists to contradict"
    );

    let result = run_apply(&reconciler, &plan, &[], None);

    assert_eq!(
        dispatch_log(&log),
        vec!["bootstrap:brew", "install:brew:ripgrep"]
    );
    assert_eq!(result.status, ApplyStatus::Success);
    assert!(
        result.action_results.iter().all(|r| r.error.is_none()),
        "no action may fail for want of the manager the run itself installs: {:?}",
        result.action_results
    );
}

#[test]
fn module_package_work_dispatches_before_the_bootstrap_tier() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, true)));
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("pipx", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![
        bootstrap_action("pipx"),
        module_install_action("nvim", "brew", "neovim"),
    ]);
    let modules = vec![module_for("nvim", "brew", "neovim")];

    let result = run_apply(&reconciler, &plan, &modules, None);

    assert_eq!(
        dispatch_log(&log),
        vec!["install:brew:neovim", "bootstrap:pipx"],
        "module-owned package work completes before the bootstrap tier begins"
    );
    assert_eq!(result.status, ApplyStatus::Success);
}

#[test]
fn module_package_work_dispatches_before_profile_package_work() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, true)));
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
fn brew_bootstrap_precedes_pipx_bootstrap() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, false)));
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("pipx", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![bootstrap_action("brew"), bootstrap_action("pipx")]);
    assert_eq!(
        owner_tokens(&plan.phases[0]),
        vec!["cfgd:managers"],
        "both bootstraps share one group, so plan order is the only order there is"
    );

    let result = run_apply(&reconciler, &plan, &[], None);

    assert_eq!(
        dispatch_log(&log),
        vec!["bootstrap:brew", "bootstrap:pipx"],
        "planned bootstraps run serially among themselves, in plan order"
    );
    assert_eq!(result.status, ApplyStatus::Success);
}

#[test]
fn planned_bootstrap_is_skipped_when_its_manager_is_already_available() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![
        bootstrap_action("brew"),
        module_install_action("nvim", "brew", "neovim"),
    ]);
    let modules = vec![module_for("nvim", "brew", "neovim")];

    let result = run_apply(&reconciler, &plan, &modules, None);

    let events = dispatch_log(&log);
    assert_eq!(
        events.iter().filter(|e| *e == "bootstrap:brew").count(),
        1,
        "the module's implicit bootstrap already installed brew: {events:?}"
    );
    assert_eq!(events[0], "bootstrap:brew");

    // The action still completes: what it promises is an available manager,
    // not an installation.
    let bootstrap = result
        .action_results
        .iter()
        .find(|r| r.description == "package:brew:bootstrap")
        .expect("the planned bootstrap reports a result of its own");
    assert!(bootstrap.success);
    assert!(bootstrap.changed, "applied, not skipped");
    assert_eq!(result.planned_total, 2);
    assert_eq!(
        state.journal_entries(result.apply_id).unwrap().len(),
        2,
        "one journal row per planned action, bootstrap included"
    );
}

#[test]
fn action_index_is_the_plan_position_not_the_dispatch_counter() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let build_plan = || Plan {
        phases: vec![
            Phase::from_actions(
                PhaseName::Packages,
                &Owner::profile("work"),
                vec![
                    install_action("brew", &["fd"]),
                    bootstrap_action("brew"),
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
            "brew:install:fd",
            "brew:bootstrap",
            "nvim:packages:neovim",
            "/home/u/.gitconfig",
        ],
        "indices follow the flattened group order, not the tiers"
    );

    // Row ids ascend in insertion order, which is dispatch order — and it is a
    // different order, which is what makes the derivation change observable.
    let mut by_dispatch: Vec<(i64, &str)> = entries
        .iter()
        .map(|e| (e.id, e.resource_id.as_str()))
        .collect();
    by_dispatch.sort_by_key(|(id, _)| *id);
    assert_eq!(
        by_dispatch.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
        vec![
            "nvim:packages:neovim",
            "brew:bootstrap",
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

    fn run(self, drive: impl FnOnce()) -> ConcurrentOutcome {
        let (printer, cap) = crate::output::Printer::for_test_doc();
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
            (result, state, crate::output::strip_ansi(&cap.human()))
        });
        drive();
        let (result, state, transcript) = worker.join().expect("apply thread");
        ConcurrentOutcome {
            result,
            state,
            transcript,
        }
    }
}

fn lane_registry(managers: Vec<DispatchLogManager>) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    for manager in managers {
        registry.package_managers.push(Box::new(manager));
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
fn bootstrap_action_drains_the_phase() {
    let probe = LaneProbe::holding(&["bootstrap:brew", "brew:fd", "apt:curl"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        bootstrap_action("brew"),
        install_action("brew", &["fd"]),
        install_action("apt", &["curl"]),
    ]);

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan).run(move || {
        assert!(driver.await_started("bootstrap:brew"));
        assert_eq!(
            driver.in_flight(),
            1,
            "nothing may overlap a bootstrap: {:?}",
            driver.events()
        );
        driver.release("bootstrap:brew");
        // Once the gate releases, the two installs it enabled run in their own
        // lanes — so the drain was the bootstrap's, not the phase's.
        assert!(
            driver.await_in_flight(2),
            "the phase stayed drained after the bootstrap finished: {:?}",
            driver.events()
        );
        driver.release_all();
    });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(event_at(&events, "end:bootstrap:brew") < event_at(&events, "start:brew:fd"));
    assert!(event_at(&events, "end:bootstrap:brew") < event_at(&events, "start:apt:curl"));
    assert_eq!(probe.peak(), 2);
}

#[test]
fn unavailable_manager_action_drains_the_phase() {
    // The implicit bootstrap inside a module install: no planned `Bootstrap`
    // action exists anywhere in this plan, so a gate keyed on the action
    // variant would miss it entirely.
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
    assert!(
        !plan.phases[0]
            .actions()
            .any(|a| matches!(a, Action::Package(PackageAction::Bootstrap { .. }))),
        "the fixture's whole point is that nothing planned a bootstrap"
    );
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
    assert!(
        dispatch_log(&log).contains(&"bootstrap:brew".to_string()),
        "the module install bootstrapped brew inline: {:?}",
        dispatch_log(&log)
    );
    assert_eq!(probe.peak(), 1);
}

#[test]
fn manager_becomes_available_mid_phase() {
    // The gate's predicate is a dispatch-time read, so once the bootstrap has
    // made brew resolve, brew's next action stops draining and dispatches
    // beside another manager's.
    let probe = LaneProbe::holding(&["brew:bpkg", "apt:cpkg"]);
    let log = new_dispatch_log();
    let registry = lane_registry(vec![
        DispatchLogManager::new("brew", &log, false).with_probe(&probe),
        DispatchLogManager::new("apt", &log, true).with_probe(&probe),
    ]);
    let plan = packages_phase(vec![
        module_install_action("a", "brew", "apkg"),
        module_install_action("b", "brew", "bpkg"),
        module_install_action("c", "apt", "cpkg"),
    ]);
    let modules = vec![
        module_for("a", "brew", "apkg"),
        module_for("b", "brew", "bpkg"),
        module_for("c", "apt", "cpkg"),
    ];

    let driver = std::sync::Arc::clone(&probe);
    let outcome = ConcurrentApply::new(registry, plan)
        .with_modules(modules)
        .run(move || {
            assert!(
                driver.await_in_flight(2),
                "after the bootstrap, brew's lane runs beside apt's again: {:?}",
                driver.events()
            );
            driver.release_all();
        });

    assert_eq!(outcome.result.status, ApplyStatus::Success);
    let events = probe.events();
    assert!(event_at(&events, "end:brew:apkg") < event_at(&events, "start:brew:bpkg"));
    assert_eq!(probe.peak(), 2);
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
    registry.system_configurators.push(Box::new(
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
    registry.system_configurators.push(Box::new(
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

    let mut module = make_resolved_module("agent");
    module.packages = vec![];
    module.files = vec![ResolvedFile {
        source: PathBuf::from("/tmp/cfgd-agent.service"),
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
            bootstrap_action("brew"),
            module_install_action("nvim", "brew", "neovim"),
        ],
    );

    phase.retain_actions(|a| !matches!(a, Action::Package(PackageAction::Bootstrap { .. })));

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
            bootstrap_action("brew"),
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
    // internal order, so that is the permutation the hash must ignore.
    let actions = || {
        vec![
            install_action("brew", &["ripgrep"]),
            module_install_action("nvim", "brew", "neovim"),
            bootstrap_action("brew"),
            install_action("apt", &["fd"]),
        ]
    };
    let permuted_actions = || {
        let mut a = actions();
        a.reverse();
        a
    };

    let plan = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &profile,
            actions(),
        )],
        warnings: vec![],
    };

    let permuted = Plan {
        phases: vec![Phase::from_actions(
            PhaseName::Packages,
            &profile,
            permuted_actions(),
        )],
        warnings: vec![],
    };

    let walk: Vec<String> = plan.phases[0].actions().map(format_plan_item).collect();
    let permuted_walk: Vec<String> = permuted.phases[0].actions().map(format_plan_item).collect();
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
fn apply_manager_plan(
    registry: &ProviderRegistry,
    state: &crate::state::StateStore,
    plan: &Plan,
) -> (ApplyResult, String) {
    let (printer, buf) = Printer::for_test();
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
    // The §3.3 edge `apt(index) -> curl(prereq) -> brew(provision)`, minus the
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
    // §3.4: brew's provision fails, so neither npm nor pnpm — which install
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
            .matches("did not run — brew failed earlier in this phase")
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
    // §4's producer-before-consumer rule, which the phase's split dispatch is
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
fn bootstrap_renders_in_cfgd_managers_group() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![
        install_action("brew", &["ripgrep"]),
        bootstrap_action("brew"),
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
fn bootstrap_detail_names_every_declaring_owner() {
    // The claimed-away shape: the SAME package under both the profile and the
    // module, which is the only fixture that catches a derivation built on
    // `effective_desired_packages` (whose claim rule drops the profile row).
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let plan = packages_phase(vec![bootstrap_action("brew")]);
    let resolved = resolved_for("work", &["neovim"]);
    let modules = vec![module_for("nvim", "brew", "neovim")];
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &modules);

    assert!(
        out.contains("for profile:work, module:nvim"),
        "the attribution names every declarer, profile-first: {out}"
    );

    // A failed bootstrap gives the detail slot to the error instead: one slot
    // cannot carry both, and the error is what the reader must act on.
    let empty_registry = ProviderRegistry::new();
    let failing = Reconciler::new(&empty_registry, &state);
    let (_, failed_out) = apply_transcript(&failing, &plan, &resolved, &modules);
    assert!(
        failed_out.contains("not found in registry"),
        "the collapsed error takes the slot: {failed_out}"
    );
    assert!(
        !failed_out.contains("for profile:work"),
        "the attribution must not survive beside an error: {failed_out}"
    );
}

#[test]
fn bootstrap_group_is_display_only() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, false)));
    let reconciler = Reconciler::new(&registry, &state);

    let action = bootstrap_action("brew");
    assert_eq!(
        crate::reconciler::format_action_description(&action),
        "package:brew:bootstrap",
        "the resource id reads no owner"
    );

    let plan = packages_phase(vec![action]);
    let resolved = resolved_for("work", &["ripgrep"]);
    let (result, _) = apply_transcript(&reconciler, &plan, &resolved, &[]);

    assert_eq!(result.action_results.len(), 1);
    assert_eq!(
        result.action_results[0].description,
        "package:brew:bootstrap"
    );
    assert_eq!(result.action_results[0].phase, "packages");
    assert_eq!(result.planned_total, 1);
}

#[test]
fn metadata_detail_is_muted_and_error_detail_is_not() {
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
        detail_is_styled(&raw, "unchanged"),
        "a metadata detail is muted"
    );
    assert!(
        !detail_is_styled(&raw, "package error"),
        "an error detail is never muted"
    );
}

#[test]
fn packages_tree_renders_profile_first_while_modules_execute_first() {
    let log = new_dispatch_log();
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(DispatchLogManager::new("brew", &log, true)));
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));
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

    let module_names = vec!["nvim".to_string(), "wsl-tools".to_string()];
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
            modules: &module_names,
            trigger: None,
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
        out.contains("2 action(s) succeeded"),
        "the rollup reconciles against the planned count: {out}"
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
    let only_names = vec!["wsl-tools".to_string()];
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
            modules: &only_names,
            trigger: None,
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
                Some(i) if l.ends_with("s)") && l.starts_with('\u{2713}') => l[..i].to_string(),
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
            "\u{2713} Apply complete \u{2014} 1 action(s) succeeded".to_string(),
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
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));
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
            kind: ModuleActionKind::DeployFiles { files: vec![] },
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
    registry
        .package_managers
        .push(Box::new(FailingPackageManager::new("brew")));
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
fn action_notes_render_under_the_status_they_belong_to() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(NotePushingManager::new("brew")));
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = resolved_for("work", &["neovim"]);

    let plan = packages_phase(vec![install_action("brew", &["neovim"])]);
    let (_, out) = apply_transcript(&reconciler, &plan, &resolved, &[]);
    let lines = transcript_lines(&out);

    // Targets the install action's own status rather than the first checkmark
    // seen: other lines in the transcript carry the same marker.
    let status = lines
        .iter()
        .position(|l| l.contains("brew install neovim"))
        .expect("the action's status");
    assert_eq!(
        lines[status + 1].trim(),
        "\u{26A0} [brew] add /opt/brew/bin to PATH",
        "one warn line per note, in order, under the status: {out}"
    );
    assert_eq!(
        lines[status + 2].trim(),
        "\u{26A0} [brew] restart your shell",
        "in order: {out}"
    );
    assert!(
        !out.contains("Post-install notes"),
        "the sub-header is gone with print_caveats: {out}"
    );
}

#[test]
fn an_empty_note_drain_emits_nothing() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(TrackingPackageManager::new("brew")));
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
    fn bootstrap_plan(&self) -> Option<crate::providers::BootstrapPlan> {
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
    fn update(&self, _: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
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
fn configurator_narration_renders_attached_under_its_system_action_line() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(NarratingConfigurator));
    let reconciler = Reconciler::new(&registry, &state);

    let (_, out) = apply_transcript(
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
    // Untagged: the line above already says which configurator spoke.
    assert_eq!(
        lines[status + 1].trim(),
        "\u{2299} sysctl -w net.ipv4.ip_forward=1",
        "narration attaches under the action, keeping its Info role: {out}"
    );
    assert_eq!(
        lines[status + 2].trim(),
        "\u{26A0} reload deferred: /proc is read-only",
        "and the Warn role survives the trip through the sink: {out}"
    );
    let attached_indent = lines[status + 1].len() - lines[status + 1].trim_start().len();
    let status_indent = lines[status].len() - lines[status].trim_start().len();
    assert!(
        attached_indent > status_indent,
        "an attached note sits one level deeper than the line it belongs to: {out}"
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
