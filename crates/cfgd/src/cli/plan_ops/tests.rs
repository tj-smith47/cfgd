use std::path::PathBuf;

use cfgd_core::config::{FileStrategy, ScriptEntry};
use cfgd_core::output::{Printer, Verbosity};
use cfgd_core::providers::{FileAction, PackageAction, SecretAction};
use cfgd_core::reconciler::ActionResult;
use cfgd_core::reconciler::ApplyResult;
use cfgd_core::reconciler::{
    Action, EnvAction, ModuleAction, ModuleActionKind, ModuleScope, ModuleSection, Phase,
    PhaseName, Plan, ScriptAction, ScriptPhase, SystemAction,
};
use cfgd_core::state::{ApplyStatus, StateStore};

use super::*;

fn file_create(target: &str) -> Action {
    Action::File(FileAction::Create {
        source: PathBuf::from("/src/dotfiles/.zshrc"),
        target: PathBuf::from(target),
        origin: "test".to_string(),
        strategy: FileStrategy::Symlink,
        source_hash: None,
    })
}

fn file_update(target: &str) -> Action {
    Action::File(FileAction::Update {
        source: PathBuf::from("/src/dotfiles/.zshrc"),
        target: PathBuf::from(target),
        diff: "--- old\n+++ new\n".to_string(),
        origin: "test".to_string(),
        strategy: FileStrategy::Copy,
        source_hash: None,
    })
}

fn file_delete(target: &str) -> Action {
    Action::File(FileAction::Delete {
        target: PathBuf::from(target),
        origin: "test".to_string(),
    })
}

fn file_chmod(target: &str) -> Action {
    Action::File(FileAction::SetPermissions {
        target: PathBuf::from(target),
        mode: 0o755,
        origin: "test".to_string(),
    })
}

fn file_skip(target: &str) -> Action {
    Action::File(FileAction::Skip {
        target: PathBuf::from(target),
        reason: "already in sync".to_string(),
        origin: "test".to_string(),
    })
}

fn pkg_bootstrap() -> Action {
    Action::Package(PackageAction::Bootstrap {
        manager: "brew".to_string(),
        method: "script".to_string(),
        origin: "test".to_string(),
    })
}

fn pkg_install(manager: &str, packages: Vec<&str>) -> Action {
    Action::Package(PackageAction::Install {
        manager: manager.to_string(),
        packages: packages.into_iter().map(|s| s.to_string()).collect(),
        origin: "test".to_string(),
    })
}

fn pkg_uninstall(manager: &str, packages: Vec<&str>) -> Action {
    Action::Package(PackageAction::Uninstall {
        manager: manager.to_string(),
        packages: packages.into_iter().map(|s| s.to_string()).collect(),
        origin: "test".to_string(),
    })
}

fn pkg_skip() -> Action {
    Action::Package(PackageAction::Skip {
        manager: "apt".to_string(),
        reason: "not available".to_string(),
        origin: "test".to_string(),
    })
}

fn secret_decrypt() -> Action {
    Action::Secret(SecretAction::Decrypt {
        source: PathBuf::from("/secrets/foo.enc"),
        target: PathBuf::from("/secrets/foo"),
        backend: "age".to_string(),
        origin: "test".to_string(),
    })
}

fn secret_resolve() -> Action {
    Action::Secret(SecretAction::Resolve {
        provider: "1password".to_string(),
        reference: "op://vault/item".to_string(),
        target: PathBuf::from("/etc/foo"),
        origin: "test".to_string(),
    })
}

fn secret_resolve_env() -> Action {
    Action::Secret(SecretAction::ResolveEnv {
        provider: "vault".to_string(),
        reference: "secret/data/app".to_string(),
        envs: vec!["TOKEN".to_string(), "KEY".to_string()],
        origin: "test".to_string(),
    })
}

fn secret_skip() -> Action {
    Action::Secret(SecretAction::Skip {
        source: "bitwarden".to_string(),
        reason: "unavailable".to_string(),
        origin: "test".to_string(),
    })
}

fn system_set() -> Action {
    Action::System(SystemAction::SetValue {
        configurator: "sysctl".to_string(),
        key: "net.ipv4.ip_forward".to_string(),
        desired: "1".to_string(),
        current: "0".to_string(),
        origin: "test".to_string(),
    })
}

fn system_skip() -> Action {
    Action::System(SystemAction::Skip {
        configurator: "sysctl".to_string(),
        reason: "already set".to_string(),
        origin: "test".to_string(),
        unknown: false,
    })
}

fn script_run() -> Action {
    Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple("setup.sh".to_string()),
        phase: ScriptPhase::PreApply,
        origin: "test".to_string(),
    })
}

fn module_install() -> Action {
    Action::Module(ModuleAction {
        module_name: "dev-tools".to_string(),
        kind: ModuleActionKind::InstallPackages { resolved: vec![] },
        origin: None,
    })
}

fn module_run_script() -> Action {
    Action::Module(ModuleAction {
        module_name: "dev-tools".to_string(),
        kind: ModuleActionKind::RunScript {
            script: ScriptEntry::Simple("install.sh".to_string()),
            phase: ScriptPhase::PostApply,
        },
        origin: None,
    })
}

fn module_deploy_files() -> Action {
    Action::Module(ModuleAction {
        module_name: "dotfiles".to_string(),
        kind: ModuleActionKind::DeployFiles { files: vec![] },
        origin: None,
    })
}

fn module_skip() -> Action {
    Action::Module(ModuleAction {
        module_name: "optional".to_string(),
        kind: ModuleActionKind::Skip {
            reason: "dependency not met".to_string(),
        },
        origin: None,
    })
}

fn env_write() -> Action {
    Action::Env(EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/user/.cfgd.env"),
        content: "export FOO=bar".to_string(),
    })
}

fn env_inject() -> Action {
    Action::Env(EnvAction::InjectSourceLine {
        rc_path: PathBuf::from("/home/user/.zshrc"),
        line: ". ~/.cfgd.env".to_string(),
    })
}

fn make_plan(phases: Vec<(PhaseName, Vec<Action>)>) -> Plan {
    Plan {
        phases: phases
            .into_iter()
            .map(|(name, actions)| Phase {
                name,
                actions,
                scope: None,
            })
            .collect(),
        warnings: vec![],
    }
}

