use std::path::PathBuf;

use cfgd_core::config::{FileStrategy, ScriptEntry};
use cfgd_core::output::{Printer, Verbosity};
use cfgd_core::providers::{FileAction, PackageAction, SecretAction};
use cfgd_core::reconciler::ActionResult;
use cfgd_core::reconciler::ApplyResult;
use cfgd_core::reconciler::{
    Action, EnvAction, ManagerAction, ModuleAction, ModuleActionKind, Owner, Phase, PhaseName,
    Plan, ScriptAction, ScriptPhase, SystemAction,
};
use cfgd_core::state::{ApplyStatus, StateStore};

use super::*;

/// A `Cli` whose config lives in `dir`, so a [`RunContext`] built from it
/// resolves manifests against that directory.
fn test_cli_in(dir: &std::path::Path) -> Cli {
    Cli {
        config: dir.join("cfgd.yaml"),
        config_explicit: false,
        profile: None,
        verbose: 0,
        quiet: true,
        no_color: true,
        color: crate::cli::ColorWhen::Auto,
        output: crate::cli::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        list_envelope: false,
        jsonpath: None,
        state_dir: None,
        config_dir: None,
        cache_dir: None,
        runtime_dir: None,
        scope_arg: crate::cli::ScopeArg::User,
        command: None,
    }
}

/// A plan built with nothing withheld — the shape every payload test but the
/// decision ones asserts against.
fn no_decisions() -> reconciler::WithheldDecisions {
    reconciler::WithheldDecisions::default()
}

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
    assert_eq!(action_type_str(&pkg_install("brew", vec!["rg"])), "install");
    assert_eq!(
        action_type_str(&pkg_uninstall("brew", vec!["rg"])),
        "uninstall"
    );
    assert_eq!(action_type_str(&pkg_skip()), "skip");
}

#[test]
fn action_type_str_manager_variants() {
    assert_eq!(
        action_type_str(&Action::Manager(ManagerAction::RefreshIndex {
            manager: "brew".to_string(),
        })),
        "refresh"
    );
    assert_eq!(
        action_type_str(&Action::Manager(ManagerAction::Provision {
            manager: "brew".to_string(),
            via: "homebrew installer".to_string(),
            batched: vec![],
            depends_on: vec![],
        })),
        "provision"
    );
    assert_eq!(
        action_type_str(&Action::Manager(ManagerAction::Prerequisite {
            tool: "xcode-select".to_string(),
            installer: "xcode-select --install".to_string(),
            required_by: vec!["brew".to_string()],
            depends_on: vec![],
        })),
        "prerequisite"
    );
    assert_eq!(
        action_type_str(&Action::Manager(ManagerAction::Refuse {
            manager: "brew".to_string(),
            reason: "no supported installer for this platform".to_string(),
        })),
        "refuse"
    );
}

#[test]
fn manager_action_output_none_for_non_manager_action() {
    assert!(manager_action_output(&file_create("/etc/foo")).is_none());
}

#[test]
fn manager_action_output_refresh_index_is_present_with_no_via_or_requires() {
    let out = manager_action_output(&Action::Manager(ManagerAction::RefreshIndex {
        manager: "brew".to_string(),
    }))
    .expect("RefreshIndex must carry a manager payload");
    assert_eq!(out.manager, "brew");
    assert_eq!(out.state, "present");
    assert_eq!(out.via, None);
    assert!(out.requires.is_empty());
    assert_eq!(out.reason, None);
}

#[test]
fn manager_action_output_provision_carries_via_and_requires() {
    let out = manager_action_output(&Action::Manager(ManagerAction::Provision {
        manager: "pipx".to_string(),
        via: "pip install pipx".to_string(),
        batched: vec![],
        depends_on: vec!["manager:prereq:curl".to_string()],
    }))
    .expect("Provision must carry a manager payload");
    assert_eq!(out.manager, "pipx");
    assert_eq!(out.state, "provisioned");
    assert_eq!(out.via.as_deref(), Some("pip install pipx"));
    assert_eq!(out.requires, vec!["manager:prereq:curl".to_string()]);
    assert_eq!(out.reason, None);
}

#[test]
fn manager_action_output_prerequisite_names_the_tool_and_installer() {
    // manager=tool, via=installer mirrors the human line's subject/actor
    // split ("{installer} install {tool}") — the installer is the "manager"
    // command that runs, but the tool is what this row is about.
    let out = manager_action_output(&Action::Manager(ManagerAction::Prerequisite {
        tool: "curl".to_string(),
        installer: "apt".to_string(),
        required_by: vec!["brew-cask".to_string(), "pipx".to_string()],
        depends_on: vec!["manager:refresh:apt".to_string()],
    }))
    .expect("Prerequisite must carry a manager payload");
    assert_eq!(out.manager, "curl");
    assert_eq!(out.state, "prerequisite");
    assert_eq!(out.via.as_deref(), Some("apt"));
    assert_eq!(out.requires, vec!["manager:refresh:apt".to_string()]);
    assert_eq!(out.reason, None);
}

#[test]
fn manager_action_output_refuse_extends_spec_with_a_refused_state_and_reason() {
    // Spec §7's literal `state` enum is present|provisioned|prerequisite —
    // it does not name Refuse. This task's own scope names `Refuse` as a
    // node requiring a payload, so a fourth state carries the reason rather
    // than the row silently disappearing from `-o json`.
    let out = manager_action_output(&Action::Manager(ManagerAction::Refuse {
        manager: "snap".to_string(),
        reason: "no available system manager".to_string(),
    }))
    .expect("Refuse must carry a manager payload");
    assert_eq!(out.manager, "snap");
    assert_eq!(out.state, "refused");
    assert_eq!(out.via, None);
    assert!(out.requires.is_empty());
    assert_eq!(out.reason.as_deref(), Some("no available system manager"));
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
    let path = action_path(&PhaseName::Prerequisites, &env_write());
    assert_eq!(path, "prerequisites:/home/user/.cfgd.env");
}

#[test]
fn action_path_env_inject() {
    let path = action_path(&PhaseName::Prerequisites, &env_inject());
    assert_eq!(path, "prerequisites:/home/user/.zshrc");
}

/// `action_path` keys a Prerequisite node on its TOOL (`curl`), not its
/// installer (`brew`), agreeing with `reconciler::action_matches_phase_filter`'s
/// `--phase` matcher — the two independent matchers finding 3 brought back
/// into agreement.
#[test]
fn action_path_manager_prerequisite_keys_on_its_tool_not_its_installer() {
    let prereq = Action::Manager(ManagerAction::Prerequisite {
        tool: "curl".to_string(),
        installer: "brew".to_string(),
        required_by: vec!["brew".to_string()],
        depends_on: vec![],
    });
    let path = action_path(&PhaseName::Prerequisites, &prereq);
    assert_eq!(
        path, "prerequisites.curl",
        "a prerequisite's path names the tool, not the installer that provisions it"
    );
}

