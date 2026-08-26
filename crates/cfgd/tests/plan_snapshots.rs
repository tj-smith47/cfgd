//! Snapshot tests for `cfgd plan`.
//!
//! Pins the rendered output of every shape `cmd_plan` produces. Goldens
//! live under `tests/output_snapshots/plan/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test plan_snapshots
//!
//! Cases:
//!   - `plan/happy.{txt,json}`   — multi-phase plan via real `cmd_plan`
//!     against `tiny_profile_setup`. The JSON case roundtrips the
//!     `PlanOutput` payload directly through `Doc::with_data` — pure data,
//!     no human-surface capture needed.
//!   - `plan/owner_groups.json` — the owner-axis payload: two groups in
//!     `Owner::sort_key` order inside one phase, with `owner`/`token`.
//!   - `plan/empty.txt`          — `MSG_NOTHING_TO_DO` branch via an
//!     empty-profile fixture.
//!   - `plan_module_only_unresolved_module_errors` (no golden — an error
//!     path) — `--module` naming a module that never resolves now fails the
//!     whole invocation instead of rendering a "matched no actions" warning
//!     and exiting 0 (the swallow that used to make that render possible was
//!     removed).
//!   - `plan/with_inert_decision.txt` — a decision row belonging to a source
//!     the config does not subscribe to: it withholds nothing and is named
//!     nowhere, so the render is byte-identical to the plain plan.
//!   - `plan/only_zero_match.txt` — `--only` naming a token
//!     (`packages.brwe`, a typo of `packages.brew`) that matches nothing in
//!     the plan. Pins the always-visible alert shape (names the token
//!     verbatim, hints the owner tokens the plan actually held) and that
//!     `MSG_NOTHING_TO_DO` never renders for this reason.
//!   - `plan/module_resolution_failure.txt` — a module whose package no
//!     registered manager can satisfy. Pins the permanent `Role::Fail` line
//!     `Printer::narrate`'s failure arm settles for the module walk.
//!   - `plan/module_package_elided.txt` — a module declaring two packages
//!     under a manager that already reports one installed: only the missing
//!     one is planned.

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::output_types::{PlanActionOutput, PlanGroupOutput, PlanOutput, PlanPhaseOutput};
use cfgd::cli::plan::cmd_plan;
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::test_capture::strip_spinner_duration;
use cfgd_core::output::{Doc, Printer};
use cfgd_core::reconciler::Owner;
use pretty_assertions::assert_eq;

use common::{
    cli_for, empty_profile_setup, plan_args, plan_args_module, state_with_pending_decision_setup,
    tiny_profile_setup,
};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_plan_output() -> PlanOutput {
    PlanOutput {
        context: "apply".to_string(),
        phases: vec![PlanPhaseOutput {
            phase: "Files".to_string(),
            // `profile:tiny` mirrors the owner the human golden draws for the
            // same run, and `create` is what `action_type_str` returns for
            // `FileAction::Create` — the fixture describes a payload the
            // product can actually emit.
            groups: vec![PlanGroupOutput::new(
                Owner::profile("tiny"),
                vec![PlanActionOutput {
                    description: "create /etc/hosts".to_string(),
                    action_type: "create".to_string(),
                    targets: vec!["/etc/hosts".to_string()],
                    origin: None,
                    manager: None,
                    detail: None,
                }],
            )],
        }],
        total_actions: 1,
        sources: vec![],
        warnings: vec![],
        pending_backups: vec![],
        pending_decisions: vec![],
        rejected_decisions: vec![],
    }
}

/// The owner-axis payload exactly as the redesign specifies it: two groups in
/// `Owner::sort_key` order (`profile:work` before `module:nvim`) inside one
/// phase, `origin` present only where a source delivered the body.
fn owner_groups_plan_output() -> PlanOutput {
    PlanOutput {
        context: "apply".to_string(),
        phases: vec![PlanPhaseOutput {
            phase: "Packages".to_string(),
            groups: vec![
                PlanGroupOutput::new(
                    Owner::profile("work"),
                    vec![PlanActionOutput {
                        description: "apt install sl, cowsay".to_string(),
                        action_type: "install".to_string(),
                        targets: vec!["sl".to_string(), "cowsay".to_string()],
                        origin: None,
                        manager: None,
                        detail: None,
                    }],
                ),
                PlanGroupOutput::new(
                    Owner::module("nvim"),
                    vec![PlanActionOutput {
                        description: "brew install neovim".to_string(),
                        action_type: "install".to_string(),
                        targets: vec!["neovim".to_string()],
                        origin: Some("team".to_string()),
                        manager: None,
                        detail: None,
                    }],
                ),
            ],
        }],
        total_actions: 2,
        sources: vec![],
        warnings: vec![],
        pending_backups: vec![],
        pending_decisions: vec![],
        rejected_decisions: vec![],
    }
}