/// A single (module, section) Phase, the shape `Reconciler::split_module_phases`
/// really produces — one Phase per consecutive module/section run, never one
/// combined "Modules" phase holding every kind of module action together.
fn scoped_phase(module: &str, section: ModuleSection, actions: Vec<Action>) -> Phase {
    Phase {
        name: PhaseName::Modules,
        actions,
        scope: Some(ModuleScope {
            module: module.to_string(),
            section,
        }),
    }
}

fn make_plan_from_phases(phases: Vec<Phase>) -> Plan {
    Plan {
        phases,
        warnings: vec![],
    }
}

fn apply_result(status: ApplyStatus, succeeded: usize, failed: usize) -> ApplyResult {
    let mut results = Vec::new();
    for _ in 0..succeeded {
        results.push(ActionResult {
            phase: "files".to_string(),
            description: "create /etc/foo".to_string(),
            success: true,
            error: None,
            changed: true,
        });
    }
    for _ in 0..failed {
        results.push(ActionResult {
            phase: "packages".to_string(),
            description: "install brew:rg".to_string(),
            success: false,
            error: Some("exit code 1".to_string()),
            changed: false,
        });
    }
    ApplyResult {
        action_results: results,
        status,
        apply_id: 0,
        aborted: None,
        planned_total: succeeded + failed,
    }
}

#[test]
fn action_type_str_file_variants() {
    assert_eq!(action_type_str(&file_create("/etc/foo")), "create");
    assert_eq!(action_type_str(&file_update("/etc/foo")), "update");
    assert_eq!(action_type_str(&file_delete("/etc/foo")), "delete");
    assert_eq!(action_type_str(&file_chmod("/etc/foo")), "chmod");
    assert_eq!(action_type_str(&file_skip("/etc/foo")), "skip");
}

#[test]
fn action_type_str_package_variants() {
    assert_eq!(action_type_str(&pkg_bootstrap()), "bootstrap");
    assert_eq!(action_type_str(&pkg_install("brew", vec!["rg"])), "install");
    assert_eq!(
        action_type_str(&pkg_uninstall("brew", vec!["rg"])),
        "uninstall"
    );
    assert_eq!(action_type_str(&pkg_skip()), "skip");
}

#[test]
fn action_type_str_secret_variants() {
    assert_eq!(action_type_str(&secret_decrypt()), "decrypt");
    assert_eq!(action_type_str(&secret_resolve()), "resolve");
    assert_eq!(action_type_str(&secret_resolve_env()), "resolve-env");
    assert_eq!(action_type_str(&secret_skip()), "skip");
}

#[test]
fn action_targets_file_variants_expose_the_target_path() {
    assert_eq!(action_targets(&file_create("/etc/foo")), vec!["/etc/foo"]);
    assert_eq!(action_targets(&file_update("/etc/foo")), vec!["/etc/foo"]);
    assert_eq!(action_targets(&file_delete("/etc/foo")), vec!["/etc/foo"]);
    assert_eq!(action_targets(&file_chmod("/etc/foo")), vec!["/etc/foo"]);
    assert_eq!(action_targets(&file_skip("/etc/foo")), vec!["/etc/foo"]);
}

#[test]
fn action_targets_env_variants_expose_file_and_rc_paths() {
    assert_eq!(action_targets(&env_write()), vec!["/home/user/.cfgd.env"]);
    assert_eq!(action_targets(&env_inject()), vec!["/home/user/.zshrc"]);
    // RefreshLiveSession writes no file — no target.
    let refresh = Action::Env(EnvAction::RefreshLiveSession {
        vars: vec![("FOO".to_string(), "bar".to_string())],
    });
    assert!(action_targets(&refresh).is_empty());
}

#[test]
fn action_targets_secret_decrypt_and_resolve_expose_target_others_empty() {
    assert_eq!(action_targets(&secret_decrypt()), vec!["/secrets/foo"]);
    assert_eq!(action_targets(&secret_resolve()), vec!["/etc/foo"]);
    // ResolveEnv injects into the env file (no own path) and Skip touch nothing.
    assert!(action_targets(&secret_resolve_env()).is_empty());
    assert!(action_targets(&secret_skip()).is_empty());
}