/// The four `--phase`/`--skip` × `prerequisites.brew`/`prerequisites.curl`
/// combinations for a curl-via-brew prerequisite node: only the TOOL spelling
/// reaches it under either flag, and the installer spelling reaches brew's
/// own provision node instead.
#[test]
fn skip_and_only_patterns_reach_a_prerequisite_by_tool_not_installer() {
    let managers_owner = reconciler::Owner::cfgd(reconciler::MANAGERS_GROUP);
    let prereq = Action::Manager(ManagerAction::Prerequisite {
        tool: "curl".to_string(),
        installer: "brew".to_string(),
        required_by: vec!["brew".to_string()],
        depends_on: vec![],
    });
    let prereq_path = action_path(&PhaseName::Prerequisites, &prereq);

    assert!(
        pattern_matches_action("prerequisites.curl", &managers_owner, &prereq_path),
        "`prerequisites.curl` (the tool) must reach the prerequisite node"
    );
    assert!(
        !pattern_matches_action("prerequisites.brew", &managers_owner, &prereq_path),
        "`prerequisites.brew` (the installer) must NOT reach the prerequisite node — \
         it names brew's own provision, a different plan node"
    );

    let brew_provision = Action::Manager(ManagerAction::Provision {
        manager: "brew".to_string(),
        via: "curl".to_string(),
        batched: vec![],
        depends_on: vec![],
    });
    let provision_path = action_path(&PhaseName::Prerequisites, &brew_provision);

    assert!(
        pattern_matches_action("prerequisites.brew", &managers_owner, &provision_path),
        "`prerequisites.brew` must still reach brew's own provision node"
    );
    assert!(
        !pattern_matches_action("prerequisites.curl", &managers_owner, &provision_path),
        "`prerequisites.curl` must not reach brew's provision node"
    );
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
fn filter_plan_honours_the_legacy_env_phase_pattern_and_says_it_is_on_the_way_out() {
    let mut plan = make_plan(vec![
        (PhaseName::Prerequisites, vec![env_write(), env_inject()]),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["env".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        !plan
            .phases
            .iter()
            .any(|p| p.name == PhaseName::Prerequisites),
        "the pre-merge spelling must still select the phase it always selected: {:?}",
        plan.phases
    );
    assert!(
        plan.phases.iter().any(|p| p.name == PhaseName::Packages),
        "and nothing else: {:?}",
        plan.phases
    );
    assert!(
        out.contains("`--skip env` is deprecated") && out.contains("--skip prerequisites"),
        "the notice must name both the spelling and its replacement:\n{out}"
    );
}

#[test]
fn filter_plan_leaves_an_owner_token_opening_with_the_legacy_word_alone() {
    let mut plan = make_plan(vec![(PhaseName::Prerequisites, vec![env_write()])]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["cfgd:env".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        plan.phases.is_empty(),
        "the owner token still selects the group it names: {:?}",
        plan.phases
    );
    assert!(
        !out.contains("deprecated"),
        "an owner token is not the phase segment and earns no notice:\n{out}"
    );
}

/// A batched provision — one apt command delivering npm and pipx — plus an
/// install for each, so `prune_to_surviving_consumers` keeps both members.
fn batched_provision_plan() -> cfgd_core::reconciler::Plan {
    make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![Action::Manager(ManagerAction::Provision {
                manager: "npm".to_string(),
                via: "apt".to_string(),
                batched: vec!["pipx".to_string()],
                depends_on: vec![],
            })],
        ),
        (
            PhaseName::Packages,
            vec![
                pkg_install("npm", vec!["prettier"]),
                pkg_install("pipx", vec!["ruff"]),
            ],
        ),
    ])
}

fn provision_lines(plan: &cfgd_core::reconciler::Plan) -> Vec<String> {
    plan.phases
        .iter()
        .flat_map(|phase| phase.actions())
        .filter(|a| matches!(a, Action::Manager(ManagerAction::Provision { .. })))
        .map(cfgd_core::reconciler::format_plan_item)
        .collect()
}

#[test]
fn skipping_one_manager_of_a_batch_leaves_the_others_provisioned() {
    // The whole point of per-member filtering: a batch is a saved command, not
    // a package deal. `--skip prerequisites.npm` must not take pipx with it.
    let mut plan = batched_provision_plan();
    let (printer, _buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["prerequisites.npm".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    assert_eq!(
        provision_lines(&plan),
        vec!["provision pipx via apt"],
        "only the manager the pattern named leaves the batch"
    );
}

#[test]
fn a_phase_selector_naming_one_batch_member_provisions_only_that_manager() {
    // `--phase` is resolved downstream as a predicate over whole actions, so a
    // batch it cannot split would provision npm as well. `filter_plan` is
    // where every selector the user supplied becomes part of the plan.
    let mut plan = batched_provision_plan();
    let (printer, _buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &[],
        &[],
        Some(&cfgd_core::reconciler::PhaseFilter::Selector(
            PhaseName::Prerequisites,
            "pipx".to_string(),
        )),
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    assert_eq!(
        provision_lines(&plan),
        vec!["provision pipx via apt"],
        "the selector narrows the batch before the predicate ever sees it"
    );
}

#[test]
fn a_batched_provision_names_every_manager_it_delivers_in_the_json_payload() {
    let out = manager_action_output(&Action::Manager(ManagerAction::Provision {
        manager: "npm".to_string(),
        via: "apt".to_string(),
        batched: vec!["pipx".to_string()],
        depends_on: vec![],
    }))
    .expect("a manager action carries a payload");
    assert_eq!(out.manager, "npm");
    assert_eq!(
        out.batched,
        vec!["pipx".to_string()],
        "a consumer reading `manager` alone would think one apt install \
         delivers only npm"
    );
}

#[test]
fn filter_plan_warns_when_a_skipped_provision_strands_the_installs_that_needed_it() {
    let mut plan = make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                batched: vec![],
                depends_on: vec![],
            })],
        ),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["prerequisites".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        out.contains("`--skip prerequisites` removes 1 bootstrap")
            && out.contains("--skip packages.brew"),
        "dropping the node that would have installed brew must name the work it strands:\n{out}"
    );
}

#[test]
fn filter_plan_skip_prerequisites_session_removes_only_the_broadcast_and_strands_nothing() {
    // The dotted group-selector grammar (`prerequisites.session`) reaches the
    // cfgd:session owner group by name via `phase_qualified_group_owner_token`
    // — a colon-joined `Action::Env` path never matches this pattern literally,
    // so this pins the alias rather than the fallback literal match. The
    // Packages phase below is load-bearing, not incidental: with no package
    // action consuming brew anywhere in the plan, `prune_to_surviving_consumers`
    // would remove brew's `Provision` node as purposeless regardless of what
    // `--skip` pattern ran, which would make this test pass or fail on a
    // mechanism unrelated to the `.session` selector it actually exercises.
    let mut plan = make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![
                Action::Manager(ManagerAction::Provision {
                    manager: "brew".to_string(),
                    via: "homebrew installer".to_string(),
                    batched: vec![],
                    depends_on: vec![],
                }),
                env_write(),
                Action::Env(EnvAction::RefreshLiveSession { vars: Vec::new() }),
            ],
        ),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["prerequisites.session".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    let prereq_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Prerequisites)
        .unwrap();
    let remaining: Vec<&Action> = prereq_phase.actions().collect();
    assert_eq!(
        remaining.len(),
        2,
        "only the session broadcast should have been removed: {remaining:?}"
    );
    assert!(
        !remaining
            .iter()
            .any(|a| matches!(a, Action::Env(EnvAction::RefreshLiveSession { .. }))),
        "the session broadcast must be gone: {remaining:?}"
    );
    assert!(
        !out.contains("removes") || !out.contains("bootstrap"),
        "skipping the broadcast half strands no package install, so no alert fires:\n{out}"
    );
}

#[test]
fn filter_plan_skip_prerequisites_managers_strands_every_manager_it_removes() {
    // The group-selector grammar reaches the WHOLE cfgd:managers owner group —
    // every registered manager's node — not one manager at a time, and each
    // BOOTSTRAP (`Provision`) it takes down strands its own consumers. Both
    // managers here are `Provision` (not `RefreshIndex`) deliberately: only a
    // `Provision` node is a bootstrap the machine is telling the user it no
    // longer creates, so only `Provision` removals feed the alert's count —
    // a `RefreshIndex` removal means the manager was already present and
    // needs no warning that it will still be there.
    let mut plan = make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![
                Action::Manager(ManagerAction::Provision {
                    manager: "brew".to_string(),
                    via: "homebrew installer".to_string(),
                    batched: vec![],
                    depends_on: vec![],
                }),
                Action::Manager(ManagerAction::Provision {
                    manager: "npm".to_string(),
                    via: "node installer".to_string(),
                    batched: vec![],
                    depends_on: vec![],
                }),
            ],
        ),
        (
            PhaseName::Packages,
            vec![
                pkg_install("brew", vec!["rg"]),
                pkg_install("npm", vec!["typescript"]),
            ],
        ),
    ]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["prerequisites.managers".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        !plan
            .phases
            .iter()
            .any(|p| p.name == PhaseName::Prerequisites),
        "both manager nodes should be gone, emptying the phase: {:?}",
        plan.phases
    );
    assert!(
        out.contains("`--skip prerequisites.managers` removes 2 bootstraps")
            && out.contains("--skip packages.brew")
            && out.contains("--skip packages.npm"),
        "the alert must name both stranded managers:\n{out}"
    );
}