/// Replace tempdir-rooted paths with stable placeholders so goldens are
/// host-stable. `cmd_plan` embeds the config-file path and target file
/// paths into its output (kv block + per-action lines).
fn normalize_tempdir_paths(raw: &str, config_dir: &Path, extra_paths: &[(&Path, &str)]) -> String {
    let cfg_file = config_dir.join("cfgd.yaml");
    let mut subs: Vec<(&Path, &str)> = Vec::with_capacity(extra_paths.len() + 2);
    subs.push((&cfg_file, "<CONFIG_DIR>/cfgd.yaml"));
    subs.extend(extra_paths.iter().copied());
    subs.push((config_dir, "<CONFIG_DIR>"));
    cfgd_core::normalize_for_snapshot(raw, &subs)
}

#[test]
fn plan_happy_human() {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "plan/happy.txt", &stripped);
}

#[test]
fn plan_happy_json() {
    // Pure data-roundtrip test on `PlanOutput` — drives the JSON path
    // through `Doc::with_data` without standing up a reconciler.
    let output = happy_plan_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(Doc::new().with_data(&output));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("plan doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(PlanOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "plan/happy.json");
}

#[test]
fn plan_json_owner_groups_payload() {
    // The owner-axis payload as a whole: group nesting, `owner`/`token`, the
    // `Owner::sort_key` group order (`profile:work` before `module:nvim`), the
    // alphabetical key order `serde_json`'s BTreeMap-backed `Map` produces, and
    // the absence — not emptiness — of `warnings`/`pendingBackups`.
    let output = owner_groups_plan_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(Doc::new().with_data(&output));
    drop(printer);

    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "plan/owner_groups.json");
}

#[test]
fn plan_json_exposes_action_target_paths() {
    // End-to-end through real `cmd_plan` (not the hand-built fixture): the
    // managed-file action's structured `targets` must carry the absolute
    // destination, so `-o json` consumers (CI, blast-radius tooling) read the
    // target without scraping the human `description`.
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    // A structured printer so `display_plan_preview` emits the data Doc
    // (`printer.is_structured()` gate) rather than human status lines.
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_plan(&cli, &printer, &plan_args()).unwrap();
    drop(printer);

    let payload = cap.json().expect("plan doc carries a payload");
    let files_phase = payload["phases"]
        .as_array()
        .expect("phases array")
        .iter()
        .find(|p| p["phase"] == "Files")
        .expect("a Files phase is planned");
    let targets = files_phase["groups"][0]["actions"][0]["targets"]
        .as_array()
        .expect("file action exposes a targets array");
    assert_eq!(
        targets,
        &vec![serde_json::json!(target.display().to_string())],
        "structured targets must equal the managed file's absolute destination"
    );
}

#[test]
fn plan_empty_human() {
    // Empty profile: zero managed files, zero modules — exercises the
    // `MSG_NOTHING_TO_DO` branch of `display_plan_preview`.
    let (config_dir, state_dir) = empty_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "plan/empty.txt", &stripped);
}

#[test]
fn plan_module_only_unresolved_module_errors() {
    // `--module` names an isolated run; it is resolved atomically (see
    // `resolve_desired_state`), so a name that never resolves (no module
    // repo configured here) now fails the whole invocation with a typed
    // "module not found" error rather than degrading to an empty plan and a
    // "matched no actions" warning at exit 0 — the swallow that produced
    // that render was removed. Nothing prints before the failure: the
    // module resolution runs before any plan output is emitted.
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // Bare config — no `spec.profile`, no profiles dir. An isolated
    // `--module` run never resolves a profile at all (see
    // `load_config_and_profile_module_scoped`), so this is otherwise inert.
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args_module("nettools");

    let err = cmd_plan(&cli, &printer, &args).unwrap_err();
    drop(printer);

    assert!(
        err.to_string().contains("module not found: nettools"),
        "expected a typed module-not-found error naming 'nettools', got: {err}"
    );
    assert!(
        cap.human().is_empty(),
        "an unresolved --module must fail before any plan output is emitted, got: {}",
        cap.human()
    );
}

#[test]
fn plan_only_zero_match_token_warns_and_names_owners_present_human() {
    // `--only` naming a token that matches nothing must never render
    // `MSG_NOTHING_TO_DO` (that would read as "the
    // machine is in sync", when really the filter just never matched
    // anything) — it renders the "No actions in scope" branch instead, plus
    // an always-visible alert naming the token verbatim and hinting the
    // owner tokens the plan actually held.
    let (config_dir, state_dir, _target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = cfgd::cli::PlanArgs {
        only: vec!["packages.brwe".to_string()],
        ..plan_args()
    };

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_ansi(&normalized);
    assert!(
        !stripped.contains("up to date") && !stripped.contains("nothing to do"),
        "a zero-match filter token must never render MSG_NOTHING_TO_DO, got:\n{stripped}"
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plan/only_zero_match.txt",
        &stripped
    );
}

#[test]
fn plan_with_a_decision_from_an_unsubscribed_source_human() {
    // A decision row whose source the config no longer lists withholds
    // nothing and is named nowhere: it is a row the operator cannot answer —
    // `cfgd decide` would act against a source that is gone — so the plan
    // renders exactly as it would with no decision at all. The block for a row
    // that IS live is asserted in `cli/tests.rs`
    // (`plan_preview_excludes_the_resource_its_pending_block_names` and
    // `plan_preview_names_the_decision_that_declined_a_resource`), where the
    // fixture can subscribe to a real source without a git clone's timing
    // landing in a golden.
    let (config_dir, state_dir, target) = state_with_pending_decision_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plan/with_inert_decision.txt",
        &stripped
    );
}

