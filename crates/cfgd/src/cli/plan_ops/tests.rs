use std::path::PathBuf;

use cfgd_core::config::{FileStrategy, ScriptEntry};
use cfgd_core::output::{Printer, Verbosity};
use cfgd_core::providers::{FileAction, PackageAction, SecretAction};
use cfgd_core::reconciler::ActionResult;
use cfgd_core::reconciler::ApplyResult;
use cfgd_core::reconciler::{
    Action, EnvAction, ModuleAction, ModuleActionKind, Owner, Phase, PhaseName, Plan, ScriptAction,
    ScriptPhase, SystemAction,
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
        patch: None,
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
        patch: None,
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
            .map(|(name, actions)| Phase::from_actions(name, &Owner::profile("test"), actions))
            .collect(),
        warnings: vec![],
    }
}

/// A phase holding module-owned work, grouped exactly as `Reconciler::plan`
/// would group it — one group per module, in `Owner::sort_key` order.
fn module_phase(name: PhaseName, actions: Vec<Action>) -> Phase {
    Phase::from_actions(name, &Owner::profile("test"), actions)
}

fn make_plan_from_phases(phases: Vec<Phase>) -> Plan {
    Plan {
        phases,
        warnings: vec![],
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
                    patch: None,
                },
                cfgd_core::modules::ResolvedFile {
                    source: PathBuf::from("/m/.vimrc"),
                    target: PathBuf::from("/home/user/.vimrc"),
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
    let path = action_path(&PhaseName::Packages, &module_install());
    assert_eq!(
        path, "packages.module:dev-tools",
        "the owner gets its own segment so a module named `brew` cannot collide with the manager"
    );
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
    filter_plan(
        &mut plan,
        &[],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );
    assert_eq!(plan.phases[0].action_count(), 2);
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
    filter_plan(
        &mut plan,
        &["files".to_string()],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

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
        pkg_phase.action_count(),
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
    filter_plan(
        &mut plan,
        &[],
        &["packages".to_string()],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

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
        pkg_phase.action_count(),
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
    filter_plan(
        &mut plan,
        &["packages.brew.rg".to_string()],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let phase = &plan.phases[0];
    assert_eq!(phase.action_count(), 1);
    if let Action::Package(PackageAction::Install { packages, .. }) =
        phase.actions().next().expect("one action")
    {
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
    filter_plan(
        &mut plan,
        &[],
        &["packages.brew.rg".to_string()],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let phase = &plan.phases[0];
    assert_eq!(phase.action_count(), 1);
    if let Action::Package(PackageAction::Install { packages, .. }) =
        phase.actions().next().expect("one action")
    {
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
    filter_plan(
        &mut plan,
        &["packages.brew".to_string()],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let phase = &plan.phases[0];
    assert_eq!(
        phase.action_count(),
        1,
        "brew install should be fully removed"
    );
    if let Action::Package(PackageAction::Install { manager, .. }) =
        phase.actions().next().expect("one action")
    {
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
    filter_plan(
        &mut plan,
        &[],
        &["packages.brew".to_string()],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let phase = &plan.phases[0];
    assert_eq!(phase.action_count(), 1, "only brew install should remain");
    if let Action::Package(PackageAction::Install { manager, .. }) =
        phase.actions().next().expect("one action")
    {
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
    filter_plan(
        &mut plan,
        &["packages.brew.old-tool".to_string()],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    if let Action::Package(PackageAction::Uninstall { packages, .. }) =
        plan.phases[0].actions().next().expect("one action")
    {
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
    // Module work is routed by kind, so a module's install, script and file
    // deploys sit in three different phases.
    let mut plan = make_plan_from_phases(vec![
        module_phase(PhaseName::Packages, vec![module_install()]),
        module_phase(PhaseName::PostScripts, vec![module_run_script()]),
        module_phase(PhaseName::Files, vec![module_deploy_files()]),
    ]);
    strip_scripts_from_plan(&mut plan);

    assert!(
        plan.phases.iter().all(|p| p.name != PhaseName::PostScripts),
        "the Post-Scripts phase held only a RunScript action, so it must be \
         dropped entirely, not left as an empty phase"
    );
    assert_eq!(
        plan.phases.len(),
        2,
        "only the Packages and Files phases should remain"
    );
    for phase in &plan.phases {
        assert!(
            phase.actions().all(|a| {
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

// Without the trailing `plan.phases.retain(|p| !p.is_empty())` in
// `strip_scripts_from_plan`, a phase whose every action was a RunScript empties
// out but the `Phase` itself stays in `plan.phases`, so `plan.phases.is_empty()`
// is false and this assertion fails.
#[test]
fn strip_scripts_drops_a_phase_left_entirely_empty() {
    let mut plan = make_plan_from_phases(vec![module_phase(
        PhaseName::PostScripts,
        vec![module_run_script()],
    )]);
    strip_scripts_from_plan(&mut plan);

    assert!(
        plan.phases.is_empty(),
        "a phase whose only action was stripped must not survive empty: {:?}",
        plan.phases
    );
}

// The `filter_plan` side: a `--skip` pattern can exclude every action in one
// phase without touching the same module's work in another. Without the trailing
// `plan.phases.retain(|p| !p.is_empty())` in `filter_plan`, the phase's groups
// empty out but the `Phase` itself stays in `plan.phases`, so
// `plan.phases.is_empty()` is false and this assertion fails.
#[test]
fn filter_plan_drops_a_phase_left_entirely_empty() {
    let module_pkg_install = Action::Module(ModuleAction {
        module_name: "nvim".to_string(),
        kind: ModuleActionKind::InstallPackages { resolved: vec![] },
        origin: None,
    });
    let mut plan = make_plan_from_phases(vec![module_phase(
        PhaseName::Packages,
        vec![module_pkg_install],
    )]);
    filter_plan(
        &mut plan,
        &["modules.nvim".to_string()],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

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
    let output = build_plan_output(&plan, "my-machine", None, &[]);

    assert_eq!(output.context, "my-machine");
    assert_eq!(output.total_actions, 3);
    assert_eq!(output.phases.len(), 2);
    let files_phase = output.phases.iter().find(|p| p.phase == "Files").unwrap();
    let files_actions = phase_actions(files_phase);
    assert_eq!(files_actions.len(), 2);
    assert!(
        files_actions.iter().any(|a| a.action_type == "create"),
        "expected create action type"
    );
    assert!(
        files_actions.iter().any(|a| a.action_type == "update"),
        "expected update action type"
    );
}

/// Every action a phase holds, flattened across its owner groups — for the
/// assertions that are about the action set rather than about the grouping.
fn phase_actions(phase: &PlanPhaseOutput) -> Vec<&PlanActionOutput> {
    phase.groups.iter().flat_map(|g| &g.actions).collect()
}

/// [`phase_actions`] on the serialized wire form.
fn json_phase_actions(json: &serde_json::Value, phase: usize) -> Vec<&serde_json::Value> {
    json["phases"][phase]["groups"]
        .as_array()
        .expect("groups is an array")
        .iter()
        .flat_map(|g| g["actions"].as_array().expect("actions is an array"))
        .collect()
}

#[test]
fn build_plan_output_phase_filter_excludes_other_phases() {
    let plan = make_plan(vec![
        (PhaseName::Files, vec![file_create("/etc/foo")]),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let output = build_plan_output(
        &plan,
        "ctx",
        Some(&PhaseFilter::Phase(PhaseName::Files)),
        &[],
    );

    assert_eq!(output.phases.len(), 1);
    assert_eq!(output.phases[0].phase, "Files");
    assert_eq!(output.total_actions, 1);
}

#[test]
fn build_plan_output_names_the_kind_phase_and_carries_the_module_as_an_owner() {
    let plan = make_plan_from_phases(vec![module_phase(
        PhaseName::PostScripts,
        vec![module_run_script()],
    )]);
    let output = build_plan_output(&plan, "ctx", None, &[]);

    assert_eq!(output.phases.len(), 1);
    assert_eq!(output.phases[0].phase, "Post-Scripts");

    let json = serde_json::to_value(&output).unwrap();
    let phase = &json["phases"][0];
    assert!(
        phase.get("module").is_none() && phase.get("section").is_none(),
        "a phase is no longer scoped to one module: {phase}"
    );
    // The module identity the removed `module` key used to carry now rides on
    // the group that owns the action, where it also names the action's owner.
    assert_eq!(
        phase["groups"][0]["owner"],
        serde_json::json!({"kind": "module", "name": "dev-tools"}),
        "the module owns its group: {phase}"
    );
    assert_eq!(
        phase["groups"][0]["token"],
        serde_json::json!("module:dev-tools")
    );
}

#[test]
fn build_plan_output_orders_groups_profile_first() {
    // The payload's group order IS `Owner::sort_key`'s, so a consumer that
    // renders the payload reproduces the tree's ordering. The actions are
    // handed to the phase module-first to prove the order is the comparator's
    // rather than insertion order's.
    let plan = make_plan_from_phases(vec![Phase::from_actions(
        PhaseName::Packages,
        &Owner::profile("work"),
        vec![
            module_install(),
            pkg_bootstrap(),
            pkg_install("apt", vec!["sl"]),
        ],
    )]);
    let output = build_plan_output(&plan, "ctx", None, &[]);

    assert_eq!(
        output.phases[0]
            .groups
            .iter()
            .map(|g| g.token.as_str())
            .collect::<Vec<_>>(),
        vec!["profile:work", "cfgd:managers", "module:dev-tools"],
    );
    for group in &output.phases[0].groups {
        assert_eq!(
            group.token,
            group.owner.token(),
            "token must be the group owner's own rendering"
        );
    }
}

#[test]
fn no_bootstrap_means_no_managers_group_in_the_payload() {
    // The `cfgd:managers` group exists only where a bootstrap does: a plan of
    // ordinary installs leaves the payload with the profile's group alone, so
    // a consumer never sees an empty manager group to special-case.
    let plan = make_plan(vec![(
        PhaseName::Packages,
        vec![pkg_install("apt", vec!["sl"])],
    )]);
    let output = build_plan_output(&plan, "ctx", None, &[]);

    assert_eq!(
        output.phases[0]
            .groups
            .iter()
            .map(|g| g.token.as_str())
            .collect::<Vec<_>>(),
        vec!["profile:test"],
    );
    let json = serde_json::to_value(&output).unwrap();
    assert!(
        !json.to_string().contains("cfgd:managers"),
        "no bootstrap planned, so no managers group anywhere in the payload: {json}"
    );
}

#[test]
fn build_plan_output_non_module_phase_omits_module_and_section_keys() {
    let plan = make_plan(vec![(PhaseName::Files, vec![file_create("/etc/foo")])]);
    let output = build_plan_output(&plan, "ctx", None, &[]);

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
    let output = build_plan_output(&plan, "ctx", None, &[]);

    let actions = phase_actions(&output.phases[0]);
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
    let acts = json_phase_actions(&json, 0);
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
    let output = build_plan_output(&plan, "ctx", None, &[]);
    for phase in &output.phases {
        for action in phase_actions(phase) {
            assert_eq!(action.origin, None, "local plan must carry no origin");
            assert!(
                !action.description.contains(" <- "),
                "local plan must carry no provenance suffix"
            );
        }
    }
    let json = serde_json::to_value(&output).unwrap();
    let acts = json_phase_actions(&json, 0);
    assert!(
        acts.iter().all(|a| a.get("origin").is_none()),
        "no origin key expected in local-only json: {json}"
    );
}

#[test]
fn build_plan_output_empty_plan_has_zero_actions() {
    let plan = make_plan(vec![]);
    let output = build_plan_output(&plan, "ctx", None, &[]);

    assert_eq!(output.total_actions, 0);
    assert!(output.phases.is_empty());
}

// `build_plan_output`'s `PlanActionOutput.description` is the
// `-o json` plan payload — it must preserve a multi-line inline script's
// run_str body byte-identical, never condensed. Condensing belongs solely to
// the human render site (`ApplyRun::preview`).
#[test]
fn build_plan_output_script_action_json_preserves_raw_multiline_body() {
    let raw_body = "echo line-one\necho line-two\necho line-three";
    let action = Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple(raw_body.to_string()),
        phase: ScriptPhase::PreApply,
        origin: "test".to_string(),
    });
    let plan = make_plan(vec![(PhaseName::PreScripts, vec![action])]);
    let output = build_plan_output(&plan, "ctx", None, &[]);

    let desc = &output.phases[0].groups[0].actions[0].description;
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
    let output = build_plan_output(&plan, "ctx", None, &[]);

    let desc = &output.phases[0].groups[0].actions[0].description;
    assert!(
        desc.contains(raw_body),
        "PlanActionOutput.description must preserve a MODULE script's raw multi-line body byte-identical, got: {desc}"
    );
}

#[test]
fn render_plan_tree_populated_plan_shows_phase_header() {
    let plan = make_plan(vec![(
        PhaseName::Files,
        vec![file_create("/etc/foo"), file_update("/etc/bar")],
    )]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    reconciler::render_plan_tree(&plan, None, &printer);

    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("Files"),
        "expected phase header in output, got: {out}"
    );
}

// `render_plan_tree` must condense a multi-line inline
// script's `format_plan_items` line before handing it to `bullet()` — the
// raw string returned by `format_plan_items` embeds `\n`, which would trip
// `Renderer::write_line`'s no-embedded-newline assert.
#[test]
fn render_plan_tree_condenses_multiline_script_bullet() {
    let raw_body = "echo line-one\necho line-two\necho line-three";
    let action = Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple(raw_body.to_string()),
        phase: ScriptPhase::PreApply,
        origin: "test".to_string(),
    });
    let plan = make_plan(vec![(PhaseName::PreScripts, vec![action])]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    reconciler::render_plan_tree(&plan, None, &printer);

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
fn render_plan_tree_unknown_system_key_renders_warn() {
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
    reconciler::render_plan_tree(&plan, None, &printer);

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
fn render_plan_tree_unavailable_system_key_renders_neutral() {
    // A registered-but-unavailable configurator is expected; the plan
    // preview must render it neutrally, never as a warning.
    let plan = make_plan(vec![(PhaseName::System, vec![system_skip()])]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    reconciler::render_plan_tree(&plan, None, &printer);

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

// --- unmanaged-file prompt: Patch adopts in place ---

fn patch_update(target: &Path) -> Action {
    Action::File(FileAction::Update {
        source: PathBuf::new(),
        target: target.to_path_buf(),
        diff: "--- old\n+++ new\n".to_string(),
        origin: "test".to_string(),
        strategy: FileStrategy::Patch,
        source_hash: None,
        patch: Some(cfgd_core::config::PatchSpec {
            format: Some(cfgd_core::config::PatchFormat::Json),
            ensure: Some(serde_yaml::from_str("telemetry: false").unwrap()),
            script: None,
            blocked_by: None,
        }),
    })
}

fn copy_update(target: &Path) -> Action {
    Action::File(FileAction::Update {
        source: PathBuf::from("/src/dotfiles/.zshrc"),
        target: target.to_path_buf(),
        diff: "--- old\n+++ new\n".to_string(),
        origin: "test".to_string(),
        strategy: FileStrategy::Copy,
        source_hash: None,
        patch: None,
    })
}

fn one_phase_plan(actions: Vec<Action>) -> Plan {
    make_plan(vec![(PhaseName::Files, actions)])
}

#[test]
fn unmanaged_prompt_never_backs_up_a_patch_target() {
    // A `Patch` target is unmanaged by definition on the first apply. Renaming
    // it away would make the merge read empty content and write only the
    // ensured keys — destroying the content the strategy exists to preserve.
    let tmp = tempfile::tempdir().unwrap();
    let patch_target = tmp.path().join("settings.json");
    let copy_target = tmp.path().join("zshrc");
    std::fs::write(&patch_target, "{\n  \"runtimeToken\": \"keep-me\"\n}\n").unwrap();
    std::fs::write(&copy_target, "old").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _cap) =
        Printer::for_test_doc_with_prompt_responses(vec![cfgd_core::output::PromptAnswer::Select(
            "Backup (save as .cfgd-backup, then overwrite)".into(),
        )]);
    let mut plan = one_phase_plan(vec![patch_update(&patch_target), copy_update(&copy_target)]);

    handle_unmanaged_file_targets(&mut plan, tmp.path(), &state, &printer, false).unwrap();

    assert!(
        patch_target.exists(),
        "a Patch target must stay in place for the merge to read"
    );
    assert_eq!(
        std::fs::read_to_string(&patch_target).unwrap(),
        "{\n  \"runtimeToken\": \"keep-me\"\n}\n",
        "a Patch target must not be touched by the unmanaged-file prompt"
    );
    assert!(
        !tmp.path().join("settings.json.cfgd-backup").exists(),
        "no sidecar should be created for a Patch target"
    );
    // The single queued answer went to the non-Patch action, proving the
    // Patch one never prompted.
    assert!(
        tmp.path().join("zshrc.cfgd-backup").exists(),
        "a Copy target still honours the Backup choice"
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
fn unmanaged_prompt_skips_patch_module_files() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("hosts");
    std::fs::write(&target, "127.0.0.1 localhost\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _cap) =
        Printer::for_test_doc_with_prompt_responses(vec![cfgd_core::output::PromptAnswer::Select(
            "Backup (save as .cfgd-backup, then overwrite)".into(),
        )]);
    let file = cfgd_core::modules::ResolvedFile {
        source: PathBuf::new(),
        target: target.clone(),
        is_git_source: false,
        strategy: Some(FileStrategy::Patch),
        permissions: None,
        encryption: None,
        patch: Some(cfgd_core::config::PatchSpec {
            format: Some(cfgd_core::config::PatchFormat::Ini),
            ensure: Some(serde_yaml::from_str("core:\n  editor: vim").unwrap()),
            script: None,
            blocked_by: None,
        }),
    };
    let mut plan = one_phase_plan(vec![Action::Module(ModuleAction::local(
        "mymod".to_string(),
        ModuleActionKind::DeployFiles { files: vec![file] },
    ))]);

    handle_unmanaged_file_targets(&mut plan, tmp.path(), &state, &printer, false).unwrap();

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "127.0.0.1 localhost\n",
        "a module-deployed Patch target must not be renamed away"
    );
    assert!(
        !tmp.path().join("hosts.cfgd-backup").exists(),
        "no sidecar should be created for a module Patch target"
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

// --- script-package gate, owner grammar, and stranded installs ---

fn resolved_package(manager: &str, name: &str) -> cfgd_core::modules::ResolvedPackage {
    cfgd_core::modules::ResolvedPackage {
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

fn module_batch(module: &str, resolved: Vec<cfgd_core::modules::ResolvedPackage>) -> Action {
    Action::Module(ModuleAction {
        module_name: module.to_string(),
        kind: ModuleActionKind::InstallPackages { resolved },
        origin: None,
    })
}

fn module_named(module: &str) -> Action {
    module_batch(module, vec![resolved_package("brew", "neovim")])
}

/// A package manager that is present on the host, for the arm of the
/// stranded-install warning that must stay silent.
struct AvailableManager(&'static str);

impl cfgd_core::providers::PackageManager for AvailableManager {
    fn name(&self) -> &str {
        self.0
    }
    fn is_available(&self) -> bool {
        true
    }
    fn can_bootstrap(&self) -> bool {
        false
    }
    fn bootstrap(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> cfgd_core::errors::Result<std::collections::HashSet<String>> {
        Ok(std::collections::HashSet::new())
    }
    fn install(
        &self,
        _packages: &[String],
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn uninstall(
        &self,
        _packages: &[String],
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn update(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> cfgd_core::errors::Result<Option<String>> {
        Ok(None)
    }
}

/// The plan the stranded-install warning is derived from: one bootstrap plus
/// two installs that need the manager it would have provided, one of them
/// through a sub-manager that has no bootstrap of its own.
fn brew_bootstrap_plan() -> Plan {
    make_plan(vec![(
        PhaseName::Packages,
        vec![
            pkg_bootstrap(),
            pkg_install("brew", vec!["ripgrep"]),
            pkg_install("brew-tap", vec!["homebrew/cask"]),
        ],
    )])
}

#[test]
fn mixed_manager_batch_with_trailing_script_is_stripped() {
    // One action, one rendered line, and no shape in the plan model that could
    // execute half of it — so a batch holding any script entry goes whole.
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![module_batch(
            "nvim",
            vec![
                resolved_package("brew", "neovim"),
                resolved_package("script", "tree-sitter"),
            ],
        )],
    )]);
    strip_scripts_from_plan(&mut plan);

    assert!(
        plan.phases.is_empty(),
        "the batch's script entry must strip the whole action: {:?}",
        plan.phases
    );
}

#[test]
fn skip_scripts_strips_module_and_profile_script_packages_alike() {
    // The gate classifies by action shape, so re-routing a module's install
    // into `Packages` beside the profile's cannot resurrect either.
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![
            module_batch("nvim", vec![resolved_package("script", "tree-sitter")]),
            pkg_install("script", vec!["custom-tool"]),
            pkg_install("brew", vec!["ripgrep"]),
        ],
    )]);
    strip_scripts_from_plan(&mut plan);

    assert_eq!(
        plan.phases.iter().map(Phase::action_count).sum::<usize>(),
        1,
        "only the brew install survives: {:?}",
        plan.phases
    );
}

#[test]
fn action_path_folds_a_windows_path_to_posix() {
    let action = file_create(r"C:\Users\u\.gitconfig");
    assert_eq!(
        action_path(&PhaseName::Files, &action),
        "files:C:/Users/u/.gitconfig",
        "a pattern authored on one OS must select the same action on the other"
    );
}

#[test]
fn action_path_module_carries_its_owner_segment_in_every_phase() {
    assert_eq!(
        action_path(&PhaseName::Files, &module_deploy_files()),
        "files.module:dotfiles"
    );
    assert_eq!(
        action_path(&PhaseName::PostScripts, &module_run_script()),
        "post-scripts.module:dev-tools"
    );
}

#[test]
fn skip_owner_pattern_selects_one_module_across_every_phase() {
    let mut plan = make_plan(vec![
        (
            PhaseName::Packages,
            vec![module_named("nvim"), pkg_install("brew", vec!["ripgrep"])],
        ),
        (PhaseName::Files, vec![module_deploy_files()]),
    ]);
    filter_plan(
        &mut plan,
        &["module:nvim".to_string()],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let owners: Vec<String> = plan
        .phases
        .iter()
        .flat_map(|p| p.groups().iter().map(|g| g.owner.token()))
        .collect();
    assert_eq!(
        owners,
        vec!["profile:test", "module:dotfiles"],
        "only the named module is dropped, and it is dropped in every phase"
    );
}

#[test]
fn skip_owner_pattern_selects_the_profile() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![module_named("nvim"), pkg_install("brew", vec!["ripgrep"])],
    )]);
    filter_plan(
        &mut plan,
        &["profile:test".to_string()],
        &[],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let owners: Vec<String> = plan
        .phases
        .iter()
        .flat_map(|p| p.groups().iter().map(|g| g.owner.token()))
        .collect();
    assert_eq!(owners, vec!["module:nvim"]);
}

#[test]
fn legacy_modules_pattern_still_skips_and_says_so() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![module_named("nvim"), pkg_install("brew", vec!["ripgrep"])],
    )]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["modules.nvim".to_string()],
        &[],
        &printer,
        &ProviderRegistry::new(),
    );
    printer.flush();
    let out = buf.lock().unwrap().clone();

    let owners: Vec<String> = plan
        .phases
        .iter()
        .flat_map(|p| p.groups().iter().map(|g| g.owner.token()))
        .collect();
    assert_eq!(owners, vec!["profile:test"], "the pattern still works");
    assert!(
        out.contains("deprecated") && out.contains("module:nvim"),
        "the run must name the replacement spelling, got: {out}"
    );
}

#[test]
fn only_packages_brew_does_not_match_a_module_named_brew() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![module_named("brew"), pkg_install("brew", vec!["ripgrep"])],
    )]);
    filter_plan(
        &mut plan,
        &[],
        &["packages.brew".to_string()],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let owners: Vec<String> = plan
        .phases
        .iter()
        .flat_map(|p| p.groups().iter().map(|g| g.owner.token()))
        .collect();
    assert_eq!(
        owners,
        vec!["profile:test"],
        "the manager segment and the owner segment are different namespaces"
    );
}

#[test]
fn only_packages_module_brew_selects_the_module_not_the_manager() {
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![module_named("brew"), pkg_install("brew", vec!["ripgrep"])],
    )]);
    filter_plan(
        &mut plan,
        &[],
        &["packages.module:brew".to_string()],
        &Printer::for_test().0,
        &ProviderRegistry::new(),
    );

    let owners: Vec<String> = plan
        .phases
        .iter()
        .flat_map(|p| p.groups().iter().map(|g| g.owner.token()))
        .collect();
    assert_eq!(owners, vec!["module:brew"]);
}

#[test]
fn skip_cfgd_managers_warns_once_about_stranded_installs() {
    let mut plan = brew_bootstrap_plan();
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["cfgd:managers".to_string()],
        &[],
        &printer,
        &ProviderRegistry::new(),
    );
    printer.flush();
    let out = buf.lock().unwrap().clone();

    assert!(
        !plan
            .phases
            .iter()
            .flat_map(|p| p.actions())
            .any(|a| matches!(a, Action::Package(PackageAction::Bootstrap { .. }))),
        "the bootstrap is gone, which is what strands the installs"
    );
    assert_eq!(
        out.matches("bootstrap(s)").count(),
        1,
        "one warning per run, not one per stranded manager: {out}"
    );
    assert!(out.contains("--skip packages.brew"), "got: {out}");
    assert!(out.contains("--skip packages.brew-tap"), "got: {out}");
    assert!(
        out.contains("`--skip cfgd:managers`"),
        "the warning names the pattern responsible: {out}"
    );
}

#[test]
fn skip_packages_brew_strands_the_sub_manager_it_does_not_cover() {
    // `pattern_matches`' segment boundary means `packages.brew` never covers
    // `packages.brew-tap`, so the tap install survives its parent's removal.
    let mut plan = brew_bootstrap_plan();
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["packages.brew".to_string()],
        &[],
        &printer,
        &ProviderRegistry::new(),
    );
    printer.flush();
    let out = buf.lock().unwrap().clone();

    assert!(out.contains("--skip packages.brew-tap"), "got: {out}");
    assert!(
        !out.contains("--skip packages.brew "),
        "brew's own install went with the pattern, so it is not stranded: {out}"
    );
}

#[test]
fn stranded_warning_counts_actions_not_distinct_managers() {
    // Two installs behind ONE stranded manager. The user is told how much work
    // will silently not apply, so the count is over actions; the `--skip` flags
    // stay per manager because that is what a flag can address.
    let mut plan = make_plan(vec![(
        PhaseName::Packages,
        vec![
            pkg_bootstrap(),
            pkg_install("brew", vec!["ripgrep"]),
            pkg_install("brew", vec!["fd"]),
        ],
    )]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["cfgd:managers".to_string()],
        &[],
        &printer,
        &ProviderRegistry::new(),
    );
    printer.flush();
    let out = buf.lock().unwrap().clone();

    assert!(
        out.contains("2 package action(s)"),
        "both installs are stranded even though one manager is: {out}"
    );
    assert_eq!(
        out.matches("--skip packages.brew").count(),
        1,
        "the flag list stays deduplicated by manager: {out}"
    );
}

#[test]
fn no_stranded_warning_when_every_manager_is_available() {
    let mut plan = brew_bootstrap_plan();
    let mut registry = ProviderRegistry::new();
    registry
        .package_managers
        .push(Box::new(AvailableManager("brew")));
    registry
        .package_managers
        .push(Box::new(AvailableManager("brew-tap")));
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["cfgd:managers".to_string()],
        &[],
        &printer,
        &registry,
    );
    printer.flush();
    let out = buf.lock().unwrap().clone();

    assert!(
        !out.contains("bootstrap(s)"),
        "a bootstrap dropped for a manager that is already installed strands nothing: {out}"
    );
}

#[test]
fn platform_skip_survives_in_the_plan_payload() {
    // The human tree renders a platform-gated skip as a header annotation and
    // opens no `Phase: Modules` block. That is a DISPLAY decision: the machine
    // payload keeps the phase, the action and its reason verbatim, because a
    // consumer diffing plans across hosts is exactly who needs to see that a
    // module was gated out on this one.
    let skip = Action::Module(ModuleAction {
        module_name: "wsl-tools".to_string(),
        kind: ModuleActionKind::Skip {
            reason: "platform not matched (requires: windows)".to_string(),
        },
        origin: None,
    });
    let plan = make_plan(vec![
        (PhaseName::Modules, vec![skip]),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let output = build_plan_output(&plan, "ctx", None, &[]);

    assert_eq!(output.total_actions, 2, "the skip is a counted action");
    let modules = output
        .phases
        .iter()
        .find(|p| p.phase == "Modules")
        .expect("the Modules phase survives in the payload");
    let modules_actions = phase_actions(modules);
    assert_eq!(modules_actions.len(), 1);
    assert_eq!(modules_actions[0].action_type, "skip");
    assert_eq!(
        modules_actions[0].description, "skip: platform not matched (requires: windows)",
        "the reason is the action's own string, byte-for-byte"
    );

    let modules_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Modules)
        .expect("the Modules phase");
    assert_eq!(
        modules_phase
            .groups()
            .iter()
            .map(|g| g.owner.token())
            .collect::<Vec<_>>(),
        vec!["module:wsl-tools".to_string()],
        "the skip keeps its module owner group"
    );
}