#[test]
fn action_targets_module_deploy_files_lists_every_file_others_empty() {
    let deploy = Action::Module(ModuleAction {
        module_name: "dotfiles".to_string(),
        kind: ModuleActionKind::DeployFiles {
            files: vec![
                cfgd_core::modules::ResolvedFile {
                    source: PathBuf::from("/m/.zshrc"),
                    target: PathBuf::from("/home/user/.zshrc"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                },
                cfgd_core::modules::ResolvedFile {
                    source: PathBuf::from("/m/.vimrc"),
                    target: PathBuf::from("/home/user/.vimrc"),
                    is_git_source: false,
                    strategy: None,
                    encryption: None,
                    permissions: None,
                },
            ],
        },
        origin: None,
    });
    assert_eq!(
        action_targets(&deploy),
        vec!["/home/user/.zshrc", "/home/user/.vimrc"]
    );
    assert!(action_targets(&module_install()).is_empty());
    assert!(action_targets(&module_run_script()).is_empty());
    assert!(action_targets(&module_skip()).is_empty());
}

#[test]
fn action_targets_empty_for_pkg_system_script() {
    assert!(action_targets(&pkg_install("brew", vec!["rg"])).is_empty());
    assert!(action_targets(&system_set()).is_empty());
    assert!(action_targets(&script_run()).is_empty());
}

#[test]
fn action_type_str_system_variants() {
    assert_eq!(action_type_str(&system_set()), "set");
    assert_eq!(action_type_str(&system_skip()), "skip");
}

#[test]
fn action_type_str_script_and_module_variants() {
    assert_eq!(action_type_str(&script_run()), "run");
    assert_eq!(action_type_str(&module_install()), "install");
    assert_eq!(action_type_str(&module_deploy_files()), "deploy");
    assert_eq!(action_type_str(&module_run_script()), "run");
    assert_eq!(action_type_str(&module_skip()), "skip");
}

#[test]
fn action_type_str_env_variants() {
    assert_eq!(action_type_str(&env_write()), "write");
    assert_eq!(action_type_str(&env_inject()), "inject");
}

#[test]
fn action_path_file_create() {
    let path = action_path(&PhaseName::Files, &file_create("/home/user/.zshrc"));
    assert_eq!(path, "files:/home/user/.zshrc");
}

#[test]
fn action_path_package_install() {
    let path = action_path(&PhaseName::Packages, &pkg_install("brew", vec!["rg"]));
    assert_eq!(path, "packages.brew");
}

#[test]
fn action_path_system_set_value() {
    let path = action_path(&PhaseName::System, &system_set());
    assert_eq!(path, "system.sysctl.net.ipv4.ip_forward");
}

#[test]
fn action_path_system_skip() {
    let path = action_path(&PhaseName::System, &system_skip());
    assert_eq!(path, "system.sysctl");
}

#[test]
fn action_path_secret_resolve() {
    let path = action_path(&PhaseName::Secrets, &secret_resolve());
    assert_eq!(path, "secrets.1password.op://vault/item");
}

#[test]
fn action_path_secret_resolve_env() {
    let path = action_path(&PhaseName::Secrets, &secret_resolve_env());
    assert_eq!(path, "secrets.vault.secret/data/app:[TOKEN,KEY]");
}

#[test]
fn action_path_secret_skip() {
    let path = action_path(&PhaseName::Secrets, &secret_skip());
    assert_eq!(path, "secrets.bitwarden");
}

#[test]
fn action_path_script_run() {
    let path = action_path(&PhaseName::PreScripts, &script_run());
    assert_eq!(path, "pre-scripts:setup.sh");
}

#[test]
fn action_path_module() {
    let path = action_path(&PhaseName::Modules, &module_install());
    assert_eq!(path, "modules.dev-tools");
}

#[test]
fn action_path_env_write() {
    let path = action_path(&PhaseName::Env, &env_write());
    assert_eq!(path, "env:/home/user/.cfgd.env");
}

#[test]
fn action_path_env_inject() {
    let path = action_path(&PhaseName::Env, &env_inject());
    assert_eq!(path, "env:/home/user/.zshrc");
}

#[test]
fn pattern_matches_exact() {
    assert!(pattern_matches("files:/etc/foo", "files:/etc/foo"));
}

#[test]
fn pattern_matches_prefix_dot_separator() {
    assert!(pattern_matches("packages", "packages.brew.ripgrep"));
    assert!(pattern_matches("packages.brew", "packages.brew.ripgrep"));
}

#[test]
fn pattern_matches_prefix_colon_separator() {
    assert!(pattern_matches("files", "files:/etc/foo"));
}

#[test]
fn pattern_matches_no_partial_word_match() {
    assert!(!pattern_matches("pack", "packages.brew.ripgrep"));
}

#[test]
fn pattern_matches_no_match_different_phase() {
    assert!(!pattern_matches("secrets", "packages.brew.ripgrep"));
}

#[test]
fn filter_plan_noop_when_empty_filters() {
    let mut plan = make_plan(vec![(
        PhaseName::Files,
        vec![file_create("/etc/foo"), file_update("/etc/bar")],
    )]);
    filter_plan(&mut plan, &[], &[]);
    assert_eq!(plan.phases[0].actions.len(), 2);
}

#[test]
fn filter_plan_skip_removes_matching_file_actions() {
    let mut plan = make_plan(vec![
        (
            PhaseName::Files,
            vec![file_create("/etc/foo"), file_update("/etc/bar")],
        ),
        (
            PhaseName::Packages,
            vec![pkg_install("brew", vec!["rg", "fd"])],
        ),
    ]);
    filter_plan(&mut plan, &["files".to_string()], &[]);

    // Every action in the Files phase was skipped, so the phase itself must
    // not survive with zero actions.
    assert!(
        !plan.phases.iter().any(|p| p.name == PhaseName::Files),
        "the Files phase should be dropped entirely once emptied: {:?}",
        plan.phases
    );
    let pkg_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Packages)
        .unwrap();
    assert_eq!(
        pkg_phase.actions.len(),
        1,
        "package actions should be untouched"
    );
}

#[test]
fn filter_plan_only_keeps_matching_actions() {
    let mut plan = make_plan(vec![
        (
            PhaseName::Files,
            vec![file_create("/etc/foo"), file_update("/etc/bar")],
        ),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    filter_plan(&mut plan, &[], &["packages".to_string()]);

    // Every file action fell outside the --only scope, so the Files phase
    // itself must not survive with zero actions.
    assert!(
        !plan.phases.iter().any(|p| p.name == PhaseName::Files),
        "the Files phase should be dropped entirely once emptied: {:?}",
        plan.phases
    );
    let pkg_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Packages)
        .unwrap();
    assert_eq!(
        pkg_phase.actions.len(),
        1,
        "package actions inside --only scope should remain"
    );
}

#[test]
fn filter_plan_skip_individual_packages() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![pkg_install("brew", vec!["rg", "fd", "bat"])],
    )]);
    filter_plan(&mut plan, &["packages.brew.rg".to_string()], &[]);

    let phase = &plan.phases[0];
    assert_eq!(phase.actions.len(), 1);
    if let Action::Package(PackageAction::Install { packages, .. }) = &phase.actions[0] {
        assert!(
            !packages.contains(&"rg".to_string()),
            "rg should be skipped"
        );
        assert!(packages.contains(&"fd".to_string()), "fd should remain");
        assert!(packages.contains(&"bat".to_string()), "bat should remain");
    } else {
        panic!("expected Install action");
    }
}

#[test]
fn filter_plan_only_specific_packages() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![pkg_install("brew", vec!["rg", "fd", "bat"])],
    )]);
    filter_plan(&mut plan, &[], &["packages.brew.rg".to_string()]);

    let phase = &plan.phases[0];
    assert_eq!(phase.actions.len(), 1);
    if let Action::Package(PackageAction::Install { packages, .. }) = &phase.actions[0] {
        assert_eq!(packages, &["rg"], "only rg should remain");
    } else {
        panic!("expected Install action");
    }
}

