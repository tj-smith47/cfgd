use super::*;
use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;

use crate::config::*;
use crate::providers::PackageManager;

use crate::providers::StubPackageManager as MockPackageManager;
use crate::test_helpers::{
    MockSecretBackend, MockSecretProvider, MockSystemConfigurator, make_empty_resolved,
    make_resolved_module, test_printer, test_state,
};

#[test]
fn empty_plan_has_eight_phases() {
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

    assert_eq!(plan.phases.len(), 8);
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
    assert_eq!(pre_phase.actions.len(), 1);

    // Post-scripts phase should have the post_reconcile script
    let post_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::PostScripts)
        .unwrap();
    assert_eq!(post_phase.actions.len(), 1);
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
        )
        .unwrap();

    // Empty plan — no actions means success with 0 results
    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 0);
}

#[test]
fn phase_name_roundtrip() {
    for name in &[
        PhaseName::PreScripts,
        PhaseName::Env,
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
    let phase = Phase {
        name: PhaseName::Packages,
        actions: vec![
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
    };

    let items = format_plan_items(&phase);
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
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["ripgrep".to_string()],
                origin: "local".to_string(),
            })],
        }],
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
    };

    assert_eq!(result.succeeded(), 1);
    assert_eq!(result.failed(), 1);
}

// --- Module integration tests ---

use crate::modules::{ResolvedFile, ResolvedModule, ResolvedPackage};

#[test]
fn plan_includes_module_phase() {
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

    assert_eq!(plan.phases.len(), 8);
    let module_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Modules)
        .unwrap();

    // Module phase should have at least 1 action (InstallPackages)
    assert!(!module_phase.actions.is_empty());

    // Check that actions are ModuleAction
    for action in &module_phase.actions {
        match action {
            Action::Module(ma) => {
                assert_eq!(ma.module_name, "nvim");
            }
            _ => panic!("expected Module action in Modules phase"),
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        .find(|p| p.name == PhaseName::Modules)
        .unwrap();
    assert_eq!(module_phase.actions.len(), 1);

    match &module_phase.actions[0] {
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
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        .find(|p| p.name == PhaseName::Modules)
        .unwrap();
    assert_eq!(module_phase.actions.len(), 2);

    for action in &module_phase.actions {
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
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        },
        ResolvedModule {
            name: "nvim".to_string(),
            packages: vec![ResolvedPackage {
                canonical_name: "neovim".to_string(),
                resolved_name: "neovim".to_string(),
                manager: "brew".to_string(),
                version: Some("0.10.2".to_string()),
                script: None,
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec!["node".to_string()],
            dir: PathBuf::from("."),
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

    let module_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Modules)
        .unwrap();
    // node packages + nvim packages = 2 actions
    assert_eq!(module_phase.actions.len(), 2);

    // First action should be for "node" (leaf dependency)
    match &module_phase.actions[0] {
        Action::Module(ma) => assert_eq!(ma.module_name, "node"),
        _ => panic!("expected Module action"),
    }
    // Second for "nvim"
    match &module_phase.actions[1] {
        Action::Module(ma) => assert_eq!(ma.module_name, "nvim"),
        _ => panic!("expected Module action"),
    }
}

#[test]
fn format_module_plan_items_packages() {
    let phase = Phase {
        name: PhaseName::Modules,
        actions: vec![Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: ModuleActionKind::InstallPackages {
                resolved: vec![
                    ResolvedPackage {
                        canonical_name: "neovim".to_string(),
                        resolved_name: "neovim".to_string(),
                        manager: "brew".to_string(),
                        version: Some("0.10.2".to_string()),
                        script: None,
                    },
                    ResolvedPackage {
                        canonical_name: "fd".to_string(),
                        resolved_name: "fd-find".to_string(),
                        manager: "apt".to_string(),
                        version: Some("8.7.0".to_string()),
                        script: None,
                    },
                ],
            },
        })],
    };

    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("[nvim]"));
    // Should show alias info for fd→fd-find
    assert!(items[0].contains("fd-find"));
}

#[test]
fn format_module_plan_items_files() {
    let phase = Phase {
        name: PhaseName::Modules,
        actions: vec![Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: ModuleActionKind::DeployFiles {
                files: vec![ResolvedFile {
                    source: PathBuf::from("/cache/nvim/config"),
                    target: PathBuf::from("/home/user/.config/nvim"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                }],
            },
        })],
    };

    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("[nvim]"));
    assert!(items[0].contains("deploy"));
    assert!(items[0].contains(".config/nvim"));
}

#[test]
fn format_module_plan_items_skip() {
    let phase = Phase {
        name: PhaseName::Modules,
        actions: vec![Action::Module(ModuleAction {
            module_name: "bad".to_string(),
            kind: ModuleActionKind::Skip {
                reason: "dependency not met".to_string(),
            },
        })],
    };

    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("[bad]"));
    assert!(items[0].contains("skip"));
    assert!(items[0].contains("dependency not met"));
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
            }],
        },
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

    // nvim/neovim should not appear as drift since it's installed
    let ok = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "nvim/neovim");
    assert!(ok.is_none()); // no drift entry for installed packages
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
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "nvim".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "neovim".to_string(),
                        resolved_name: "neovim".to_string(),
                        manager: "brew".to_string(),
                        version: Some("0.10.2".to_string()),
                        script: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let hash = plan.to_hash_string();
    assert!(hash.contains("nvim"));
    assert!(hash.contains("neovim"));
    assert!(hash.contains("brew"));
}

#[test]
fn verify_module_healthy_when_all_installed() {
    let state = test_state();
    let mut registry = ProviderRegistry::new();

    registry.package_managers.push(Box::new(
        MockPackageManager::new("brew").with_installed(&["neovim", "ripgrep"]),
    ));

    let resolved = make_empty_resolved();
    let printer = test_printer();

    let modules = vec![make_resolved_module("nvim")];
    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    // All packages installed → should get a single "healthy" result
    let healthy = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "nvim");
    assert!(healthy.is_some());
    assert!(healthy.unwrap().matches);
    assert_eq!(healthy.unwrap().expected, "healthy");

    // No drift entries
    let drifts: Vec<_> = results
        .iter()
        .filter(|r| r.resource_type == "module" && !r.matches)
        .collect();
    assert!(drifts.is_empty());
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
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    // Script packages should be skipped in verification, so module should be healthy
    let healthy = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "rustup");
    assert!(healthy.is_some());
    assert!(healthy.unwrap().matches);
    assert_eq!(healthy.unwrap().expected, "healthy");

    // No drift entries for script packages
    let drifts: Vec<_> = results
        .iter()
        .filter(|r| r.resource_type == "module" && !r.matches)
        .collect();
    assert!(drifts.is_empty());
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
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        .find(|p| p.name == PhaseName::Modules)
        .unwrap();
    assert_eq!(module_phase.actions.len(), 1);

    match &module_phase.actions[0] {
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
    let phase = Phase {
        name: PhaseName::Modules,
        actions: vec![Action::Module(ModuleAction {
            module_name: "rustup".to_string(),
            kind: ModuleActionKind::InstallPackages {
                resolved: vec![ResolvedPackage {
                    canonical_name: "rustup".to_string(),
                    resolved_name: "rustup".to_string(),
                    manager: "script".to_string(),
                    version: None,
                    script: Some("install-rustup.sh".into()),
                }],
            },
        })],
    };

    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("[rustup]"));
    assert!(items[0].contains("script"));
    assert!(items[0].contains("rustup"));
}