#[test]
fn filter_plan_skip_prerequisites_brew_leaves_other_managers_untouched() {
    // The literal manager-name selector already worked with zero new code,
    // because `action_path` for a Manager node was already dot-joined as
    // `<phase>.<manager>` and sub-managers are family-collapsed at plan time.
    // Pinned here as a regression guard for the dotted grammar's manager arm.
    let mut plan = make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![
                Action::Manager(ManagerAction::Provision {
                    manager: "brew".to_string(),
                    via: "homebrew installer".to_string(),
                    batched: vec![],
                    depends_on: vec![],
                }),
                Action::Manager(ManagerAction::RefreshIndex {
                    manager: "npm".to_string(),
                }),
            ],
        ),
        (
            PhaseName::Packages,
            vec![
                pkg_install("brew", vec!["rg"]),
                pkg_install("npm", vec!["typescript"]),
            ],
        ),
    ]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["prerequisites.brew".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    let prereq_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Prerequisites)
        .unwrap();
    let remaining: Vec<&Action> = prereq_phase.actions().collect();
    assert_eq!(
        remaining.len(),
        1,
        "only brew's node should be removed, npm's must survive: {remaining:?}"
    );
    assert!(
        matches!(
            remaining[0],
            Action::Manager(ManagerAction::RefreshIndex { manager }) if manager == "npm"
        ),
        "the surviving node must be npm's: {remaining:?}"
    );
    assert!(
        out.contains("`--skip prerequisites.brew` removes 1 bootstrap")
            && out.contains("--skip packages.brew")
            && !out.contains("--skip packages.npm"),
        "the alert must name only the manager the pattern actually removed:\n{out}"
    );
}

#[test]
fn filter_plan_skip_last_package_consumer_silently_prunes_its_now_purposeless_manager_node() {
    // Distinct from skipping the MANAGER: skipping the CONSUMERS that were
    // its last reason to run prunes the manager's Provision node too, via
    // `prune_to_surviving_consumers` — but silently, since a refresh with
    // zero surviving consumers is the machine's own bookkeeping, not a
    // stranding the user needs to be told about.
    let mut plan = make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                batched: vec![],
                depends_on: vec![],
            })],
        ),
        (PhaseName::Packages, vec![pkg_install("brew", vec!["rg"])]),
    ]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["packages.brew.rg".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        plan.phases.is_empty(),
        "the package was brew's last consumer, so its Provision node is now \
         purposeless and the plan should be empty: {:?}",
        plan.phases
    );
    assert!(
        !out.contains("removes") || !out.contains("bootstrap"),
        "a consumer-side skip prunes its manager silently, never through the \
         stranded-install alert:\n{out}"
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
fn filter_plan_only_prerequisites_managers_keeps_every_manager_node() {
    // `--only prerequisites.managers` (the docs' own recovery command for a
    // stranded-install alert) drops every package install — none of them
    // matches the selector — which would leave zero surviving consumers for
    // either manager. Proves the fix for finding 1: `prune_to_surviving_consumers`
    // must NOT run when `only` is non-empty, or the very managers the user
    // asked to keep are deleted for having no consumers left.
    let mut plan = make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![
                Action::Manager(ManagerAction::Provision {
                    manager: "brew".to_string(),
                    via: "homebrew installer".to_string(),
                    batched: vec![],
                    depends_on: vec![],
                }),
                Action::Manager(ManagerAction::RefreshIndex {
                    manager: "npm".to_string(),
                }),
            ],
        ),
        (
            PhaseName::Packages,
            vec![
                pkg_install("brew", vec!["rg"]),
                pkg_install("npm", vec!["typescript"]),
            ],
        ),
    ]);
    filter_plan(
        &mut plan,
        &[],
        &["prerequisites.managers".to_string()],
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );

    let prereq_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Prerequisites)
        .expect("both manager nodes must survive; the Prerequisites phase must not be dropped");
    assert_eq!(
        prereq_phase.action_count(),
        2,
        "both manager nodes must survive `--only prerequisites.managers` even \
         though every package consumer fell out of scope: {:?}",
        prereq_phase.actions().collect::<Vec<_>>()
    );
    assert!(
        !plan.phases.iter().any(|p| p.name == PhaseName::Packages),
        "no package matched the selector, so the Packages phase must be dropped: {:?}",
        plan.phases
    );
}