#[test]
fn filter_plan_skip_removes_entire_manager_with_all_packages_skipped() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![
            pkg_install("brew", vec!["rg"]),
            pkg_install("cargo", vec!["bat"]),
        ],
    )]);
    filter_plan(&mut plan, &["packages.brew".to_string()], &[]);

    let phase = &plan.phases[0];
    assert_eq!(
        phase.actions.len(),
        1,
        "brew install should be fully removed"
    );
    if let Action::Package(PackageAction::Install { manager, .. }) = &phase.actions[0] {
        assert_eq!(manager, "cargo");
    } else {
        panic!("expected cargo Install action");
    }
}

#[test]
fn filter_plan_only_specific_manager_keeps_just_that_manager() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![
            pkg_install("brew", vec!["ripgrep"]),
            pkg_install("cargo", vec!["bat"]),
        ],
    )]);
    filter_plan(&mut plan, &[], &["packages.brew".to_string()]);

    let phase = &plan.phases[0];
    assert_eq!(phase.actions.len(), 1, "only brew install should remain");
    if let Action::Package(PackageAction::Install { manager, .. }) = &phase.actions[0] {
        assert_eq!(manager, "brew");
    } else {
        panic!("expected brew Install action");
    }
}

#[test]
fn filter_plan_skip_uninstall_individual_packages() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![pkg_uninstall("brew", vec!["old-tool", "keep-me"])],
    )]);
    filter_plan(&mut plan, &["packages.brew.old-tool".to_string()], &[]);

    if let Action::Package(PackageAction::Uninstall { packages, .. }) = &plan.phases[0].actions[0] {
        assert_eq!(packages, &["keep-me".to_string()]);
    } else {
        panic!("expected Uninstall action");
    }
}

#[test]
fn strip_scripts_removes_pre_post_script_phases() {
    let mut plan = make_plan(vec![
        (PhaseName::PreScripts, vec![script_run()]),
        (PhaseName::Files, vec![file_create("/etc/foo")]),
        (PhaseName::PostScripts, vec![script_run()]),
    ]);
    strip_scripts_from_plan(&mut plan);

    assert!(
        plan.phases
            .iter()
            .all(|p| p.name != PhaseName::PreScripts && p.name != PhaseName::PostScripts),
        "pre/post-script phases must be removed"
    );
    assert_eq!(plan.phases.len(), 1, "only the Files phase should remain");
}

#[test]
fn strip_scripts_removes_module_run_script_actions() {
    // Realistic post-split shape: one Phase per (module, section) run, never
    // one combined "Modules" phase holding install + run_script + deploy_files
    // together — `split_module_phases` can never produce that shape.
    let mut plan = make_plan_from_phases(vec![
        scoped_phase("dev-tools", ModuleSection::Packages, vec![module_install()]),
        scoped_phase(
            "dev-tools",
            ModuleSection::PostScripts,
            vec![module_run_script()],
        ),
        scoped_phase(
            "dotfiles",
            ModuleSection::Files,
            vec![module_deploy_files()],
        ),
    ]);
    strip_scripts_from_plan(&mut plan);

    assert!(
        plan.phases.iter().all(|p| !matches!(
            &p.scope,
            Some(ModuleScope {
                section: ModuleSection::PostScripts,
                ..
            })
        )),
        "the dev-tools/Post-Scripts phase held only a RunScript action, so it \
         must be dropped entirely, not left as an empty phase"
    );
    assert_eq!(
        plan.phases.len(),
        2,
        "only the Packages and Files phases should remain"
    );
    for phase in &plan.phases {
        assert!(
            phase.actions.iter().all(|a| {
                !matches!(
                    a,
                    Action::Module(ModuleAction {
                        kind: ModuleActionKind::RunScript { .. },
                        ..
                    })
                )
            }),
            "no RunScript actions should remain"
        );
    }
}

// Item 1 regression: before the module-plan split, `strip_scripts_from_plan`
// only ever emptied an existing Modules phase's actions, never dropped it, so
// the missing final `plan.phases.retain(...)` never showed up. Now every
// module section is its own Phase, so a section made entirely of RunScript
// actions (`nvim / Post-Scripts` on a plain `--skip-scripts` run) must vanish
// completely instead of surviving with zero actions.
//
// Without the trailing `plan.phases.retain(|p| !p.actions.is_empty())` in
// `strip_scripts_from_plan`, this phase's `actions` empties out (its only
// action is a RunScript) but the `Phase` itself stays in `plan.phases`, so
// `plan.phases.is_empty()` is false and this assertion fails.
#[test]
fn strip_scripts_drops_a_phase_left_entirely_empty() {
    let mut plan = make_plan_from_phases(vec![scoped_phase(
        "nvim",
        ModuleSection::PostScripts,
        vec![module_run_script()],
    )]);
    strip_scripts_from_plan(&mut plan);

    assert!(
        plan.phases.is_empty(),
        "a phase whose only action was stripped must not survive empty: {:?}",
        plan.phases
    );
}

// Item 1 regression, `filter_plan` side: a `--skip` pattern can exclude every
// action in one (module, section) Phase without touching a sibling section of
// the same module. Without the trailing
// `plan.phases.retain(|p| !p.actions.is_empty())` in `filter_plan`, the
// `nvim / Packages` phase's `actions` empties out but the `Phase` itself
// stays in `plan.phases`, so `plan.phases.is_empty()` is false and this
// assertion fails.
#[test]
fn filter_plan_drops_a_phase_left_entirely_empty() {
    let module_pkg_install = Action::Module(ModuleAction {
        module_name: "nvim".to_string(),
        kind: ModuleActionKind::InstallPackages { resolved: vec![] },
        origin: None,
    });
    let mut plan = make_plan_from_phases(vec![scoped_phase(
        "nvim",
        ModuleSection::Packages,
        vec![module_pkg_install],
    )]);
    filter_plan(&mut plan, &["modules.nvim".to_string()], &[]);

    assert!(
        plan.phases.is_empty(),
        "a phase whose only action was skipped must not survive empty: {:?}",
        plan.phases
    );
}