#[test]
fn empty_modules_produces_empty_phase() {
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

    let module_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Modules)
        .unwrap();
    assert!(module_phase.actions.is_empty());
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("conflict"), "expected conflict error: {err}");
}

#[test]
fn conflict_detection_two_profile_actions_same_target_different_content_errs() {
    // Covers plan.rs L122-129: two profile FileActions hitting the same
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
        },
        FileAction::Update {
            source: file_b,
            target,
            diff: String::new(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        },
    ];

    let err = Reconciler::detect_file_conflicts(&file_actions, &[])
        .expect_err("profile-vs-profile conflict must error");
    assert!(err.to_string().contains("conflict"));
}

#[test]
fn conflict_detection_two_profile_actions_same_target_identical_content_ok() {
    // The dedup branch of L121: same target, same content hash → no error.
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
        },
        FileAction::Update {
            source: file_b,
            target,
            diff: String::new(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
    let content = super::generate_env_file_content(&env, &[]);
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
    let content = super::generate_fish_env_content(&env, &[]);
    assert!(content.starts_with("# managed by cfgd"));
    assert!(content.contains("set -gx EDITOR 'nvim'"));
    assert!(content.contains("set -gx PATH '/usr/local/bin' '/home/user/.cargo/bin' '$PATH'"));
}

#[test]
fn plan_env_empty_when_no_env() {
    let tmp = tempfile::tempdir().unwrap();
    let (actions, _warnings) = Reconciler::plan_env_with_home(&[], &[], &[], &[], tmp.path());
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
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];
    // plan_env merges and generates actions — the merged env should have EDITOR=nvim
    let tmp = tempfile::tempdir().unwrap();
    let (actions, _warnings) =
        Reconciler::plan_env_with_home(&profile_env, &[], &modules, &[], tmp.path());
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
    let expected = super::generate_env_file_content(&env, &[]);
    std::fs::write(&env_path, &expected).unwrap();

    // plan_env checks the real ~/.cfgd.env path, not our temp file,
    // so it will still generate actions. This test validates the content generation.
    assert!(expected.contains("export EDITOR=\"nvim\""));
    assert!(expected.contains("# managed by cfgd"));
}

#[test]
fn phase_name_env_roundtrip() {
    assert_eq!(PhaseName::Env.as_str(), "env");
    assert_eq!(PhaseName::Env.display_name(), "Environment");
    assert_eq!("env".parse::<PhaseName>().unwrap(), PhaseName::Env);
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
    let content = super::generate_env_file_content(&env, &aliases);
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
    let content = super::generate_fish_env_content(&env, &aliases);
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
    let (actions, _warnings) = Reconciler::plan_env_with_home(&[], &aliases, &[], &[], tmp.path());
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
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];
    let tmp = tempfile::tempdir().unwrap();
    let (actions, _warnings) =
        Reconciler::plan_env_with_home(&[], &profile_aliases, &modules, &[], tmp.path());
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
    let content = super::generate_env_file_content(&[], &aliases);
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
        &actions[0]
    );
    assert!(
        matches!(&actions[1], Action::Secret(SecretAction::ResolveEnv { .. })),
        "Second action should be ResolveEnv, got {:?}",
        &actions[1]
    );
}

#[test]
fn plan_env_with_secret_envs_includes_them() {
    let secret_envs = vec![
        ("GITHUB_TOKEN".to_string(), "ghp_abc123".to_string()),
        ("NPM_TOKEN".to_string(), "npm_xyz789".to_string()),
    ];
    let tmp = tempfile::tempdir().unwrap();
    let (actions, _warnings) =
        Reconciler::plan_env_with_home(&[], &[], &[], &secret_envs, tmp.path());
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
    let (actions, _warnings) =
        Reconciler::plan_env_with_home(&regular_env, &[], &[], &secret_envs, tmp.path());

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
        "export EDITOR=\"vim\"\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
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
        "export EDITOR=\"nvim\"\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
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
        "alias vim=\"vi\"\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
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
        "[ -f ~/.cfgd.env ] && source ~/.cfgd.env\nexport EDITOR=\"vim\"\n",
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
    let content = super::generate_powershell_env_content(&env, &[]);
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
    let content = super::generate_powershell_env_content(&[], &aliases);
    assert!(content.contains("Set-Alias -Name g -Value git"));
    assert!(content.contains("function ll {"));
    assert!(content.contains("Get-ChildItem -Force @args"));
}

#[test]
fn generate_powershell_env_escapes_quotes() {
    let env = vec![crate::config::EnvVar {
        name: "GREETING".into(),
        value: r#"say "hello""#.into(),
    }];
    let content = super::generate_powershell_env_content(&env, &[]);
    // No $env: reference, so single-quoted (PS single quotes don't need escaping except ')
    assert!(content.contains("$env:GREETING = 'say \"hello\"'"));
}

#[test]
fn generate_powershell_env_empty() {
    let content = super::generate_powershell_env_content(&[], &[]);
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
}

impl TrackingPackageManager {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            installed: std::sync::Mutex::new(HashSet::new()),
            install_calls: std::sync::Mutex::new(Vec::new()),
            uninstall_calls: std::sync::Mutex::new(Vec::new()),
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
        }
    }
}