#[test]
fn filter_plan_only_cfgd_managers_keeps_every_manager_node() {
    // `--only cfgd:managers` is the OWNER-group spelling of the same
    // recovery command `prerequisites.managers` covers, but it reaches the
    // action through `pattern_matches_action`'s first rule (`owner.token()
    // == pattern`) rather than the phase-qualified group alias
    // (`phase_qualified_group_owner_token`) the dotted form uses — a
    // genuinely different code path. Proves finding 1's fix holds for BOTH
    // spellings: `prune_to_surviving_consumers` must not run when `only` is
    // non-empty, whichever grammar named the managers group.
    let mut plan = make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![
                Action::Manager(ManagerAction::Provision {
                    manager: "brew".to_string(),
                    via: "homebrew installer".to_string(),
                    batched: vec![],
                    depends_on: vec![],
                }),
                Action::Manager(ManagerAction::RefreshIndex {
                    manager: "npm".to_string(),
                }),
            ],
        ),
        (
            PhaseName::Packages,
            vec![
                pkg_install("brew", vec!["rg"]),
                pkg_install("npm", vec!["typescript"]),
            ],
        ),
    ]);
    filter_plan(
        &mut plan,
        &[],
        &["cfgd:managers".to_string()],
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );

    let prereq_phase = plan
        .phases
        .iter()
        .find(|p| p.name == PhaseName::Prerequisites)
        .expect("both manager nodes must survive; the Prerequisites phase must not be dropped");
    assert_eq!(
        prereq_phase.action_count(),
        2,
        "both manager nodes must survive `--only cfgd:managers` even \
         though every package consumer fell out of scope: {:?}",
        prereq_phase.actions().collect::<Vec<_>>()
    );
    assert!(
        !plan.phases.iter().any(|p| p.name == PhaseName::Packages),
        "no package matched the selector, so the Packages phase must be dropped: {:?}",
        plan.phases
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
    let output = build_plan_output(&plan, "my-machine", None, &[], &no_decisions());

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
    phase.groups.iter().flat_map(|g| g.actions()).collect()
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
        &no_decisions(),
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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

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
            Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                batched: vec![],
                depends_on: vec![],
            }),
            pkg_install("apt", vec!["sl"]),
        ],
    )]);
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

    assert_eq!(
        output.phases[0]
            .groups
            .iter()
            .map(|g| g.token())
            .collect::<Vec<_>>(),
        vec!["profile:work", "cfgd:managers", "module:dev-tools"],
    );
    for group in &output.phases[0].groups {
        assert_eq!(
            group.token(),
            group.owner().token(),
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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

    assert_eq!(
        output.phases[0]
            .groups
            .iter()
            .map(|g| g.token())
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
fn build_plan_output_manager_action_carries_the_structured_manager_payload() {
    // Spec §7: `phases[]` gains a `managers` phase object with one group,
    // `cfgd:managers`, whose actions carry `{manager, state, via, requires}`.
    let plan = make_plan(vec![(
        PhaseName::Prerequisites,
        vec![
            Action::Manager(ManagerAction::RefreshIndex {
                manager: "brew".to_string(),
            }),
            Action::Manager(ManagerAction::Provision {
                manager: "pipx".to_string(),
                via: "pip install pipx".to_string(),
                batched: vec![],
                depends_on: vec!["manager:prereq:curl".to_string()],
            }),
            Action::Manager(ManagerAction::Refuse {
                manager: "snap".to_string(),
                reason: "no available system manager".to_string(),
            }),
        ],
    )]);
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());
    let json = serde_json::to_value(&output).unwrap();
    let groups = json["phases"][0]["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["token"], serde_json::json!("cfgd:managers"));
    let actions = groups[0]["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 3);

    let refresh = &actions[0];
    assert_eq!(refresh["manager"]["manager"], serde_json::json!("brew"));
    assert_eq!(refresh["manager"]["state"], serde_json::json!("present"));
    assert!(
        refresh["manager"].get("via").is_none(),
        "refresh carries no via: {refresh}"
    );
    assert!(
        refresh["manager"].get("requires").is_none(),
        "refresh depends on nothing, so requires is omitted: {refresh}"
    );

    let provision = &actions[1];
    assert_eq!(provision["manager"]["manager"], serde_json::json!("pipx"));
    assert_eq!(
        provision["manager"]["state"],
        serde_json::json!("provisioned")
    );
    assert_eq!(
        provision["manager"]["via"],
        serde_json::json!("pip install pipx")
    );
    assert_eq!(
        provision["manager"]["requires"],
        serde_json::json!(["manager:prereq:curl"]),
        "requires resolves against a sibling row's description one-to-one"
    );

    let refuse = &actions[2];
    assert_eq!(refuse["manager"]["manager"], serde_json::json!("snap"));
    assert_eq!(refuse["manager"]["state"], serde_json::json!("refused"));
    assert_eq!(
        refuse["manager"]["reason"],
        serde_json::json!("no available system manager")
    );

    // A non-manager row never carries the key at all.
    let other = build_plan_output(
        &make_plan(vec![(PhaseName::Files, vec![file_create("/etc/foo")])]),
        "ctx",
        None,
        &[],
        &no_decisions(),
    );
    let other_json = serde_json::to_value(&other).unwrap();
    assert!(
        other_json["phases"][0]["groups"][0]["actions"][0]
            .get("manager")
            .is_none(),
        "a file action must not carry the manager key: {other_json}"
    );
}

#[test]
fn build_plan_output_non_module_phase_omits_module_and_section_keys() {
    let plan = make_plan(vec![(PhaseName::Files, vec![file_create("/etc/foo")])]);
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());
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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

    let desc = &output.phases[0].groups[0].actions()[0].description;
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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

    let desc = &output.phases[0].groups[0].actions()[0].description;
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

    let out = cfgd_core::test_helpers::captured_text(&buf);
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

    let out = cfgd_core::test_helpers::captured_text(&buf);
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

    let out = cfgd_core::test_helpers::captured_text(&buf);
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

    let out = cfgd_core::test_helpers::captured_text(&buf);
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
    // The production id is minted posix-folded (`reconciler::format`), and
    // `is_unmanaged_file` folds its lookup to match — a `display()` id would
    // never be found on Windows.
    state
        .upsert_managed_resource(
            "file",
            &cfgd_core::to_posix_string(&file_path),
            "test",
            None,
            None,
        )
        .unwrap();

    let config_dir = PathBuf::from("/config");
    let result = is_unmanaged_file(&file_path, &config_dir, &state);
    assert!(
        !result,
        "state-tracked file should not be considered unmanaged"
    );
}

#[test]
fn backup_file_copies_to_cfgd_backup_suffix_and_leaves_the_original() {
    let tmp = tempfile::tempdir().unwrap();
    let original = tmp.path().join("myfile.txt");
    std::fs::write(&original, "original content").unwrap();

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let written = backup_file(&original, &printer).unwrap();

    let backup = tmp.path().join("myfile.txt.cfgd-backup");
    assert_eq!(written, backup, "backup should land at the sidecar path");
    assert!(backup.exists(), "backup file should exist at expected path");
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "original content",
        "the sidecar must hold the original bytes"
    );
    // The crash window the rename opened: between moving the file away and
    // writing the managed one, the content existed at neither path.
    assert!(
        original.exists(),
        "the original must stay in place until the apply's own atomic write replaces it"
    );
    assert_eq!(
        std::fs::read_to_string(&original).unwrap(),
        "original content",
        "the original content must be untouched by the backup"
    );

    let out = cfgd_core::test_helpers::captured_text(&buf);
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
fn backup_file_verifies_and_preserves_the_mode_of_its_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let original = tmp.path().join("secret.env");
    std::fs::write(&original, "TOKEN=keep-me\n").unwrap();
    cfgd_core::set_file_permissions(&original, 0o600).unwrap();

    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let backup = backup_file(&original, &printer).unwrap();

    let meta = std::fs::metadata(&backup).unwrap();
    assert_eq!(
        cfgd_core::file_permissions_mode(&meta),
        cfgd_core::file_permissions_mode(&std::fs::metadata(&original).unwrap()),
        "the sidecar must carry the mode of the file it preserves"
    );
    assert_eq!(
        cfgd_core::sha256_hex(&std::fs::read(&backup).unwrap()),
        cfgd_core::sha256_hex(&std::fs::read(&original).unwrap()),
        "the sidecar must hash identically to the original"
    );
}

#[test]
fn backup_file_never_clobbers_an_older_sidecar() {
    // The primary sidecar holds the content that predates cfgd; a second,
    // different original is stamped instead of destroying the first.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("conf.toml");
    let primary = tmp.path().join("conf.toml.cfgd-backup");
    std::fs::write(&primary, "the original").unwrap();
    std::fs::write(&target, "something else entirely").unwrap();

    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let written = backup_file(&target, &printer).unwrap();

    assert_ne!(written, primary, "an occupied sidecar must not be reused");
    assert_eq!(
        std::fs::read_to_string(&primary).unwrap(),
        "the original",
        "the older sidecar must survive untouched"
    );
    assert_eq!(
        std::fs::read_to_string(&written).unwrap(),
        "something else entirely"
    );
}

#[test]
fn backup_file_reuses_a_sidecar_that_already_holds_the_same_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("conf.toml");
    let primary = tmp.path().join("conf.toml.cfgd-backup");
    std::fs::write(&target, "same bytes").unwrap();
    std::fs::write(&primary, "same bytes").unwrap();

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let written = backup_file(&target, &printer).unwrap();

    assert_eq!(
        written, primary,
        "an identical sidecar is reused, not stamped"
    );
    let entries: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 2, "no second sidecar should be created");
    let out = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        out.contains("Already backed up at"),
        "a reused sidecar says so, got: {out}"
    );
}

#[test]
fn apply_conflict_policy_backup_copies_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("target.txt");
    std::fs::write(&file, "content").unwrap();

    let mut action = file_create(file.to_str().unwrap());
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    apply_conflict_policy(ResolvedConflict::Backup, &file, &mut action, &printer).unwrap();

    let backup = tmp.path().join("target.txt.cfgd-backup");
    assert!(
        backup.exists(),
        "backup file should exist after Backup policy"
    );
    assert!(file.exists(), "the target must survive the backup");
}

#[test]
fn apply_conflict_policy_skip_converts_action_to_skip() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("target.txt");
    std::fs::write(&file, "content").unwrap();

    let mut action = file_create(file.to_str().unwrap());
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    apply_conflict_policy(ResolvedConflict::Skip, &file, &mut action, &printer).unwrap();

    assert!(
        matches!(action, Action::File(FileAction::Skip { .. })),
        "action should be converted to Skip after Skip policy"
    );
    assert!(
        !tmp.path().join("target.txt.cfgd-backup").exists(),
        "Skip writes no sidecar"
    );
}

