//! `cfgd apply --plan <file>`: the replay, and the refusals.
//!
//! Every plan file here is produced by the real `cmd_plan` under `-o json`, so
//! the producer and the consumer are pinned against each other rather than
//! against a hand-written payload.

mod common;

use std::path::Path;

use cfgd::cli::apply::run_apply;
use cfgd::cli::plan::cmd_plan;
use cfgd::cli::{ApplyArgs, Cli, PlanArgs};
use cfgd_core::output::{OutputFormat, Printer};
use cfgd_core::state::{ApplyStatus, StateStore};
use cfgd_core::test_helpers::test_printer;

use common::{apply_args, cli_for, plan_args, tiny_profile_setup};

/// Write what `cfgd plan -o json` produced to `dest`, exactly as a shell
/// redirect would. `dest` is in the STATE directory, never the config
/// directory: a file written inside the config directory is itself a change to
/// what the derivation read, which is the very thing the refusal tests move.
fn record_plan_file(cli: &Cli, args: &PlanArgs, dest: &Path) {
    let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
    cmd_plan(cli, &printer, args).unwrap();
    drop(printer);
    let payload = cap.json().expect("plan doc carries a payload");
    std::fs::write(dest, serde_json::to_string(&payload).unwrap()).unwrap();
}

/// `ApplyArgs` for a replay of `path` — `apply_args()`'s defaults, plus the file.
fn replay_args(path: &Path) -> ApplyArgs {
    ApplyArgs {
        plan: Some(path.to_path_buf()),
        ..apply_args()
    }
}

#[test]
fn a_saved_plan_applies_the_actions_it_recorded() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);
    assert!(!target.exists(), "the plan itself changes nothing");

    let printer = test_printer();
    let outcome = run_apply(&cli, &printer, &replay_args(&plan_file)).unwrap();

    assert_eq!(outcome.status, ApplyStatus::Success);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "hello world",
        "the recorded file action ran: {}",
        target.display()
    );
}

#[test]
fn a_replay_runs_the_files_own_actions_rather_than_planning_again() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);

    // Aim the recorded deploy somewhere the config never names. A run that
    // planned again would write `target`; a replay writes this.
    let renamed = config_dir.path().join("out").join("renamed.txt");
    let body = std::fs::read_to_string(&plan_file)
        .unwrap()
        .replace("out/hello.txt", "out/renamed.txt");
    std::fs::write(&plan_file, body).unwrap();

    let printer = test_printer();
    run_apply(&cli, &printer, &replay_args(&plan_file)).unwrap();

    assert_eq!(
        std::fs::read_to_string(&renamed).unwrap(),
        "hello world",
        "the file's own action ran: {}",
        renamed.display()
    );
    assert!(
        !target.exists(),
        "nothing re-planned the profile: {}",
        target.display()
    );
}

#[test]
fn a_saved_plan_is_refused_once_the_config_moves() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);

    // The recorded stamp is (mtime, len), so APPENDING moves it whatever the
    // filesystem's timestamp granularity is — a same-length rewrite inside one
    // mtime tick would not, and this test is about the refusal, not the probe.
    let profile = config_dir.path().join("profiles").join("tiny.yaml");
    let body = std::fs::read_to_string(&profile).unwrap();
    std::fs::write(&profile, format!("{body}# the operator edited this\n")).unwrap();

    let printer = test_printer();
    let err = run_apply(&cli, &printer, &replay_args(&plan_file))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("plan.json is stale"),
        "the refusal names the file: {err}"
    );
    assert!(
        err.contains("tiny.yaml"),
        "the refusal names what moved: {err}"
    );
    assert!(err.contains("`cfgd plan -o json`"), "{err}");
    assert!(!target.exists(), "a refused plan runs nothing: {err}");
}