#[test]
fn plan_module_resolution_failure_human() {
    // `resolve_modules` narrates its walk through `Printer::narrate`, whose
    // FAILURE arm settles a permanent `Role::Fail` line at whatever module the
    // walk was on — a line that survives `Verbosity::Quiet` and a `-o json`
    // run. It is the one legitimately-permanent line the narration wave added,
    // and this golden is what pins it: nothing else in the corpus reaches a
    // failing module walk.
    //
    // The module declares a package no registered manager can satisfy
    // (`prefer:` names a manager that does not exist), so the failure is the
    // same on every host and reaches no real package manager.
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
    )
    .unwrap();
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - badmod\n",
    )
    .unwrap();
    let mod_dir = config_dir.path().join("modules").join("badmod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: badmod\nspec:\n  packages:\n    - name: nothing-provides-this\n      prefer:\n        - no-such-manager\n",
    )
    .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err =
        cmd_plan(&cli, &printer, &plan_args()).expect_err("an unresolvable package must fail");
    render_cli_error(&printer, &err);
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_spinner_duration(strip_ansi(&normalized));
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plan/module_resolution_failure.txt",
        &stripped
    );
}

#[test]
fn plan_module_package_already_installed_is_elided() {
    // The rendered pin for the elision: a module declaring two packages under
    // one manager that already reports one of them installed plans the OTHER
    // one only. Nothing else in the corpus captures a module package the
    // runner actually has, so a change that re-listed the whole declared set
    // would go red in zero snapshot binaries.
    //
    // `fakemgr` is a custom manager built from `echo`, which runs on every host
    // cfgd targets — the fixture describes the same machine everywhere and no
    // real package manager is reached.
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
    )
    .unwrap();
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - demo\n  packages:\n    custom:\n      - name: fakemgr\n        check: echo ok\n        listInstalled: echo ripgrep\n        install: echo install\n        uninstall: echo uninstall\n",
    )
    .unwrap();
    let mod_dir = config_dir.path().join("modules").join("demo");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: demo\nspec:\n  packages:\n    - name: ripgrep\n      prefer:\n        - fakemgr\n    - name: fd\n      prefer:\n        - fakemgr\n",
    )
    .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_plan(&cli, &printer, &plan_args()).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_spinner_duration(strip_ansi(&normalized));
    assert!(
        !stripped.contains("ripgrep"),
        "the installed package must not appear in the plan:\n{stripped}"
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plan/module_package_elided.txt",
        &stripped
    );
}

// ─────────────────────────────────────────────────────
// snapshot helpers — local to keep tests/output_snapshots/ self-contained
// ─────────────────────────────────────────────────────

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A run that composed a source names it in the header, so the reader can tell
/// which of the rows below arrived from a subscription rather than from their
/// own profile.
///
/// Every fact under that header is DECLARED rather than probed, because a
/// golden holds one render for every machine that runs it. The delivered
/// profile pins `envScope: Interactive`, which leaves the login and
/// session surfaces (`~/.profile`, `~/.zshenv`, `environment.d`, the macOS
/// LaunchAgent, the live-session publish) out of the plan — those are the
/// rows whose presence is a property of the *platform* — and the host probe
/// is pinned to a bash-only host with no fish, which is the row set's other
/// free variable. What is left is byte-identical on Linux, macOS and
/// FreeBSD. Windows renders a PowerShell surface instead of a POSIX one, so
/// the golden cannot cover it and the test is Unix-only.
#[cfg(unix)]
#[test]
fn plan_composed_source_human() {
    let _env = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    // The delivered profile writes env, whose targets hang off `$HOME`; an
    // unguarded test home is named after the pid and would not be host-stable.
    let home = tempfile::tempdir().unwrap();
    let _home = cfgd_core::with_test_home_guard(home.path());
    let _probe = cfgd_core::reconciler::with_env_host_probe_override_guard(
        cfgd_core::reconciler::EnvHostProbeOverride {
            shell: "/bin/bash".to_string(),
            fish_present: false,
            bash_profile_exists: false,
            bash_login_exists: false,
            git_bash_present: false,
            zsh_present: false,
        },
    );
    let (workspace, config_dir, state_dir) = common::local_source_setup("", |_workspace| {
        (
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: acme\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n".to_string(),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: ACME_EDITOR\n      value: vim\n".to_string(),
        )
    });

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_plan(&cli, &printer, &plan_args()).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(
        &cap.human(),
        config_dir.path(),
        &[
            (workspace.path(), "<WORKSPACE>"),
            (state_dir.path(), "<STATE_DIR>"),
            (home.path(), "<HOME>"),
        ],
    );
    let stripped = strip_spinner_duration(strip_ansi(&normalized));
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plan/composed_source.txt",
        &stripped
    );
}