#[test]
fn apply_conflict_policy_overwrite_leaves_action_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("target.txt");
    std::fs::write(&file, "content").unwrap();

    let mut action = file_create(file.to_str().unwrap());
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    apply_conflict_policy(ResolvedConflict::Overwrite, &file, &mut action, &printer).unwrap();

    assert!(
        matches!(action, Action::File(FileAction::Create { .. })),
        "action should remain Create after Overwrite policy"
    );
    assert!(
        !tmp.path().join("target.txt.cfgd-backup").exists(),
        "Overwrite keeps no copy"
    );
}

#[test]
fn apply_conflict_policy_fail_aborts_and_touches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("target.txt");
    std::fs::write(&file, "content").unwrap();

    let mut action = file_create(file.to_str().unwrap());
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let err =
        apply_conflict_policy(ResolvedConflict::Fail, &file, &mut action, &printer).unwrap_err();

    assert!(
        err.to_string().contains("--on-conflict fail"),
        "the abort names the policy that caused it, got: {err}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "content");
    assert!(!tmp.path().join("target.txt.cfgd-backup").exists());
}

#[test]
fn yes_resolves_ask_to_backup_and_leaves_an_explicit_policy_alone() {
    assert_eq!(
        resolve_conflict_policy(OnConflict::Ask, true),
        Some(ResolvedConflict::Backup),
        "--yes must mean 'do not stop to ask', never 'discard my file'"
    );
    assert_eq!(
        resolve_conflict_policy(OnConflict::Ask, false),
        None,
        "without --yes the question is still asked, per target"
    );
    for (policy, resolved) in [
        (OnConflict::Backup, ResolvedConflict::Backup),
        (OnConflict::Overwrite, ResolvedConflict::Overwrite),
        (OnConflict::Skip, ResolvedConflict::Skip),
        (OnConflict::Fail, ResolvedConflict::Fail),
    ] {
        assert_eq!(
            resolve_conflict_policy(policy, true),
            Some(resolved),
            "an explicit policy passes through --yes untouched"
        );
    }
}

#[test]
fn every_prompt_option_maps_to_a_settled_policy() {
    let options = conflict_prompt_options();
    assert_eq!(
        options.len(),
        PROMPT_POLICIES.len(),
        "an option with no policy beside it selects the fallback silently"
    );
    // The interactive vocabulary is the flag's vocabulary: no outcome may be
    // reachable only by re-running with `--on-conflict`.
    for policy in [
        ResolvedConflict::Backup,
        ResolvedConflict::Overwrite,
        ResolvedConflict::Skip,
        ResolvedConflict::Fail,
    ] {
        assert!(
            PROMPT_POLICIES.contains(&policy),
            "{policy:?} is offered by the flag but not by the prompt"
        );
    }
}

#[test]
fn an_unanswerable_prompt_backs_the_file_up_instead_of_overwriting_it() {
    // No seeded answer and `interactive_stdin: false`, so `prompt_select`
    // fails — the shape a `--dry-run`-less apply piped into a script hits.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("zshrc");
    std::fs::write(&target, "hand written").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = one_phase_plan(vec![copy_update(&target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        false,
        OnConflict::Ask,
        FileStrategy::Symlink,
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(tmp.path().join("zshrc.cfgd-backup")).unwrap(),
        "hand written",
        "a prompt nobody can answer must still preserve the file"
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
            "Backup (copy to <target>.cfgd-backup, then overwrite)".into(),
        )]);
    let mut plan = one_phase_plan(vec![patch_update(&patch_target), copy_update(&copy_target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        false,
        OnConflict::Ask,
        FileStrategy::Symlink,
    )
    .unwrap();

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

#[test]
fn a_managed_target_is_recognised_by_the_id_the_reconciler_actually_mints() {
    // Ground truth, not a hand-written key: the id comes from the producer.
    // The two spellings agree on POSIX and diverge on Windows, where a lookup
    // keyed on native separators reports every managed file as unmanaged — so
    // `--yes` (which now means backup) mints a sidecar for cfgd's OWN files on
    // every apply, forever.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("zshrc");
    std::fs::write(&target, "written by cfgd").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    assert!(
        is_unmanaged_file(&target, tmp.path(), &state),
        "control: with no row at all the target is unmanaged"
    );

    let desc = cfgd_core::reconciler::format_action_description(&copy_update(&target));
    let id = desc
        .strip_prefix("file:update:")
        .expect("a file Update description carries the file:update: prefix");
    state
        .upsert_managed_resource("file", id, "local", None, None)
        .unwrap();

    assert!(
        !is_unmanaged_file(&target, tmp.path(), &state),
        "a target whose managed id the reconciler minted must be recognised as managed"
    );
}

#[test]
fn a_module_file_inheriting_the_global_copy_strategy_is_not_re_adopted() {
    // The file declares no strategy of its own; the config's `fileStrategy:
    // copy` decides what gets written. Judged on the per-file field, the target
    // is treated as un-comparable and copied aside and rewritten every apply.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "identical\n").unwrap();
    std::fs::write(&target, "identical\n").unwrap();

    let mut file = module_copy_file(&source, &target);
    file.strategy = None;

    let state = StateStore::open_in_memory().unwrap();
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![file]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Ask,
        FileStrategy::Copy,
    )
    .unwrap();

    assert!(
        !tmp.path().join("live.conf.cfgd-backup").exists(),
        "a converged target must not be copied aside"
    );
    assert_eq!(
        deployed_files(&plan),
        vec![target],
        "the file stays in the deployment"
    );
    let out = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        !out.contains("unmanaged file"),
        "a converged target is not a conflict, got: {out}"
    );
}

fn set_permissions(target: &Path, mode: u32) -> Action {
    Action::File(FileAction::SetPermissions {
        target: target.to_path_buf(),
        mode,
        origin: "test".to_string(),
    })
}

#[test]
fn skip_drops_the_chmod_planned_beside_the_write_it_skipped() {
    // Planning pairs every write with a sibling `SetPermissions`. Left behind,
    // `--on-conflict skip` still changes the mode of the file it undertook to
    // leave untouched.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("zshrc");
    std::fs::write(&target, "hand written").unwrap();
    cfgd_core::set_file_permissions(&target, 0o600).ok();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = one_phase_plan(vec![copy_update(&target), set_permissions(&target, 0o644)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Skip,
        FileStrategy::Symlink,
    )
    .unwrap();

    let remaining: Vec<_> = plan
        .phases
        .iter()
        .flat_map(|p| p.owned_actions())
        .map(|(_, a)| a)
        .collect();
    assert!(
        !remaining
            .iter()
            .any(|a| matches!(a, Action::File(FileAction::SetPermissions { .. }))),
        "the chmod planned beside a skipped write must go with it, got: {remaining:?}"
    );
    assert!(
        remaining
            .iter()
            .any(|a| matches!(a, Action::File(FileAction::Skip { .. }))),
        "the write itself is still reported as skipped"
    );
}

#[test]
fn a_skipped_module_file_reports_the_same_reason_the_profile_arm_does() {
    // A dropped module file leaves no action behind to render, so a silent
    // removal is a file the user is never told was left alone.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from module\n").unwrap();
    std::fs::write(&target, "hand written\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![module_copy_file(&source, &target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Skip,
        FileStrategy::Symlink,
    )
    .unwrap();

    let out = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        out.contains("skipped: target exists as unmanaged file"),
        "the module arm must say what the profile arm's Skip action says, got: {out}"
    );
    assert!(
        out.contains("live.conf"),
        "the report names the file it left alone, got: {out}"
    );
    assert!(
        deployed_files(&plan).is_empty(),
        "the file is dropped from the deployment"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hand written\n");
}

#[test]
fn two_adoptions_in_the_same_second_land_beside_each_other_never_on_top() {
    // The stamp has one-second resolution, so it is a hint at a free name and
    // never a guarantee of one: unchecked, the second adoption of a second
    // original overwrites the sidecar holding the first.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("live.conf");
    let primary = tmp.path().join("live.conf.cfgd-backup");
    std::fs::write(&primary, "first original").unwrap();

    let (printer, _) = Printer::for_test_at(Verbosity::Normal);

    std::fs::write(&target, "second original").unwrap();
    let second = backup_file(&target, &printer).unwrap();
    std::fs::write(&target, "third original").unwrap();
    let third = backup_file(&target, &printer).unwrap();

    assert_ne!(second, third, "back-to-back adoptions need distinct names");
    assert_eq!(std::fs::read_to_string(&primary).unwrap(), "first original");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "second original");
    assert_eq!(std::fs::read_to_string(&third).unwrap(), "third original");
}

#[test]
fn a_directory_backup_never_merges_into_an_occupied_sidecar() {
    // `copy_dir_recursive` writes INTO an existing directory, so an occupied
    // sidecar silently fuses two different originals into one tree.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("conf.d");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("new.conf"), "new").unwrap();

    let primary = tmp.path().join("conf.d.cfgd-backup");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::write(primary.join("old.conf"), "old").unwrap();

    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let first = backup_file(&target, &printer).unwrap();

    // A second, different original in the same second: the stamp alone would
    // name the directory the first one just filled.
    std::fs::remove_file(target.join("new.conf")).unwrap();
    std::fs::write(target.join("newer.conf"), "newer").unwrap();
    let second = backup_file(&target, &printer).unwrap();

    assert_ne!(
        first, primary,
        "an occupied sidecar directory is not reused"
    );
    assert_ne!(first, second, "two originals need two directories");
    assert_eq!(
        std::fs::read_dir(&primary).unwrap().count(),
        1,
        "the older sidecar must not gain the newer originals' entries"
    );
    assert!(primary.join("old.conf").exists());
    assert!(first.join("new.conf").exists() && !first.join("newer.conf").exists());
    assert!(second.join("newer.conf").exists() && !second.join("new.conf").exists());
}

#[test]
fn an_interrupted_prompt_aborts_while_an_unreachable_one_backs_up() {
    // Ctrl-C and "nobody to ask" are not the same event: resolving the first
    // like the second carries out, file by file, the work the user interrupted
    // to prevent.
    let target = Path::new("/tmp/does-not-need-to-exist");

    let err = settle_prompt_failure(inquire::InquireError::OperationInterrupted, target)
        .expect_err("an interrupted prompt must abort the run");
    assert!(
        err.to_string().contains("interrupted"),
        "the abort says why, got: {err}"
    );
    settle_prompt_failure(inquire::InquireError::OperationCanceled, target)
        .expect_err("a cancelled prompt must abort the run");

    assert_eq!(
        settle_prompt_failure(inquire::InquireError::NotTTY, target).unwrap(),
        ResolvedConflict::Backup,
        "a prompt that could not be reached lands where --yes lands"
    );
}

#[test]
fn the_prompts_abort_answer_stops_the_run_without_touching_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("zshrc");
    std::fs::write(&target, "hand written").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _cap) =
        Printer::for_test_doc_with_prompt_responses(vec![cfgd_core::output::PromptAnswer::Select(
            "Abort (stop the apply without touching the file)".into(),
        )]);
    let mut plan = one_phase_plan(vec![copy_update(&target)]);

    let err = handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        false,
        OnConflict::Ask,
        FileStrategy::Symlink,
    )
    .unwrap_err()
    .to_string();

    assert!(
        err.contains("--on-conflict fail"),
        "the interactive abort is the same abort the flag gives, got: {err}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hand written");
    assert!(!tmp.path().join("zshrc.cfgd-backup").exists());
}