impl PackageManager for TrackingPackageManager {
    fn name(&self) -> &str {
        &self.name
    }
    fn is_available(&self) -> bool {
        true
    }
    fn can_bootstrap(&self) -> bool {
        false
    }
    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self) -> Result<HashSet<String>> {
        Ok(self.installed.lock().unwrap().clone())
    }
    fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
        self.install_calls.lock().unwrap().push(packages.to_vec());
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.insert(p.clone());
        }
        Ok(())
    }
    fn uninstall(&self, packages: &[String], _printer: &Printer) -> Result<()> {
        self.uninstall_calls.lock().unwrap().push(packages.to_vec());
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.remove(p);
        }
        Ok(())
    }
    fn update(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
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
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);
    assert!(result.action_results[0].error.is_none());
    assert!(result.action_results[0].description.contains("ripgrep"));

    // Verify install was actually called on the tracking mock
    let pm = registry.package_managers[0].as_ref();
    let installed = pm.installed_packages().unwrap();
    assert!(installed.contains("ripgrep"));
    assert!(installed.contains("fd"));
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
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);

    let pm = registry.package_managers[0].as_ref();
    let installed = pm.installed_packages().unwrap();
    assert!(!installed.contains("ripgrep"));
    assert!(installed.contains("fd"));
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
    let content = super::generate_env_file_content(&env, &[]);

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content: content.clone(),
    };

    let printer = test_printer();
    let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

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
    let content = super::generate_env_file_content(&env, &[]);

    // Pre-write identical content
    std::fs::write(&env_path, &content).unwrap();

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content,
    };

    let printer = test_printer();
    let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

    // Should report skipped
    assert!(desc.contains("skipped"), "Expected skip: {}", desc);
}

#[test]
fn apply_env_inject_source_line_creates_file() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
    };

    let printer = test_printer();
    let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

    let written = std::fs::read_to_string(&rc_path).unwrap();
    assert!(written.contains("source ~/.cfgd.env"));
    assert!(desc.starts_with("env:inject:"));
}

#[test]
fn apply_env_inject_skips_when_already_present() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");

    // Pre-write content that already mentions cfgd.env
    std::fs::write(
        &rc_path,
        "# existing config\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
    )
    .unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
    };

    let printer = test_printer();
    let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

    assert!(desc.contains("skipped"), "Expected skip: {}", desc);
}

#[test]
fn apply_env_inject_appends_to_existing_content() {
    let dir = tempfile::tempdir().unwrap();
    let rc_path = dir.path().join(".bashrc");

    std::fs::write(&rc_path, "# my config\nexport FOO=bar").unwrap();

    let action = EnvAction::InjectSourceLine {
        rc_path: rc_path.clone(),
        line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
    };

    let printer = test_printer();
    Reconciler::apply_env_action(&action, &printer).unwrap();

    let written = std::fs::read_to_string(&rc_path).unwrap();
    assert!(written.starts_with("# my config\n"));
    assert!(written.contains("export FOO=bar"));
    assert!(written.contains("source ~/.cfgd.env"));
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
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.succeeded(), 1);
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
        )
        .unwrap();

    // Verify the summary JSON in the state store
    let last = state.last_apply().unwrap().unwrap();
    let summary = last.summary.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(parsed["total"], 1);
    assert_eq!(parsed["succeeded"], 1);
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
            Some(&PhaseName::Env),
            &[],
            ReconcileContext::Apply,
            false,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // No actions executed because Env phase is empty and Packages phase was filtered out
    assert_eq!(result.action_results.len(), 0);
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
            Some(&PhaseName::Packages),
            &[],
            ReconcileContext::Apply,
            false,
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
            Some(&PhaseName::Files),
            &[],
            ReconcileContext::Apply,
            false,
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
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert_eq!(result.action_results.len(), 2);
    assert_eq!(result.succeeded(), 2);
    assert_eq!(result.failed(), 0);

    // Verify both managers had their install called
    let brew = registry.package_managers[0].as_ref();
    assert!(brew.installed_packages().unwrap().contains("jq"));
    let cargo = registry.package_managers[1].as_ref();
    assert!(cargo.installed_packages().unwrap().contains("bat"));
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
    let content = super::generate_env_file_content(&env, &aliases);

    let action = EnvAction::WriteEnvFile {
        path: env_path.clone(),
        content: content.clone(),
    };

    let printer = test_printer();
    Reconciler::apply_env_action(&action, &printer).unwrap();

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
        run: "echo test".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: Some(true),
    };
    // Should be true even for pre-apply (which defaults to false)
    assert!(super::effective_continue_on_error(
        &entry,
        &ScriptPhase::PreApply
    ));

    let entry_false = ScriptEntry::Full {
        run: "echo test".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: Some(false),
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
        run: "echo test".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
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
    assert_eq!(pre_phase.actions.len(), 1);
    match &pre_phase.actions[0] {
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
    assert_eq!(post_phase.actions.len(), 1);
    match &post_phase.actions[0] {
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
        run: "scripts/check.sh".to_string(),
        timeout: Some("10s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(true),
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
    assert_eq!(pre_phase.actions.len(), 1);
    match &pre_phase.actions[0] {
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
    let env = super::build_script_env(
        std::path::Path::new("/home/user/.config/cfgd"),
        "default",
        ReconcileContext::Apply,
        &ScriptPhase::PreApply,
        None,
        None,
    );
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
    let env = super::build_script_env(
        std::path::Path::new("/config"),
        "work",
        ReconcileContext::Reconcile,
        &ScriptPhase::PostApply,
        Some("nvim"),
        Some(std::path::Path::new("/modules/nvim")),
    );
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
        &[],
        std::time::Duration::from_secs(10),
        &printer,
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
        &[],
        std::time::Duration::from_secs(10),
        &printer,
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
        run: "echo fast".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let (_, _, output) = super::execute_script(
        &entry,
        dir.path(),
        &[],
        std::time::Duration::from_secs(300),
        &printer,
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
        &env,
        std::time::Duration::from_secs(10),
        &printer,
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
        &[],
        std::time::Duration::from_secs(10),
        &printer,
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
        &[],
        std::time::Duration::from_secs(10),
        &printer,
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
        run: "echo started; sleep 60".to_string(),
        timeout: Some("30s".to_string()),
        idle_timeout: Some("1s".to_string()),
        continue_on_error: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let result = super::execute_script(
        &entry,
        dir.path(),
        &[],
        std::time::Duration::from_secs(30),
        &printer,
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
    state.journal_complete(jid1, None, None).unwrap();
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
    state.journal_complete(jid2, None, None).unwrap();
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
    state.journal_complete(journal_id, None, None).unwrap();

    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let rollback_result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

    assert_eq!(rollback_result.files_restored, 0);
    assert_eq!(rollback_result.files_removed, 0);
    assert_eq!(rollback_result.non_file_actions.len(), 1);
    assert!(rollback_result.non_file_actions[0].contains("ripgrep"));
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
    fn can_bootstrap(&self) -> bool {
        false
    }
    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
        Err(crate::errors::PackageError::InstallFailed {
            manager: self.name.clone(),
            message: "simulated install failure".to_string(),
        }
        .into())
    }
    fn uninstall(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
        Ok(())
    }
    fn update(&self, _printer: &Printer) -> Result<()> {
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
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Partial);
    assert_eq!(result.succeeded(), 1);
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
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Failed);
    assert_eq!(result.succeeded(), 0);
    assert_eq!(result.failed(), 1);
    assert!(result.action_results[0].error.is_some());

    let last = state.last_apply().unwrap().unwrap();
    assert_eq!(last.status, ApplyStatus::Failed);
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
        run: "exit 42".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(true),
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
        )
        .unwrap();

    // Package install succeeded, post-script failed but continued
    assert_eq!(result.status, ApplyStatus::Partial);
    assert_eq!(result.succeeded(), 1); // package install
    assert_eq!(result.failed(), 1); // failed post-script

    // Verify the failed action is the script
    let failed = result.action_results.iter().find(|r| !r.success).unwrap();
    assert!(
        failed.description.contains("exit 42"),
        "failed action should be the script: {}",
        failed.description
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
        run: "exit 1".to_string(),
        timeout: Some("5s".to_string()),
        idle_timeout: None,
        continue_on_error: Some(false),
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
    );

    // Pre-script failure with continueOnError=false should return an error
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("pre-script failed"),
        "should mention pre-script failure: {err}"
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
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    // No changes occurred, so onChange should NOT have run
    assert!(
        !marker.exists(),
        "onChange marker should NOT exist when no changes occurred"
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
    ];
    for (input, expected_type, expected_id) in cases {
        let (rtype, rid) = super::parse_resource_from_description(input);
        assert_eq!(rtype, *expected_type, "wrong type for {input:?}");
        assert_eq!(rid, *expected_id, "wrong id for {input:?}");
    }
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
    });
    assert_eq!(super::action_target_path(&action), Some(target));
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
    });
    assert_eq!(super::action_target_path(&action), Some(target));
}