#[test]
fn a_saved_plan_is_refused_once_an_apply_has_run() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);

    // Someone applied in the window between the review and the replay. The
    // handle is dropped before the apply opens the same database.
    {
        let state = StateStore::open(&state_dir.path().join("state.db")).unwrap();
        let id = state
            .record_apply("tiny", "deadbeef", ApplyStatus::Success, None)
            .unwrap();
        assert_eq!(id, 1, "the first recorded apply is #1");
    }

    let printer = test_printer();
    let err = run_apply(&cli, &printer, &replay_args(&plan_file))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("plan.json is stale"),
        "the refusal names the file: {err}"
    );
    assert!(err.contains("apply #1 has run"), "{err}");
    assert!(
        err.contains("it recorded #0"),
        "the refusal names both serials: {err}"
    );
    assert!(!target.exists(), "a refused plan runs nothing: {err}");
}

#[test]
fn a_filtered_payload_is_refused_as_a_plan_file() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("filtered.json");
    let mut args = plan_args();
    args.only = vec!["files".to_string()];
    record_plan_file(&cli, &args, &plan_file);

    let printer = test_printer();
    let err = run_apply(&cli, &printer, &replay_args(&plan_file))
        .unwrap_err()
        .to_string();
    assert!(err.contains("carries no saved plan"), "{err}");
    assert!(
        err.contains("--only"),
        "the refusal names the filters that suppress the recording: {err}"
    );
    assert!(!target.exists(), "a refused plan runs nothing: {err}");
}

#[test]
fn a_plan_file_whose_phases_were_reordered_is_refused() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);

    // A hand-edited file, which is what `--plan` has to survive: the phases
    // are duplicated, so the recorded file deploy would run twice.
    let mut payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&plan_file).unwrap()).unwrap();
    let phases = payload["savedPlan"]["plan"]["phases"]
        .as_array()
        .expect("the recorded plan carries phases")
        .clone();
    assert_eq!(phases.len(), 1, "the fixture plans one phase: {phases:?}");
    payload["savedPlan"]["plan"]["phases"] =
        serde_json::json!([phases[0].clone(), phases[0].clone()]);
    std::fs::write(&plan_file, serde_json::to_string(&payload).unwrap()).unwrap();

    let printer = test_printer();
    let err = run_apply(&cli, &printer, &replay_args(&plan_file))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("is not a plan cfgd wrote"),
        "a duplicated phase is a shape refusal, not a staleness one: {err}"
    );
    assert!(err.contains("Files"), "the refusal names the phases: {err}");
    assert!(!target.exists(), "a refused plan runs nothing: {err}");
}

#[test]
fn a_filter_is_refused_with_a_plan_file() {
    // clap owns the refusal, and `--context`'s DEFAULT must not trip it.
    use clap::Parser;
    assert!(Cli::try_parse_from(["cfgd", "apply", "--plan", "p.json"]).is_ok());
    for filter in [
        ["--only", "files"],
        ["--skip", "files"],
        ["--module", "nvim"],
        ["--phase", "files"],
        ["--from", "acme/config"],
        ["--context", "reconcile"],
    ] {
        let bad = Cli::try_parse_from(["cfgd", "apply", "--plan", "p.json", filter[0], filter[1]]);
        assert!(bad.is_err(), "`{}` must be refused with --plan", filter[0]);
    }
    for flag in ["--skip-scripts", "--with-profile"] {
        let bad = Cli::try_parse_from(["cfgd", "apply", "--plan", "p.json", flag]);
        assert!(bad.is_err(), "`{flag}` must be refused with --plan");
    }
    // The execution knobs stay legal: they say HOW this run behaves, not what
    // it does, which is the file's to say.
    assert!(Cli::try_parse_from(["cfgd", "apply", "--plan", "p.json", "--dry-run"]).is_ok());
    assert!(Cli::try_parse_from(["cfgd", "apply", "--plan", "p.json", "--yes"]).is_ok());
    assert!(
        Cli::try_parse_from(["cfgd", "apply", "--plan", "p.json", "--on-conflict", "skip"]).is_ok()
    );
}