#[cfg(unix)]
#[test]
fn a_sidecar_carries_the_setuid_bit_of_the_file_it_preserves() {
    // A backup is the file it preserves; a special bit dropped in the copy is
    // not restorable from it.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("helper.sh");
    std::fs::write(&target, "#!/bin/sh\n").unwrap();
    cfgd_core::set_file_permissions(&target, 0o4755).unwrap();

    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let backup = backup_file(&target, &printer).unwrap();

    let mode = cfgd_core::file_permissions_mode_full(&std::fs::metadata(&backup).unwrap());
    assert_eq!(
        mode,
        Some(0o4755),
        "the sidecar must reproduce the mode it is a copy of"
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
        caveats: Vec::new(),
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
    print_caveats(&result, &printer);

    let out = cfgd_core::test_helpers::captured_text(&buf);
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
            "Backup (copy to <target>.cfgd-backup, then overwrite)".into(),
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

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        false,
        OnConflict::Ask,
        FileStrategy::Symlink,
    )
    .unwrap();

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

// --- unmanaged-file conflicts: hash short-circuit + `--on-conflict` ---

fn module_copy_file(source: &Path, target: &Path) -> cfgd_core::modules::ResolvedFile {
    cfgd_core::modules::ResolvedFile {
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        is_git_source: false,
        strategy: Some(FileStrategy::Copy),
        permissions: None,
        encryption: None,
        patch: None,
    }
}

fn module_deploy_plan(files: Vec<cfgd_core::modules::ResolvedFile>) -> Plan {
    one_phase_plan(vec![Action::Module(ModuleAction::local(
        "mymod".to_string(),
        ModuleActionKind::DeployFiles { files },
    ))])
}

fn deployed_files(plan: &Plan) -> Vec<PathBuf> {
    plan.phases
        .iter()
        .flat_map(|p| p.owned_actions())
        .filter_map(|(_, a)| match a {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::DeployFiles { files } => Some(files),
                _ => None,
            },
            _ => None,
        })
        .flat_map(|files| files.iter().map(|f| f.target.clone()))
        .collect()
}

#[test]
fn a_module_target_already_holding_the_desired_bytes_is_never_backed_up() {
    // Re-adopting content that is already there must not mint a sidecar copy
    // of bytes the module source already holds.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "identical\n").unwrap();
    std::fs::write(&target, "identical\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![module_copy_file(&source, &target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Ask,
        FileStrategy::Symlink,
    )
    .unwrap();

    assert!(
        !tmp.path().join("live.conf.cfgd-backup").exists(),
        "a converged target is not a conflict and needs no sidecar"
    );
    let out = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        !out.contains("unmanaged file"),
        "a converged target must not be announced as a conflict, got: {out}"
    );
}

#[test]
fn a_module_target_holding_different_bytes_is_copied_aside_under_yes() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    std::fs::write(&target, "hand written\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![module_copy_file(&source, &target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Ask,
        FileStrategy::Symlink,
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(tmp.path().join("live.conf.cfgd-backup")).unwrap(),
        "hand written\n",
        "--yes must preserve the file it is about to replace"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "hand written\n",
        "the target survives until the deployment's own write replaces it"
    );
    assert_eq!(
        deployed_files(&plan),
        vec![target],
        "backup leaves the deployment in the plan"
    );
}

#[test]
fn on_conflict_overwrite_keeps_no_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    std::fs::write(&target, "hand written\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![module_copy_file(&source, &target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Overwrite,
        FileStrategy::Symlink,
    )
    .unwrap();

    assert!(
        !tmp.path().join("live.conf.cfgd-backup").exists(),
        "overwrite keeps no copy"
    );
    assert_eq!(
        deployed_files(&plan),
        vec![target],
        "overwrite leaves the deployment in the plan"
    );
}

#[test]
fn on_conflict_skip_drops_the_file_from_the_deployment() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    std::fs::write(&target, "hand written\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![module_copy_file(&source, &target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Skip,
        FileStrategy::Symlink,
    )
    .unwrap();

    assert!(
        deployed_files(&plan).is_empty(),
        "skip removes the file from the deployment"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hand written\n");
    assert!(!tmp.path().join("live.conf.cfgd-backup").exists());
}

#[test]
fn on_conflict_fail_aborts_naming_the_module_and_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    std::fs::write(&target, "hand written\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![module_copy_file(&source, &target)]);

    let err = handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Fail,
        FileStrategy::Symlink,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("mymod"), "the abort names the module: {err}");
    assert!(err.contains("live.conf"), "the abort names the file: {err}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hand written\n");
    assert!(!tmp.path().join("live.conf.cfgd-backup").exists());
}