#[test]
fn action_target_path_file_delete() {
    let target = PathBuf::from("/home/user/.old");
    let action = Action::File(FileAction::Delete {
        target: target.clone(),
        origin: "local".into(),
    });
    assert_eq!(super::action_target_path(&action), Some(target));
}

#[test]
fn action_target_path_env_write() {
    let path = PathBuf::from("/home/user/.cfgd.env");
    let action = Action::Env(EnvAction::WriteEnvFile {
        path: path.clone(),
        content: "test".into(),
    });
    assert_eq!(super::action_target_path(&action), Some(path));
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
    });
    assert!(super::action_target_path(&action).is_none());
}

#[test]
fn action_target_path_env_inject_returns_none() {
    let action = Action::Env(EnvAction::InjectSourceLine {
        rc_path: PathBuf::from("/home/user/.bashrc"),
        line: "source ~/.cfgd.env".into(),
    });
    assert!(super::action_target_path(&action).is_none());
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
        line: "source ~/.cfgd.env".into(),
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
                },
                crate::modules::ResolvedFile {
                    source: PathBuf::from("/src/b"),
                    target: PathBuf::from("/dst/b"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                },
            ],
        },
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
            Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["jq".into()],
                    origin: "local".into(),
                })],
            },
            Phase {
                name: PhaseName::Files,
                actions: vec![Action::File(FileAction::Create {
                    source: PathBuf::from("/src"),
                    target: PathBuf::from("/dst"),
                    origin: "local".into(),
                    strategy: crate::config::FileStrategy::Copy,
                    source_hash: None,
                })],
            },
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
            Phase {
                name: PhaseName::Packages,
                actions: vec![
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
            },
            Phase {
                name: PhaseName::Files,
                actions: vec![Action::File(FileAction::Skip {
                    target: PathBuf::from("/x"),
                    reason: "n/a".into(),
                    origin: "local".into(),
                })],
            },
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
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    // Reconcile context should use pre/post reconcile scripts, not apply scripts
    let actions = reconciler.plan_modules(&modules, ReconcileContext::Reconcile);
    assert_eq!(actions.len(), 2); // pre-reconcile + post-reconcile

    // First action should be pre-reconcile
    match &actions[0] {
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
    match &actions[1] {
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
    let phase = Phase {
        name: PhaseName::System,
        actions: vec![
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
            }),
        ],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 2);
    assert!(items[0].contains("set sysctl.net.ipv4.ip_forward"));
    assert!(items[0].contains("0 \u{2192} 1"));
    assert!(items[1].contains("skip custom: no configurator"));
}

#[test]
fn format_plan_items_secret_actions() {
    let phase = Phase {
        name: PhaseName::Secrets,
        actions: vec![
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
    };
    let items = format_plan_items(&phase);
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
    let phase = Phase {
        name: PhaseName::Env,
        actions: vec![
            Action::Env(EnvAction::WriteEnvFile {
                path: PathBuf::from("/home/user/.cfgd.env"),
                content: "content".into(),
            }),
            Action::Env(EnvAction::InjectSourceLine {
                rc_path: PathBuf::from("/home/user/.bashrc"),
                line: "source ~/.cfgd.env".into(),
            }),
        ],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 2);
    assert!(items[0].contains("write"));
    assert!(items[0].contains(".cfgd.env"));
    assert!(items[1].contains("inject source line"));
    assert!(items[1].contains(".bashrc"));
}

#[test]
fn format_plan_items_script_action_with_provenance() {
    let phase = Phase {
        name: PhaseName::PreScripts,
        actions: vec![Action::Script(ScriptAction::Run {
            entry: ScriptEntry::Simple("setup.sh".into()),
            phase: ScriptPhase::PreApply,
            origin: "corp-source".into(),
        })],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("run preApply script: setup.sh"));
    assert!(items[0].contains("<- corp-source"));
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
        })
        .collect();
    let action = ModuleAction {
        module_name: "big".into(),
        kind: ModuleActionKind::DeployFiles { files },
    };
    let item = super::format_module_action_item(&action);
    assert!(item.contains("[big]"));
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
    let content = super::generate_powershell_env_content(&env, &[]);
    // Single quotes in values are doubled in PS
    assert!(content.contains("$env:MSG = 'it''s a test'"));
}