#[test]
fn build_plan_output_counts_actions_and_sets_context() {
    let plan = make_plan(vec![
        (
            PhaseName::Files,
            vec![file_create("/etc/foo"), file_update("/etc/bar")],
        ),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let output = build_plan_output(&plan, "my-machine", None);

    assert_eq!(output.context, "my-machine");
    assert_eq!(output.total_actions, 3);
    assert_eq!(output.phases.len(), 2);
    let files_phase = output.phases.iter().find(|p| p.phase == "Files").unwrap();
    assert_eq!(files_phase.actions.len(), 2);
    assert!(
        files_phase
            .actions
            .iter()
            .any(|a| a.action_type == "create"),
        "expected create action type"
    );
    assert!(
        files_phase
            .actions
            .iter()
            .any(|a| a.action_type == "update"),
        "expected update action type"
    );
}

#[test]
fn build_plan_output_phase_filter_excludes_other_phases() {
    let plan = make_plan(vec![
        (PhaseName::Files, vec![file_create("/etc/foo")]),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let output = build_plan_output(&plan, "ctx", Some(&PhaseName::Files));

    assert_eq!(output.phases.len(), 1);
    assert_eq!(output.phases[0].phase, "Files");
    assert_eq!(output.total_actions, 1);
}

#[test]
fn build_plan_output_scoped_phase_carries_module_and_kebab_section() {
    let plan = make_plan_from_phases(vec![scoped_phase(
        "nvim",
        ModuleSection::PostScripts,
        vec![module_run_script()],
    )]);
    let output = build_plan_output(&plan, "ctx", None);

    assert_eq!(output.phases.len(), 1);
    assert_eq!(output.phases[0].phase, "Modules");
    assert_eq!(output.phases[0].module.as_deref(), Some("nvim"));
    assert_eq!(
        output.phases[0].section.as_deref(),
        Some("post-scripts"),
        "section must serialize in kebab form, matching ModuleSection::as_str"
    );

    let json = serde_json::to_value(&output).unwrap();
    let phase = &json["phases"][0];
    assert_eq!(phase["module"], serde_json::json!("nvim"));
    assert_eq!(phase["section"], serde_json::json!("post-scripts"));
}

#[test]
fn build_plan_output_non_module_phase_omits_module_and_section_keys() {
    let plan = make_plan(vec![(PhaseName::Files, vec![file_create("/etc/foo")])]);
    let output = build_plan_output(&plan, "ctx", None);

    assert_eq!(output.phases[0].module, None);
    assert_eq!(output.phases[0].section, None);

    // `skip_serializing_if` back-compat guarantee: a non-module phase's wire
    // form carries no `module`/`section` keys at all, not `null` values.
    let json = serde_json::to_value(&output).unwrap();
    let phase = &json["phases"][0];
    assert!(
        phase.get("module").is_none(),
        "non-module phase must omit the module key entirely: {phase}"
    );
    assert!(
        phase.get("section").is_none(),
        "non-module phase must omit the section key entirely: {phase}"
    );
}

fn module_install_from_source(source: &str) -> Action {
    Action::Module(ModuleAction::with_origin(
        "dev-tools",
        ModuleActionKind::InstallPackages { resolved: vec![] },
        Some(source.to_string()),
    ))
}

#[test]
fn build_plan_output_carries_source_module_origin() {
    // A source-delivered module exposes its origin in the structured payload;
    // a co-planned local module omits origin (serde skips None on the wire).
    let plan = make_plan(vec![(
        PhaseName::Modules,
        vec![module_install_from_source("acme"), module_install()],
    )]);
    let output = build_plan_output(&plan, "ctx", None);

    let actions = &output.phases[0].actions;
    let sourced = actions
        .iter()
        .find(|a| a.description.contains(" <- acme"))
        .expect("source-delivered module action present");
    assert_eq!(sourced.origin.as_deref(), Some("acme"));

    let local = actions
        .iter()
        .find(|a| !a.description.contains(" <- "))
        .expect("local module action present");
    assert_eq!(local.origin, None, "local module must omit origin");

    // The wire form omits origin for the local action and includes it for
    // the source-delivered one (serde camelCase + skip_serializing_if=None).
    let json = serde_json::to_value(&output).unwrap();
    let acts = json["phases"][0]["actions"].as_array().unwrap();
    assert!(
        acts.iter()
            .any(|a| a["origin"] == serde_json::json!("acme")),
        "expected origin: \"acme\" in json: {json}"
    );
    assert!(
        acts.iter().any(|a| a.get("origin").is_none()),
        "expected a local action with no origin key in json: {json}"
    );
}

#[test]
fn build_plan_output_local_only_omits_all_origins() {
    // Regression: a plan of only local modules emits no origin keys at all.
    let plan = make_plan(vec![(
        PhaseName::Modules,
        vec![module_install(), module_deploy_files()],
    )]);
    let output = build_plan_output(&plan, "ctx", None);
    for phase in &output.phases {
        for action in &phase.actions {
            assert_eq!(action.origin, None, "local plan must carry no origin");
            assert!(
                !action.description.contains(" <- "),
                "local plan must carry no provenance suffix"
            );
        }
    }
    let json = serde_json::to_value(&output).unwrap();
    let acts = json["phases"][0]["actions"].as_array().unwrap();
    assert!(
        acts.iter().all(|a| a.get("origin").is_none()),
        "no origin key expected in local-only json: {json}"
    );
}

#[test]
fn build_plan_output_empty_plan_has_zero_actions() {
    let plan = make_plan(vec![]);
    let output = build_plan_output(&plan, "ctx", None);

    assert_eq!(output.total_actions, 0);
    assert!(output.phases.is_empty());
}

// `build_plan_output`'s `PlanActionOutput.description` is the
// `-o json` plan payload — it must preserve a multi-line inline script's
// run_str body byte-identical, never condensed. Condensing belongs solely to
// human render sites (`display_plan_table`, `cli/apply.rs`'s dry-run preview).
#[test]
fn build_plan_output_script_action_json_preserves_raw_multiline_body() {
    let raw_body = "echo line-one\necho line-two\necho line-three";
    let action = Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple(raw_body.to_string()),
        phase: ScriptPhase::PreApply,
        origin: "test".to_string(),
    });
    let plan = make_plan(vec![(PhaseName::PreScripts, vec![action])]);
    let output = build_plan_output(&plan, "ctx", None);

    let desc = &output.phases[0].actions[0].description;
    assert!(
        desc.contains(raw_body),
        "PlanActionOutput.description must preserve the raw multi-line body byte-identical, got: {desc}"
    );
}

// Mirrors the phase-script case above but for a MODULE script:
// `format_module_action_body`'s `ModuleActionKind::RunScript` arm used to
// condense the body inline, truncating it in the `-o json` plan payload too
// (it shares the same `format_plan_items` -> `build_plan_output` zip).
#[test]
fn build_plan_output_module_script_action_json_preserves_raw_multiline_body() {
    let raw_body = "echo module-line-one\necho module-line-two\necho module-line-three";
    let action = Action::Module(ModuleAction {
        module_name: "dev-tools".to_string(),
        kind: ModuleActionKind::RunScript {
            script: ScriptEntry::Simple(raw_body.to_string()),
            phase: ScriptPhase::PostApply,
        },
        origin: None,
    });
    let plan = make_plan(vec![(PhaseName::Modules, vec![action])]);
    let output = build_plan_output(&plan, "ctx", None);

    let desc = &output.phases[0].actions[0].description;
    assert!(
        desc.contains(raw_body),
        "PlanActionOutput.description must preserve a MODULE script's raw multi-line body byte-identical, got: {desc}"
    );
}

#[test]
fn print_apply_result_success_emits_ok_role() {
    let result = apply_result(ApplyStatus::Success, 5, 0);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let status = print_apply_result(&result, &printer, None);

    assert_eq!(status, ApplyStatus::Success);
    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("5 action(s) succeeded"),
        "expected success count in output, got: {out}"
    );
}

#[test]
fn print_apply_result_failure_emits_fail_role() {
    let result = apply_result(ApplyStatus::Failed, 0, 3);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let status = print_apply_result(&result, &printer, None);

    assert_eq!(status, ApplyStatus::Failed);
    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("3 action(s) failed"),
        "expected failure count in output, got: {out}"
    );
}