#[test]
fn a_profile_target_already_holding_the_planned_content_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("zshrc");
    std::fs::write(&target, "export EDITOR=vim\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let mut action = copy_update(&target);
    if let Action::File(FileAction::Update {
        ref mut source_hash,
        ..
    }) = action
    {
        *source_hash = Some(cfgd_core::sha256_hex(b"export EDITOR=vim\n"));
    }
    let mut plan = one_phase_plan(vec![action]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Backup,
        FileStrategy::Symlink,
    )
    .unwrap();

    assert!(
        !tmp.path().join("zshrc.cfgd-backup").exists(),
        "a converged profile target must not be copied aside"
    );
    let out = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        !out.contains("unmanaged file"),
        "a converged profile target is not announced as a conflict, got: {out}"
    );
}

#[test]
fn a_crash_between_adoption_and_the_write_leaves_the_users_file_on_disk() {
    // Adoption runs and the process dies before the reconciler writes a byte:
    // the state the rename could not survive. Nothing below writes the target,
    // so what the assertions read IS the post-crash filesystem.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&source, "from the module\n").unwrap();
    std::fs::write(&target, "years of hand edits\n").unwrap();

    let state = StateStore::open_in_memory().unwrap();
    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let mut plan = module_deploy_plan(vec![module_copy_file(&source, &target)]);

    handle_unmanaged_file_targets(
        &mut plan,
        tmp.path(),
        &state,
        &printer,
        true,
        OnConflict::Backup,
        FileStrategy::Symlink,
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "years of hand edits\n",
        "the user's file must still be at the path they know it by"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("live.conf.cfgd-backup")).unwrap(),
        "years of hand edits\n",
        "and at the sidecar, so either survivor is the whole file"
    );
}

#[test]
fn a_symlinked_target_is_backed_up_as_a_link_not_as_its_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("elsewhere.conf");
    let target = tmp.path().join("live.conf");
    std::fs::write(&dest, "the destination\n").unwrap();
    cfgd_core::create_symlink(&dest, &target).unwrap();

    let (printer, _) = Printer::for_test_at(Verbosity::Normal);
    let backup = backup_file(&target, &printer).unwrap();

    assert_eq!(
        backup.read_link().unwrap(),
        dest,
        "the sidecar must preserve the link, not materialize its destination"
    );
    assert!(target.symlink_metadata().is_ok(), "the link stays in place");
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
        print_caveats(&result, &printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);
        (out, home)
    });

    assert!(
        !home.is_empty() && home != "~",
        "the test home must resolve to a real sandbox path, got: {home}"
    );
    assert!(
        out.contains("Caveats"),
        "expected Caveats heading, got: {out}"
    );
    assert!(
        out.contains("cfgd:env"),
        "expected the cfgd:env owner group, got: {out}"
    );
    assert!(
        out.contains("run `source ~/.cfgd.env`"),
        "expected a retypeable source command, got: {out}"
    );
    assert!(
        out.contains("— or open a new shell"),
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
        print_caveats(&result, &printer);
        cfgd_core::test_helpers::captured_text(&buf)
    });

    assert!(
        out.contains("run `source /home/u/.cfgd.env`"),
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
    print_caveats(&result, &printer);

    let out = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        out.contains("Caveats") && out.contains("cfgd:env"),
        "an rc file that only just learned to source the env file still leaves \
         the running shell stale: {out}"
    );
}