#[test]
fn generate_fish_env_escapes_single_quotes() {
    let env = vec![crate::config::EnvVar {
        name: "MSG".into(),
        value: "it's a test".into(),
    }];
    let content = super::generate_fish_env_content(&env, &[]);
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
    fn can_bootstrap(&self) -> bool {
        true
    }
    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        *self.bootstrapped.lock().unwrap() = true;
        Ok(())
    }
    fn installed_packages(&self) -> Result<HashSet<String>> {
        Ok(self.installed.lock().unwrap().clone())
    }
    fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.insert(p.clone());
        }
        Ok(())
    }
    fn uninstall(&self, packages: &[String], _printer: &Printer) -> Result<()> {
        let mut installed = self.installed.lock().unwrap();
        for p in packages {
            installed.remove(p);
        }
        Ok(())
    }
    fn update(&self, _printer: &Printer) -> Result<()> {
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
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Bootstrap {
                manager: "snap".to_string(),
                method: "auto".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Packages),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Bootstrap {
                manager: "nonexistent".to_string(),
                method: "auto".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Packages),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Install {
                manager: "nonexistent".to_string(),
                packages: vec!["foo".to_string()],
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Packages),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Uninstall {
                manager: "nonexistent".to_string(),
                packages: vec!["foo".to_string()],
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Packages),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Secrets,
            actions: vec![Action::Secret(SecretAction::Decrypt {
                source: source.clone(),
                target: target.clone(),
                backend: "test-sops".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Secrets),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Secrets,
            actions: vec![Action::Secret(SecretAction::Decrypt {
                source: source.clone(),
                target: target.clone(),
                backend: "sops".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Secrets),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Secrets,
            actions: vec![Action::Secret(SecretAction::Resolve {
                provider: "vault".to_string(),
                reference: "secret/data/app#key".to_string(),
                target: target.clone(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Secrets),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Secrets,
            actions: vec![Action::Secret(SecretAction::Resolve {
                provider: "vault".to_string(),
                reference: "secret/data/app#key".to_string(),
                target: target.clone(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Secrets),
            &[],
            ReconcileContext::Apply,
            false,
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
    // never touch the user's home. See task #37 for the broader audit.
    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry.secret_providers.push(Box::new(
        MockSecretProvider::new("vault").with_resolve_result("env-secret-value"),
    ));
    let reconciler = Reconciler::new(&registry, &state);
    let printer = test_printer();
    let tmp = tempfile::tempdir().unwrap();

    let mut collector: Vec<(String, String)> = Vec::new();
    let action = SecretAction::ResolveEnv {
        provider: "vault".to_string(),
        reference: "secret/data/gh#token".to_string(),
        envs: vec!["GH_TOKEN".to_string(), "GITHUB_TOKEN".to_string()],
        origin: "local".to_string(),
    };

    let desc = reconciler
        .apply_secret_action(&action, tmp.path(), &printer, &mut collector)
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
fn apply_secret_resolve_env_unknown_provider_errors() {
    let state = test_state();
    let registry = ProviderRegistry::new(); // no providers

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Secrets,
            actions: vec![Action::Secret(SecretAction::ResolveEnv {
                provider: "vault".to_string(),
                reference: "secret/data/gh#token".to_string(),
                envs: vec!["GH_TOKEN".to_string()],
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Secrets),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Secrets,
            actions: vec![Action::Secret(SecretAction::Skip {
                source: "vault://test".to_string(),
                reason: "not available".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Secrets),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Files,
            actions: vec![Action::File(FileAction::Delete {
                target: target.clone(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Files),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Files,
            actions: vec![Action::File(FileAction::SetPermissions {
                target: target.clone(),
                mode: 0o755,
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Files),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Files,
            actions: vec![Action::File(FileAction::Skip {
                target: PathBuf::from("/nonexistent"),
                reason: "unchanged".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Files),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Files,
            actions: vec![Action::File(FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: "diff output".to_string(),
                origin: "local".to_string(),
                strategy: crate::config::FileStrategy::Copy,
                source_hash: None,
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Files),
            &[],
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::System,
            actions: vec![Action::System(SystemAction::SetValue {
                configurator: "sysctl".to_string(),
                key: "net.ipv4.ip_forward".to_string(),
                desired: "1".to_string(),
                current: "0".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::System),
            &[],
            ReconcileContext::Apply,
            false,
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
fn apply_system_skip_logs_warning() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::System,
            actions: vec![Action::System(SystemAction::Skip {
                configurator: "customThing".to_string(),
                reason: "no configurator registered".to_string(),
                origin: "local".to_string(),
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::System),
            &[],
            ReconcileContext::Apply,
            false,
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

#[test]
fn plan_system_emits_set_value_actions_per_drift() {
    // Covers plan.rs L180-189: when a configurator returns drift entries,
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
            ..
        }) => {
            assert_eq!(configurator, "unknownConf");
            assert!(reason.contains("no configurator registered"));
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
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "nvim".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "neovim".to_string(),
                        resolved_name: "neovim".to_string(),
                        manager: "brew".to_string(),
                        version: None,
                        script: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
    let installed = registry.package_managers[0].installed_packages().unwrap();
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "linkmod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "broken".to_string(),
                kind: ModuleActionKind::Skip {
                    reason: "dependency not met".to_string(),
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Modules),
            &[],
            ReconcileContext::Apply,
            false,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);
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
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "tools".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "jq".to_string(),
                        resolved_name: "jq".to_string(),
                        manager: "brew".to_string(),
                        version: None,
                        script: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);

    // Manager should have been bootstrapped and package installed
    assert!(registry.package_managers[0].is_available());
    assert!(
        registry.package_managers[0]
            .installed_packages()
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
    state.journal_complete(jid1, None, None).unwrap();

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
    state.journal_complete(jid2, None, None).unwrap();

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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
    // Should produce a Skip action because encryption=Always + symlink is incompatible
    assert_eq!(actions.len(), 1);
    match &actions[0] {
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
    // Should produce DeployFiles (encryption=Always + copy is OK, and file has sops marker)
    assert_eq!(actions.len(), 1);
    match &actions[0] {
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
    // crashing. Covers plan.rs L474-486.
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
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
            },
            ResolvedFile {
                source: b.clone(),
                target: PathBuf::from("/home/user/.b"),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: None,
            },
        ],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
    // After the failing encryption check, planner does NOT emit DeployFiles —
    // single Skip is the only module action emitted.
    let kinds: Vec<&ModuleActionKind> = actions
        .iter()
        .filter_map(|a| match a {
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
    // Should skip because file requires encryption but isn't encrypted
    assert_eq!(actions.len(), 1);
    match &actions[0] {
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "testmod".to_string(),
                kind: ModuleActionKind::RunScript {
                    script: ScriptEntry::Simple(format!("touch {}", marker.display())),
                    phase: ScriptPhase::PostApply,
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
    let content = super::generate_fish_env_content(&env, &aliases);
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
    let content = super::generate_powershell_env_content(&env, &[]);
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
    let content = super::generate_powershell_env_content(&[], &aliases);
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
    let content = super::generate_fish_env_content(&env, &[]);
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
        let env = super::build_script_env(
            std::path::Path::new("/etc/cfgd"),
            "default",
            ReconcileContext::Apply,
            phase,
            None,
            None,
        );
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
    let env = super::build_script_env(
        std::path::Path::new("/cfg"),
        "laptop",
        ReconcileContext::Apply,
        &ScriptPhase::PreApply,
        None,
        None,
    );
    let map: HashMap<String, String> = env.into_iter().collect();
    assert!(!map.contains_key("CFGD_DRY_RUN"));
}

#[test]
fn build_script_env_reconcile_context() {
    let env = super::build_script_env(
        std::path::Path::new("/cfg"),
        "server",
        ReconcileContext::Reconcile,
        &ScriptPhase::PostReconcile,
        None,
        None,
    );
    let map: HashMap<String, String> = env.into_iter().collect();
    assert_eq!(map.get("CFGD_CONTEXT").unwrap(), "reconcile");
    assert_eq!(map.get("CFGD_PHASE").unwrap(), "postReconcile");
    assert_eq!(map.get("CFGD_PROFILE").unwrap(), "server");
}

#[test]
fn build_script_env_module_name_without_dir() {
    // module_name provided but module_dir is None
    let env = super::build_script_env(
        std::path::Path::new("/cfg"),
        "default",
        ReconcileContext::Apply,
        &ScriptPhase::PreApply,
        Some("zsh"),
        None,
    );
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
    let env = super::build_script_env(
        std::path::Path::new("/x"),
        "p",
        ReconcileContext::Apply,
        &ScriptPhase::PreApply,
        None,
        None,
    );
    assert_eq!(env.len(), 4, "base env should have 4 entries");

    // With both module name and dir, should have 6
    let env_with_module = super::build_script_env(
        std::path::Path::new("/x"),
        "p",
        ReconcileContext::Apply,
        &ScriptPhase::PreApply,
        Some("m"),
        Some(std::path::Path::new("/modules/m")),
    );
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

#[test]
fn verify_file_target_exists() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let printer = test_printer();
    let tmp = tempfile::tempdir().unwrap();

    // Create a file that exists
    let target_path = tmp.path().join("existing.conf");
    std::fs::write(&target_path, "content").unwrap();

    let mut resolved = make_empty_resolved();
    resolved.merged.files.managed.push(ManagedFileSpec {
        source: "source.conf".to_string(),
        target: target_path.clone(),
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    });

    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();
    let file_result = results
        .iter()
        .find(|r| r.resource_type == "file")
        .expect("should have a file verify result");
    assert!(file_result.matches, "existing file should match");
    assert_eq!(file_result.expected, "present");
    assert_eq!(file_result.actual, "present");
}

#[test]
fn verify_file_target_missing() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let printer = test_printer();

    let mut resolved = make_empty_resolved();
    resolved.merged.files.managed.push(ManagedFileSpec {
        source: "source.conf".to_string(),
        target: PathBuf::from("/tmp/cfgd-test-nonexistent-file-39485738"),
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    });

    let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();
    let file_result = results
        .iter()
        .find(|r| r.resource_type == "file")
        .expect("should have a file verify result");
    assert!(!file_result.matches, "missing file should not match");
    assert_eq!(file_result.expected, "present");
    assert_eq!(file_result.actual, "missing");
}

#[test]
fn verify_module_file_target_missing_causes_drift() {
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    // Should have drift for the missing file
    let drift = results
        .iter()
        .find(|r| r.resource_type == "module" && !r.matches);
    assert!(
        drift.is_some(),
        "missing module file target should cause drift"
    );
    let d = drift.unwrap();
    assert_eq!(d.expected, "present");
    assert_eq!(d.actual, "missing");
    assert!(d.resource_id.contains("test-mod"));
}

#[test]
fn verify_module_file_target_exists_no_drift() {
    let state = test_state();
    let registry = ProviderRegistry::new();
    let printer = test_printer();
    let resolved = make_empty_resolved();
    let tmp = tempfile::tempdir().unwrap();

    let target_path = tmp.path().join("module-config");
    std::fs::write(&target_path, "content").unwrap();

    let modules = vec![ResolvedModule {
        name: "files-mod".to_string(),
        packages: vec![],
        files: vec![ResolvedFile {
            source: PathBuf::from("/src/config"),
            target: target_path,
            is_git_source: false,
            strategy: None,
            encryption: None,
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

    // Module should be healthy
    let healthy = results
        .iter()
        .find(|r| r.resource_type == "module" && r.resource_id == "files-mod");
    assert!(healthy.is_some(), "module should have a healthy result");
    assert!(healthy.unwrap().matches);
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
        line: "source ~/.cfgd.env".to_string(),
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
                },
                ResolvedPackage {
                    canonical_name: "ripgrep".to_string(),
                    resolved_name: "ripgrep".to_string(),
                    manager: "brew".to_string(),
                    version: None,
                    script: None,
                },
            ],
        },
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
                },
                ResolvedFile {
                    source: PathBuf::from("/src/plugins.lua"),
                    target: PathBuf::from("/home/.config/nvim/plugins.lua"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                },
            ],
        },
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
        ("env", PhaseName::Env, "Environment"),
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
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
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
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "hardmod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Hardlink),
                        encryption: None,
                    }],
                },
            })],
        }],
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "copymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "dirmod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_dir.clone(),
                        target: target_dir.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
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
        }],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "overmod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "changemod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: source_file.clone(),
                        target: target_file.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
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

    // Apply 2: creates new-file.txt (file didn't exist before)
    let apply_id_2 = state
        .record_apply("default", "hash-2", ApplyStatus::InProgress, None)
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
    state.journal_complete(j_id, None, None).unwrap();
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
    state.journal_complete(j_id, None, None).unwrap();
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
    state.journal_complete(j_id, None, None).unwrap();
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
    state.journal_complete(j1, None, None).unwrap();
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
    state.journal_complete(j2, None, None).unwrap();
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
            .contains(&"brew:ripgrep".to_string()),
        "should list package action for manual review: {:?}",
        result.non_file_actions
    );
    assert!(
        result
            .non_file_actions
            .contains(&"script:post:setup.sh".to_string()),
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
        fn apply(&self, _: &serde_yaml::Value, _: &Printer) -> crate::errors::Result<()> {
            Ok(())
        }
    }

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(DriftingConfigurator));

    let mut system = HashMap::new();
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
        fn apply(&self, _: &serde_yaml::Value, _: &Printer) -> crate::errors::Result<()> {
            Ok(())
        }
    }

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .system_configurators
        .push(Box::new(HealthyConfigurator));

    let mut system = HashMap::new();
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

// `Reconciler::apply` streams per-action status via `status_simple` (warn/fail
// lines with `[N/M]` prefixes). Callers that want a buffered summary (e.g.
// `cmd_apply`'s `ApplyOutput`) emit a `Doc` on the same `Printer` right after.
// The renderer's blank-line accounting must produce exactly one blank line
// between the last streaming line and the buffered Doc's first visible line —
// zero blanks would let the spinner's tail bleed into the summary; two would
// leave a visual gap.

mod bridge {
    use super::super::Reconciler;
    use super::*;
    use crate::output::test_capture::{assert_snapshot_at, strip_ansi};
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
    fn run_minimal_bridge() -> String {
        let (printer, cap) = Printer::for_test_doc();
        printer.status_simple(Role::Ok, "[1/1] Wrote /etc/hosts");
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
            run: "exit 42".to_string(),
            timeout: Some("5s".to_string()),
            idle_timeout: None,
            continue_on_error: Some(true),
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

        strip_ansi(&cap.human())
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
    let phase = Phase {
        name: PhaseName::Files,
        actions: vec![Action::File(FileAction::Skip {
            target: PathBuf::from("/home/user/.config/skipped"),
            reason: "unchanged".into(),
            origin: "corp".into(),
        })],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("skip"), "got: {}", items[0]);
    assert!(items[0].contains("unchanged"), "got: {}", items[0]);
    assert!(items[0].contains("<- corp"), "got: {}", items[0]);
}

#[test]
fn format_plan_items_file_set_permissions() {
    let phase = Phase {
        name: PhaseName::Files,
        actions: vec![Action::File(FileAction::SetPermissions {
            target: PathBuf::from("/home/user/.ssh/id_rsa"),
            mode: 0o600,
            origin: "local".into(),
        })],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("chmod"), "got: {}", items[0]);
    assert!(items[0].contains("0o600"), "got: {}", items[0]);
    assert!(items[0].contains("id_rsa"), "got: {}", items[0]);
}

#[test]
fn format_plan_items_package_bootstrap() {
    let phase = Phase {
        name: PhaseName::Packages,
        actions: vec![Action::Package(PackageAction::Bootstrap {
            manager: "brew".into(),
            method: "curl | bash".into(),
            origin: "corp".into(),
        })],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("bootstrap brew"), "got: {}", items[0]);
    assert!(items[0].contains("curl | bash"), "got: {}", items[0]);
    assert!(items[0].contains("<- corp"), "got: {}", items[0]);
}

#[test]
fn format_plan_items_package_uninstall() {
    let phase = Phase {
        name: PhaseName::Packages,
        actions: vec![Action::Package(PackageAction::Uninstall {
            manager: "apt".into(),
            packages: vec!["vim".into(), "nano".into()],
            origin: "local".into(),
        })],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("uninstall"), "got: {}", items[0]);
    assert!(items[0].contains("vim"), "got: {}", items[0]);
    assert!(items[0].contains("nano"), "got: {}", items[0]);
}

#[test]
fn format_module_action_item_run_script() {
    let phase = Phase {
        name: PhaseName::Modules,
        actions: vec![Action::Module(ModuleAction {
            module_name: "nvim".into(),
            kind: ModuleActionKind::RunScript {
                script: ScriptEntry::Simple("make install".into()),
                phase: ScriptPhase::PostApply,
            },
        })],
    };
    let items = format_plan_items(&phase);
    assert_eq!(items.len(), 1);
    assert!(items[0].contains("[nvim]"), "got: {}", items[0]);
    assert!(items[0].contains("postApply"), "got: {}", items[0]);
    assert!(items[0].contains("make install"), "got: {}", items[0]);
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
        })
        .collect();
    let phase = Phase {
        name: PhaseName::Modules,
        actions: vec![Action::Module(ModuleAction {
            module_name: "big".into(),
            kind: ModuleActionKind::DeployFiles { files },
        })],
    };
    let items = format_plan_items(&phase);
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
            }],
        },
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
    };
    let cloned = action.clone_action();
    match cloned {
        FileAction::Create {
            source,
            target,
            origin,
            strategy,
            source_hash,
        } => {
            assert_eq!(source, PathBuf::from("/src/file"));
            assert_eq!(target, PathBuf::from("/dst/file"));
            assert_eq!(origin, "remote");
            assert_eq!(strategy, crate::config::FileStrategy::Symlink);
            assert_eq!(source_hash.as_deref(), Some("abc123"));
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
        } => {
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
    };
    let profile = make_empty_resolved();
    super::file_action::apply_file_action_direct(&action, dir.path(), &profile.merged).unwrap();
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "hello");
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
    };
    let profile = make_empty_resolved();
    super::file_action::apply_file_action_direct(&action, dir.path(), &profile.merged).unwrap();
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
    };
    let profile = make_empty_resolved();
    super::file_action::apply_file_action_direct(&action, dir.path(), &profile.merged).unwrap();
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
    let profile = make_empty_resolved();
    super::file_action::apply_file_action_direct(&action, dir.path(), &profile.merged).unwrap();
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
    let profile = make_empty_resolved();
    super::file_action::apply_file_action_direct(&action, dir.path(), &profile.merged).unwrap();
}

#[test]
fn apply_file_action_direct_skip_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let action = FileAction::Skip {
        target: dir.path().join("whatever.txt"),
        reason: "unchanged".into(),
        origin: "local".into(),
    };
    let profile = make_empty_resolved();
    super::file_action::apply_file_action_direct(&action, dir.path(), &profile.merged).unwrap();
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
    };
    let profile = make_empty_resolved();
    super::file_action::apply_file_action_direct(&action, dir.path(), &profile.merged).unwrap();
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "v2");
}

// -----------------------------------------------------------------------
// apply_module_action: additional uncovered branches
// (script-based package install, bootstrap-needs-PATH-write, manager-missing,
// DeployFiles with no parent, RunScript with no module dir)
// -----------------------------------------------------------------------

/// A package manager that reports unavailable, can_bootstrap=true,
/// and emits path_dirs after bootstrap so the .cfgd.env write branch fires.
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
    fn can_bootstrap(&self) -> bool {
        true
    }
    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        *self.bootstrap_called.lock().unwrap() = true;
        *self.available.lock().unwrap() = true;
        Ok(())
    }
    fn installed_packages(&self) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
        self.install_calls.lock().unwrap().push(packages.to_vec());
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
        Ok(())
    }
    fn update(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn path_dirs(&self) -> Vec<String> {
        self.path_dirs_after.clone()
    }
}

#[test]
#[serial_test::serial]
fn apply_module_install_packages_bootstraps_unavailable_manager_and_writes_env() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(BootstrappingPackageManager::new(
            "brew",
            &["/opt/homebrew/bin", "/opt/homebrew/sbin"],
        )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "ripgrep".to_string(),
            resolved_name: "ripgrep".to_string(),
            manager: "brew".to_string(),
            version: None,
            script: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "tools".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "ripgrep".to_string(),
                        resolved_name: "ripgrep".to_string(),
                        manager: "brew".to_string(),
                        version: None,
                        script: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);
    assert!(result.action_results[0].success);

    // Bootstrap must have been called and .cfgd.env populated with the new path dirs.
    let env_path = tmp_home.path().join(".cfgd.env");
    assert!(
        env_path.exists(),
        ".cfgd.env must be created after bootstrap with path_dirs"
    );
    let contents = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        contents.contains("/opt/homebrew/bin"),
        "bootstrap path must be in .cfgd.env: {contents}"
    );
}

#[test]
#[serial_test::serial]
fn apply_module_install_packages_with_existing_env_appends_new_dirs() {
    use crate::with_test_home_guard;

    let tmp_home = tempfile::tempdir().unwrap();
    // Pre-create .cfgd.env with one path entry already present.
    std::fs::write(
        tmp_home.path().join(".cfgd.env"),
        "export PATH=\"/usr/local/bin:$PATH\"\n",
    )
    .unwrap();
    let _home = with_test_home_guard(tmp_home.path());

    let state = test_state();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(BootstrappingPackageManager::new(
            "brew",
            // One dir already exists in env; one is new.
            &["/usr/local/bin", "/opt/homebrew/bin"],
        )));

    let reconciler = Reconciler::new(&registry, &state);
    let resolved = make_empty_resolved();

    let modules = vec![ResolvedModule {
        name: "tools".to_string(),
        packages: vec![ResolvedPackage {
            canonical_name: "ripgrep".to_string(),
            resolved_name: "ripgrep".to_string(),
            manager: "brew".to_string(),
            version: None,
            script: None,
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "tools".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "ripgrep".to_string(),
                        resolved_name: "ripgrep".to_string(),
                        manager: "brew".to_string(),
                        version: None,
                        script: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
        )
        .unwrap();

    assert_eq!(result.status, ApplyStatus::Success);

    let contents = std::fs::read_to_string(tmp_home.path().join(".cfgd.env")).unwrap();
    // Existing dir must remain; new dir must be appended; no duplicate.
    assert!(
        contents.contains("/usr/local/bin"),
        "pre-existing line preserved"
    );
    assert!(
        contents.contains("/opt/homebrew/bin"),
        "new dir appended: {contents}"
    );
    assert_eq!(
        contents.matches("/usr/local/bin").count(),
        1,
        "no dup of existing dir: {contents}"
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
        }],
        files: vec![],
        env: vec![],
        aliases: vec![],
        post_apply_scripts: vec![],
        pre_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        system: HashMap::new(),
        depends: vec![],
        dir: PathBuf::from("."),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "ghost".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "anything".to_string(),
                        resolved_name: "anything".to_string(),
                        manager: "no-such-manager".to_string(),
                        version: None,
                        script: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "scripted".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![
                        ResolvedPackage {
                            canonical_name: "pkg-a".to_string(),
                            resolved_name: "pkg-a".to_string(),
                            manager: "script".to_string(),
                            version: None,
                            script: Some(format!("touch {}", marker_a.display())),
                        },
                        ResolvedPackage {
                            canonical_name: "pkg-b".to_string(),
                            resolved_name: "pkg-b".to_string(),
                            manager: "script".to_string(),
                            version: None,
                            script: Some(format!("touch {}", marker_b.display())),
                        },
                    ],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "bad-script".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "broken".to_string(),
                        resolved_name: "broken".to_string(),
                        manager: "script".to_string(),
                        version: None,
                        script: Some("exit 3".to_string()),
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &modules,
            ReconcileContext::Apply,
            false,
        )
        .unwrap();

    // Apply records the failure inside action_results[0].success=false.
    assert!(
        !result.action_results[0].success,
        "script failure must surface as action failure"
    );
}

// -----------------------------------------------------------------------
// apply: module-level onChange scripts (L384-400 in apply.rs)
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    // Plan includes a file action that affects this module → records a
    // `module:mymod:files:1` change entry → the module-level on_change runs.
    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "mymod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &module_actions,
            ReconcileContext::Apply,
            false,
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
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
            Some(&PhaseName::Modules),
            &module_actions,
            ReconcileContext::Apply,
            false,
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "skipmod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
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
            Some(&PhaseName::Modules),
            &module_actions,
            ReconcileContext::Apply,
            true,
        )
        .unwrap();

    assert!(
        !marker.exists(),
        "skip_scripts=true must suppress module on_change execution"
    );
}