#[test]
fn print_apply_result_partial_emits_both_lines() {
    let result = apply_result(ApplyStatus::Partial, 4, 2);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let status = print_apply_result(&result, &printer, None);

    assert_eq!(status, ApplyStatus::Partial);
    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("4 action(s) succeeded"),
        "expected success line in partial output, got: {out}"
    );
    assert!(
        out.contains("2 action(s) failed"),
        "expected failure line in partial output, got: {out}"
    );
}

#[test]
fn print_apply_result_in_progress_emits_warn() {
    let result = apply_result(ApplyStatus::InProgress, 0, 0);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let status = print_apply_result(&result, &printer, None);

    assert_eq!(status, ApplyStatus::InProgress);
    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("in progress"),
        "expected in-progress message, got: {out}"
    );
}

#[test]
fn print_apply_result_with_elapsed_includes_duration() {
    let result = apply_result(ApplyStatus::Success, 2, 0);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    print_apply_result(
        &result,
        &printer,
        Some(std::time::Duration::from_millis(1500)),
    );
    let out = cfgd_core::output::strip_ansi(&buf.lock().unwrap());
    assert!(
        out.contains("action(s) succeeded"),
        "expected success subject in output, got: {out}"
    );
    // Renderer formats Duration::from_millis(1500) as " (1.5s)" — see
    // `crates/cfgd-core/src/output/renderer/status.rs::duration_trailed_in_parens`.
    assert!(
        out.contains("(1.5s)"),
        "expected '(1.5s)' duration suffix, got: {out}"
    );
}

#[test]
fn print_apply_result_aborted_emits_warn_with_succeeded_count() {
    let result = apply_result(ApplyStatus::Aborted, 3, 0);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let status = print_apply_result(&result, &printer, None);

    assert_eq!(status, ApplyStatus::Aborted);
    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("aborted by signal"),
        "expected abort message in output, got: {out}"
    );
    assert!(
        out.contains("3 action(s) applied"),
        "expected succeeded count in abort output, got: {out}"
    );
}

#[test]
fn print_apply_result_partial_with_elapsed_attaches_duration_to_failed_line() {
    let result = apply_result(ApplyStatus::Partial, 1, 4);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    print_apply_result(
        &result,
        &printer,
        Some(std::time::Duration::from_millis(2500)),
    );
    let out = cfgd_core::output::strip_ansi(&buf.lock().unwrap());
    // Partial branch emits the Accent-tagged failure line through the
    // StatusBuilder path that attaches `.duration(d)`. The success line
    // does not get the duration.
    let failed_line = out
        .lines()
        .find(|l| l.contains("4 action(s) failed"))
        .unwrap_or_else(|| panic!("expected failed line in output, got: {out}"));
    assert!(
        failed_line.contains("(2.5s)"),
        "expected '(2.5s)' on failed line, got: {failed_line}"
    );
}

#[test]
fn display_plan_table_empty_phase_shows_nothing_to_do() {
    let plan = make_plan(vec![(PhaseName::Files, vec![])]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    display_plan_table(&plan, &printer, None);

    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("nothing to do"),
        "expected empty-state message, got: {out}"
    );
}

#[test]
fn display_plan_table_populated_plan_shows_phase_header() {
    let plan = make_plan(vec![(
        PhaseName::Files,
        vec![file_create("/etc/foo"), file_update("/etc/bar")],
    )]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    display_plan_table(&plan, &printer, None);

    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("Files"),
        "expected phase header in output, got: {out}"
    );
}

#[test]
fn display_plan_table_phase_filter_omits_other_phases() {
    let plan = make_plan(vec![
        (PhaseName::Files, vec![file_create("/etc/foo")]),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    display_plan_table(&plan, &printer, Some(&PhaseName::Files));

    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("Files"),
        "expected Files phase header, got: {out}"
    );
    assert!(
        !out.contains("Packages"),
        "Packages phase should be filtered out, got: {out}"
    );
}

// `display_plan_table` must condense a multi-line inline
// script's `format_plan_items` line before handing it to `bullet()` — the
// raw string returned by `format_plan_items` embeds `\n`, which would trip
// `Renderer::write_line`'s no-embedded-newline assert.
#[test]
fn display_plan_table_condenses_multiline_script_bullet() {
    let raw_body = "echo line-one\necho line-two\necho line-three";
    let action = Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple(raw_body.to_string()),
        phase: ScriptPhase::PreApply,
        origin: "test".to_string(),
    });
    let plan = make_plan(vec![(PhaseName::PreScripts, vec![action])]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    display_plan_table(&plan, &printer, None);

    let out = buf.lock().unwrap().clone();
    assert!(
        !out.contains("line-three"),
        "human bullet must condense away subsequent lines, got: {out}"
    );
    assert!(
        out.contains("line-one"),
        "condensed bullet should reference the first line, got: {out}"
    );
}