#[test]
fn shell_env_reminder_absent_under_structured_output() {
    let result = env_apply_result(&["env:write:/home/u/.cfgd.env"]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Quiet);
    print_caveats(&result, &printer);

    let out = cfgd_core::test_helpers::captured_text(&buf);
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
    fn bootstrap_plan(&self) -> Option<cfgd_core::providers::BootstrapPlan> {
        None
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
    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> cfgd_core::errors::Result<Option<String>> {
        Ok(None)
    }
}

/// The plan the stranded-install warning is derived from: one Prerequisites
/// provision plus two Packages installs that need the manager it would have
/// provided, one of them through a sub-manager that has no provision of its
/// own.
fn brew_provision_plan() -> Plan {
    make_plan(vec![
        (
            PhaseName::Prerequisites,
            vec![Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                batched: vec![],
                depends_on: vec![],
            })],
        ),
        (
            PhaseName::Packages,
            vec![
                pkg_install("brew", vec!["ripgrep"]),
                pkg_install("brew-tap", vec!["homebrew/cask"]),
            ],
        ),
    ])
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
        None,
        &Printer::for_test().0,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
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
    let mut plan = brew_provision_plan();
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["cfgd:managers".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        !plan
            .phases
            .iter()
            .flat_map(|p| p.actions())
            .any(|a| matches!(a, Action::Manager(ManagerAction::Provision { .. }))),
        "the provision node is gone, which is what strands the installs"
    );
    assert_eq!(
        out.matches("removes 1 bootstrap").count(),
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
fn skip_packages_brew_leaves_the_sub_manager_it_does_not_cover_untouched() {
    // `pattern_matches`' segment boundary means `packages.brew` never covers
    // `packages.brew-tap`, so the tap install survives its parent's removal —
    // and the pattern can never reach the shared provision node at all, since
    // that node lives in `Prerequisites`, not `Packages`. Nothing is stranded:
    // the manager stays provisioned and brew-tap applies normally.
    let mut plan = brew_provision_plan();
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["packages.brew".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        plan.phases
            .iter()
            .flat_map(|p| p.actions())
            .any(|a| matches!(
                a,
                Action::Package(PackageAction::Install { manager, .. }) if manager == "brew-tap"
            )),
        "brew-tap's install survives its parent's removal: {:?}",
        plan.phases
    );
    assert!(
        !plan
            .phases
            .iter()
            .flat_map(|p| p.actions())
            .any(|a| matches!(a, Action::Package(PackageAction::Install { manager, .. }) if manager == "brew")),
        "brew's own install went with the pattern: {:?}",
        plan.phases
    );
    assert!(
        out.is_empty(),
        "the provision node lives in Prerequisites, untouched by a \
         `packages.*` pattern, so nothing is stranded: {out}"
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
            Action::Manager(ManagerAction::Provision {
                manager: "brew".to_string(),
                via: "homebrew installer".to_string(),
                batched: vec![],
                depends_on: vec![],
            }),
            pkg_install("brew", vec!["ripgrep"]),
            pkg_install("brew", vec!["fd"]),
        ],
    )]);
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["cfgd:managers".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        out.contains("2 package actions"),
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
    let mut plan = brew_provision_plan();
    let mut registry = ProviderRegistry::new();
    registry.add_package_manager(Box::new(AvailableManager("brew")));
    registry.add_package_manager(Box::new(AvailableManager("brew-tap")));
    let (printer, buf) = Printer::for_test();
    filter_plan(
        &mut plan,
        &["cfgd:managers".to_string()],
        &[],
        None,
        &printer,
        &registry,
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        !out.contains("still name") && !out.contains("still names"),
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
    let output = build_plan_output(&plan, "ctx", None, &[], &no_decisions());

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

/// A resolved profile whose one local layer declares `spec` as YAML.
fn local_resolved(spec_yaml: &str) -> cfgd_core::config::ResolvedProfile {
    use cfgd_core::config::{LOCAL_LAYER, LayerPolicy, ProfileLayer, ProfileSpec, merge_layers};
    let spec: ProfileSpec = serde_yaml::from_str(spec_yaml).expect("profile spec parses");
    let layers = vec![ProfileLayer {
        source: LOCAL_LAYER.to_string(),
        profile_name: "p".to_string(),
        priority: 1000,
        policy: LayerPolicy::Local,
        spec,
    }];
    let merged = merge_layers(&layers);
    cfgd_core::config::ResolvedProfile { layers, merged }
}

/// A config subscribed to one source named `acme`.
fn config_subscribed_to_acme() -> cfgd_core::config::CfgdConfig {
    serde_yaml::from_str(
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: p\n  \
         sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.test/acme.git\n",
    )
    .expect("config parses")
}

#[test]
fn a_decision_never_withholds_a_package_the_operator_declares_in_a_manifest_file() {
    // `brew.file: Brewfile` is a declaration like any other — it just resolves
    // later, into the merged package set rather than into a layer. A guard
    // reading only the layers leaves that entire declaration style unprotected:
    // a source's decision over the same package name withholds the operator's
    // own install from every plan.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Brewfile"), "brew \"ripgrep\"\n").unwrap();

    let store = cfgd_core::test_helpers::test_state();
    store
        .upsert_pending_decision(
            "acme",
            "packages.brew.ripgrep",
            "recommended",
            "install",
            "recommended ripgrep (from acme)",
        )
        .unwrap();

    let cli = test_cli_in(dir.path());
    let printer = Printer::for_test().0;
    let ctx = RunContext::new(&cli, &printer);
    let (withheld, _review) = withheld_for_run(
        &ctx,
        &store,
        &config_subscribed_to_acme(),
        &local_resolved("packages:\n  brew:\n    file: Brewfile\n"),
        true,
        DecisionWrites::ReadOnly,
        &reconciler::ActualPackages::default(),
    )
    .expect("the decision gate reads a healthy store");

    assert!(
        withheld.is_empty(),
        "the operator's manifest-declared package outranks a source's decision \
         over the same name, got {withheld:?}"
    );
}

#[test]
fn a_run_that_could_not_read_its_config_still_withholds_every_row() {
    // The fail-closed half: with no authoritative subscription list, a row must
    // keep withholding rather than be released by a fabricated empty list.
    let dir = tempfile::tempdir().unwrap();
    let store = cfgd_core::test_helpers::test_state();
    store
        .upsert_pending_decision(
            "acme",
            "packages.brew.ripgrep",
            "recommended",
            "install",
            "recommended ripgrep (from acme)",
        )
        .unwrap();

    let cli = test_cli_in(dir.path());
    let printer = Printer::for_test().0;
    let ctx = RunContext::new(&cli, &printer);
    let (withheld, _review) = withheld_for_run(
        &ctx,
        &store,
        &cfgd_core::config::minimal_config(),
        &local_resolved("{}\n"),
        false,
        DecisionWrites::ReadOnly,
        &reconciler::ActualPackages::default(),
    )
    .expect("the decision gate reads a healthy store");

    assert_eq!(
        withheld.pending.len(),
        1,
        "an unparsed config is not evidence that the source was dropped"
    );
}

// -----------------------------------------------------------------------
// TokenHits — per-token --skip/--only match accounting (QP9b deliverable 3)
// -----------------------------------------------------------------------

#[test]
fn token_hits_seeds_every_supplied_token_at_zero_in_first_seen_order() {
    let hits = TokenHits::new(&[
        "files".to_string(),
        "packages.brew".to_string(),
        "files".to_string(),
    ]);
    // A duplicate token collapses to one accounting slot, and every slot
    // starts at zero — nothing is recorded until `record` is called.
    assert_eq!(
        hits.misses(),
        vec!["files", "packages.brew"],
        "every never-recorded token is a miss, deduped, in first-seen order"
    );
}

#[test]
fn token_hits_record_only_advances_the_named_token() {
    let mut hits = TokenHits::new(&["files".to_string(), "packages.brew".to_string()]);
    hits.record("files");
    hits.record("files");
    // Recording a token this instance was never seeded with must not panic
    // and must not fabricate a new tracked entry.
    hits.record("packages.npm");
    assert_eq!(
        hits.misses(),
        vec!["packages.brew"],
        "a recorded token drops out of misses; an unseeded record is a no-op"
    );
}

#[test]
fn token_hits_misses_empty_once_every_token_is_recorded() {
    let mut hits = TokenHits::new(&["files".to_string()]);
    hits.record("files");
    assert!(hits.misses().is_empty());
}

// -----------------------------------------------------------------------
// module_known_but_unresolved — the --module hint gate for a `module:<name>`
// zero-match token (QP9b deliverable 3)
// -----------------------------------------------------------------------

#[test]
fn module_known_but_unresolved_true_for_a_locally_declared_module() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("nvm");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvm\nspec: {}\n",
    )
    .unwrap();

    assert!(
        module_known_but_unresolved(dir.path(), "nvm"),
        "a module declared under modules/ is known, even though it is not part of the active profile"
    );
    assert!(
        !module_known_but_unresolved(dir.path(), "no-such-module"),
        "a name naming nothing on disk or in the lockfile is not known"
    );
}

#[test]
fn module_known_but_unresolved_false_when_config_dir_has_no_modules_at_all() {
    assert!(!module_known_but_unresolved(
        Path::new("/nonexistent-config"),
        "anything"
    ));
}

// -----------------------------------------------------------------------
// filter_plan zero-match token accounting (QP9b deliverable 3) — the alert
// every `--skip`/`--only` token that matched nothing renders, and the
// `bool` return `apply.rs`/`plan.rs` fold into `ScopeReport.filter_miss`.
// -----------------------------------------------------------------------

#[test]
fn filter_plan_zero_match_skip_token_alerts_and_reports_a_miss() {
    let mut plan = make_plan(vec![(PhaseName::Files, vec![file_create("/etc/foo")])]);
    let (printer, buf) = Printer::for_test();
    let missed = filter_plan(
        &mut plan,
        &["no-such-owner".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(
        missed,
        "a token that matched zero actions must report a miss"
    );
    assert!(
        out.contains("`--skip no-such-owner` matched no actions in this plan"),
        "expected the zero-match alert naming the token verbatim, got:\n{out}"
    );
    assert!(
        out.contains("owners present: profile:test"),
        "expected the owner-token hint naming what the plan actually held, got:\n{out}"
    );
    // The file action itself never matched "no-such-owner", so it survives.
    assert_eq!(plan.phases[0].action_count(), 1);
}

#[test]
fn filter_plan_zero_match_only_token_naming_a_known_unresolved_module_hints_the_module_flag() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("nvm");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvm\nspec: {}\n",
    )
    .unwrap();

    let mut plan = make_plan(vec![(PhaseName::Files, vec![file_create("/etc/foo")])]);
    let (printer, buf) = Printer::for_test();
    let missed = filter_plan(
        &mut plan,
        &[],
        &["module:nvm".to_string()],
        None,
        &printer,
        &ProviderRegistry::new(),
        dir.path(),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(missed);
    assert!(
        out.contains("`--only module:nvm` matched no actions in this plan"),
        "got:\n{out}"
    );
    assert!(
        out.contains("to resolve a module outside the profile: --module nvm"),
        "a token naming a module cfgd already knows about (just not part of this run's graph) \
         must hint the way to bring it in, not the generic owner-token list, got:\n{out}"
    );
    assert!(
        !out.contains("owners present:"),
        "the module-specific hint replaces the generic one, got:\n{out}"
    );
}

#[test]
fn filter_plan_a_token_that_matches_something_alerts_for_nothing() {
    let mut plan = make_plan(vec![(
        PhaseName::Files,
        vec![file_create("/etc/foo"), file_update("/etc/bar")],
    )]);
    let (printer, buf) = Printer::for_test();
    let missed = filter_plan(
        &mut plan,
        &["files".to_string()],
        &[],
        None,
        &printer,
        &ProviderRegistry::new(),
        Path::new("/nonexistent-config"),
    );
    printer.flush();
    let out = cfgd_core::test_helpers::captured_text(&buf);

    assert!(!missed, "a token that matched every action is not a miss");
    assert!(
        !out.contains("matched no actions in this plan"),
        "a fully-matching token must not render the zero-match alert, got:\n{out}"
    );
}