// ---------------------------------------------------------------------------
// secret_env_collector: ResolveEnv action with a registered provider drives
// the secret-env injection branch of apply.rs (lines L256-292). After every
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
        phases: vec![Phase {
            name: PhaseName::Secrets,
            actions: vec![secret_action],
        }],
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
    // PathBuf usage to anchor the import even when run on platforms where
    // home expansion differs.
    let _: PathBuf = tmp.path().to_path_buf();
}

// ---------------------------------------------------------------------------
// Module on_change error handling (apply.rs L384-400): script failure with
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
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "failmod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let result = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &module_actions,
            ReconcileContext::Apply,
            false,
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
            run: "exit 5".to_string(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: Some(false),
        }],
        system: HashMap::new(),
        depends: vec![],
        dir: dir.path().to_path_buf(),
    }];

    let plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "abortmod".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![crate::modules::ResolvedFile {
                        source: source.clone(),
                        target: target.clone(),
                        is_git_source: false,
                        strategy: Some(crate::config::FileStrategy::Copy),
                        encryption: None,
                    }],
                },
            })],
        }],
        warnings: vec![],
    };

    let printer = test_printer();
    let err = reconciler
        .apply(
            &plan,
            &resolved,
            dir.path(),
            &printer,
            Some(&PhaseName::Modules),
            &module_actions,
            ReconcileContext::Apply,
            false,
        )
        .expect_err("explicit continueOnError=false must return Err");
    let _ = err.to_string();
}

// ---------------------------------------------------------------------------
// Profile on_change error handling (apply.rs L327-339): identical pattern but
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
        run: "exit 11".to_string(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: Some(false),
    }];

    let file_actions = vec![FileAction::Create {
        source: source.clone(),
        target: target.clone(),
        origin: "local".to_string(),
        strategy: crate::config::FileStrategy::Copy,
        source_hash: None,
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
        )
        .expect_err("profile onChange continueOnError=false must propagate err");
    let _ = err.to_string();
}