#[test]
fn display_plan_table_unknown_system_key_renders_warn() {
    // A typo'd system key (no configurator registered) must surface as a
    // real warning (⚠) at plan time, not a neutral bullet.
    let unknown = Action::System(SystemAction::Skip {
        configurator: "gti".to_string(),
        reason: "no configurator registered for 'gti'".to_string(),
        origin: "local".to_string(),
        unknown: true,
    });
    let plan = make_plan(vec![(PhaseName::System, vec![unknown])]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    display_plan_table(&plan, &printer, None);

    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains('\u{26A0}'),
        "unknown system key must warn (⚠) at plan time, got: {out}"
    );
    assert!(
        out.contains("unknown system key 'gti'"),
        "warning must name the typo'd key, got: {out}"
    );
}

#[test]
fn display_plan_table_unavailable_system_key_renders_neutral() {
    // A registered-but-unavailable configurator is expected; the plan
    // preview must render it neutrally, never as a warning.
    let plan = make_plan(vec![(PhaseName::System, vec![system_skip()])]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    display_plan_table(&plan, &printer, None);

    let out = buf.lock().unwrap().clone();
    assert!(
        !out.contains('\u{26A0}'),
        "expected platform skip must not warn (⚠), got: {out}"
    );
    assert!(
        out.contains("skip sysctl"),
        "neutral skip should still show the skip line, got: {out}"
    );
}

#[test]
fn is_unmanaged_file_missing_path_returns_false() {
    let state = StateStore::open_in_memory().unwrap();
    let config_dir = PathBuf::from("/config");
    let result = is_unmanaged_file(
        &PathBuf::from("/nonexistent/path/that/does/not/exist/abc123"),
        &config_dir,
        &state,
    );
    assert!(!result, "missing file should not be considered unmanaged");
}

#[test]
fn is_unmanaged_file_managed_path_returns_false() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("foo");
    std::fs::write(&file_path, "content").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    state
        .upsert_managed_resource("file", &file_path.display().to_string(), "test", None, None)
        .unwrap();

    let config_dir = PathBuf::from("/config");
    let result = is_unmanaged_file(&file_path, &config_dir, &state);
    assert!(
        !result,
        "state-tracked file should not be considered unmanaged"
    );
}

#[test]
fn backup_file_renames_with_cfgd_backup_suffix() {
    let tmp = tempfile::tempdir().unwrap();
    let original = tmp.path().join("myfile.txt");
    std::fs::write(&original, "original content").unwrap();

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    backup_file(&original, &printer).unwrap();

    let backup = tmp.path().join("myfile.txt.cfgd-backup");
    assert!(backup.exists(), "backup file should exist at expected path");
    assert!(
        !original.exists(),
        "original file should be gone after rename"
    );

    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("Backed up to"),
        "expected backup confirmation in output, got: {out}"
    );
    assert!(
        out.contains("cfgd-backup"),
        "output should mention backup path, got: {out}"
    );
}

#[test]
fn backup_file_nonexistent_target_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does_not_exist.txt");
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);

    let result = backup_file(&missing, &printer);
    assert!(result.is_err(), "backup of nonexistent file should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Failed to backup"),
        "error should describe backup failure, got: {err_msg}"
    );
}

#[test]
fn apply_backup_choice_backup_renames_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("target.txt");
    std::fs::write(&file, "content").unwrap();

    let mut action = file_create(file.to_str().unwrap());
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    apply_backup_choice(
        "Backup (save as .cfgd-backup, then overwrite)",
        &file,
        &mut action,
        &printer,
    )
    .unwrap();

    let backup = tmp.path().join("target.txt.cfgd-backup");
    assert!(
        backup.exists(),
        "backup file should exist after Backup choice"
    );
}

#[test]
fn apply_backup_choice_skip_converts_action_to_skip() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("target.txt");
    std::fs::write(&file, "content").unwrap();

    let mut action = file_create(file.to_str().unwrap());
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    apply_backup_choice("Skip (leave file untouched)", &file, &mut action, &printer).unwrap();

    assert!(
        matches!(action, Action::File(FileAction::Skip { .. })),
        "action should be converted to Skip after Skip choice"
    );
}

#[test]
fn apply_backup_choice_adopt_leaves_action_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("target.txt");
    std::fs::write(&file, "content").unwrap();

    let mut action = file_create(file.to_str().unwrap());
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    apply_backup_choice(
        "Adopt (overwrite with cfgd-managed version)",
        &file,
        &mut action,
        &printer,
    )
    .unwrap();

    assert!(
        matches!(action, Action::File(FileAction::Create { .. })),
        "action should remain Create after Adopt choice"
    );
}

// --- Shell environment reminder ---

fn env_apply_result(descriptions: &[&str]) -> ApplyResult {
    ApplyResult {
        action_results: descriptions
            .iter()
            .map(|d| ActionResult {
                phase: "env".to_string(),
                description: (*d).to_string(),
                success: true,
                error: None,
                changed: !d.ends_with(":skipped"),
            })
            .collect(),
        status: ApplyStatus::Success,
        apply_id: 0,
        aborted: None,
        planned_total: descriptions.len(),
    }
}

#[test]
fn shell_env_reminder_silent_when_all_env_actions_skipped() {
    let result = env_apply_result(&[
        "env:write:/home/u/.cfgd.env:skipped",
        "env:inject:/home/u/.bashrc:skipped",
        "env:session:refresh:skipped",
    ]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    print_shell_env_reminder(&result, &printer);

    let out = buf.lock().unwrap().clone();
    assert!(
        out.is_empty(),
        "an apply that changed no env surface must not nag: {out}"
    );
}

#[test]
#[serial_test::serial]
fn shell_env_reminder_names_the_written_env_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, home) = cfgd_core::with_test_home(tmp.path(), || {
        let home = cfgd_core::to_posix_string(cfgd_core::expand_tilde(std::path::Path::new("~")));
        let result = env_apply_result(&[
            format!("env:write:{home}/.cfgd.env").as_str(),
            "env:inject:/home/u/.bashrc:skipped",
        ]);
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        print_shell_env_reminder(&result, &printer);
        let out = cfgd_core::output::strip_ansi(&buf.lock().unwrap());
        (out, home)
    });

    assert!(
        !home.is_empty() && home != "~",
        "the test home must resolve to a real sandbox path, got: {home}"
    );
    assert!(
        out.contains("Shell environment changed"),
        "expected reminder heading, got: {out}"
    );
    assert!(
        out.contains("- run: source ~/.cfgd.env"),
        "expected a retypeable source command, got: {out}"
    );
    assert!(
        out.contains("- or open a new shell"),
        "expected the new-shell alternative, got: {out}"
    );
}

#[test]
#[serial_test::serial]
fn shell_env_reminder_picks_the_env_file_by_shell_not_by_emission_order() {
    // The env engine emits the PowerShell file BEFORE the Git Bash one, so a
    // first-match-wins pick would name `.cfgd-env.ps1` here. The shell is pinned
    // rather than inherited: the pick reads ambient MSYSTEM/SHELL, so on Windows
    // the same property holds or fails purely on how the runner was launched —
    // Git Bash in CI, `cmd /c` on a bare console.
    let _msys = cfgd_core::test_helpers::EnvVarGuard::set("MSYSTEM", "MINGW64");
    let tmp = tempfile::tempdir().unwrap();
    let out = cfgd_core::with_test_home(tmp.path(), || {
        let result = env_apply_result(&[
            "env:write:/home/u/.cfgd-env.ps1",
            "env:write:/home/u/.cfgd.env",
        ]);
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        print_shell_env_reminder(&result, &printer);
        cfgd_core::output::strip_ansi(&buf.lock().unwrap())
    });

    assert!(
        out.contains("- run: source /home/u/.cfgd.env"),
        "expected the shell-matching file, got: {out}"
    );
    assert!(
        !out.contains(".cfgd-env.ps1"),
        "must not name a file this shell cannot source: {out}"
    );
}

#[test]
fn preferred_env_file_follows_the_running_shell_on_windows() {
    let cases: [(bool, Option<&str>, Option<&str>, &str); 7] = [
        // POSIX hosts have exactly one env file, whatever `SHELL` says.
        (false, None, Some("/bin/bash"), ".cfgd.env"),
        (false, None, None, ".cfgd.env"),
        // Windows with no POSIX-shell marker: PowerShell is the shell in use.
        (true, None, None, ".cfgd-env.ps1"),
        (true, Some(""), Some("cmd.exe"), ".cfgd-env.ps1"),
        // MSYSTEM is exported by every MSYS2 / Git Bash shell.
        (true, Some("MINGW64"), None, ".cfgd.env"),
        (true, Some("CLANG64"), Some("powershell.exe"), ".cfgd.env"),
        // A POSIX shell reached some other way still reads SHELL.
        (
            true,
            None,
            Some(r"C:\Program Files\Git\usr\bin\bash.exe"),
            ".cfgd.env",
        ),
    ];
    for (windows, msystem, shell, want) in cases {
        let got = preferred_env_file(windows, msystem, shell);
        assert_eq!(
            got, want,
            "windows={windows} msystem={msystem:?} shell={shell:?}"
        );
    }
}

#[test]
fn shell_env_reminder_fires_for_source_line_injection_alone() {
    let result = env_apply_result(&[
        "env:write:/home/u/.cfgd.env:skipped",
        "env:inject:/home/u/.bashrc",
    ]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    print_shell_env_reminder(&result, &printer);

    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("Shell environment changed"),
        "an rc file that only just learned to source the env file still leaves \
         the running shell stale: {out}"
    );
}

#[test]
fn shell_env_reminder_absent_under_structured_output() {
    let result = env_apply_result(&["env:write:/home/u/.cfgd.env"]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Quiet);
    print_shell_env_reminder(&result, &printer);

    let out = buf.lock().unwrap().clone();
    assert!(
        out.is_empty(),
        "structured output auto-quiets; the reminder must not corrupt it: {out}"
    );
}

#[test]
fn scoped_phase_bullet_drops_the_module_name_its_heading_already_carries() {
    let phase = scoped_phase(
        "dev-tools",
        ModuleSection::PostScripts,
        vec![module_run_script()],
    );
    let items = reconciler::format_plan_items(&phase);
    assert!(
        items[0].starts_with("[dev-tools] "),
        "fixture must carry the prefix under test; got: {:?}",
        items[0]
    );

    let shown = reconciler::display_action_desc_in_phase(
        &phase.actions[0],
        &items[0],
        phase.scope.as_ref(),
    );
    assert_eq!(shown, "postApply: install.sh");
}

#[test]
fn unscoped_phase_bullet_keeps_the_module_name() {
    let phase = Phase {
        name: PhaseName::Modules,
        actions: vec![module_install()],
        scope: None,
    };
    let items = reconciler::format_plan_items(&phase);
    let shown = reconciler::display_action_desc_in_phase(&phase.actions[0], &items[0], None);
    assert_eq!(shown, items[0], "an unscoped heading names no module");
    assert!(shown.starts_with("[dev-tools] "));
}

#[test]
fn a_bullet_for_a_different_module_keeps_its_own_prefix() {
    // Splitting is by consecutive run, so a phase's scope and an action's
    // module always agree today. Stripping by exact match rather than by
    // "leading bracket group" keeps a future mismatch visible instead of
    // silently erasing the one word that would explain it.
    let phase = scoped_phase(
        "other-module",
        ModuleSection::Packages,
        vec![module_install()],
    );
    let items = reconciler::format_plan_items(&phase);
    let shown = reconciler::display_action_desc_in_phase(
        &phase.actions[0],
        &items[0],
        phase.scope.as_ref(),
    );
    assert_eq!(shown, items[0]);
}
