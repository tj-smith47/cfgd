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

/// What a refused replay puts on the wire, rendered through the CLI's own
/// error sink rather than read off the carrier.
///
/// Each refusal names the question the file failed, so a script can tell
/// "re-plan" (`stale`) from "you named the wrong file" (`not_found`) from
/// "that run was filtered" (`no_saved_plan`). Written as bare `anyhow!`s,
/// all three reported the `internal` kind the sink falls back to for an
/// untyped failure.
#[test]
fn every_saved_plan_refusal_names_its_own_kind_on_the_wire() {
    fn payload_of(err: &anyhow::Error) -> serde_json::Value {
        let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
        let _ = cfgd::cli::error::render_cli_error(&printer, err);
        drop(printer);
        cap.json().expect("an error doc carries a payload")
    }

    let (config_dir, state_dir, _target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let printer = test_printer();

    let absent = state_dir.path().join("nope.json");
    let missing = payload_of(&run_apply(&cli, &printer, &replay_args(&absent)).unwrap_err());
    assert_eq!(missing["error"], "not_found", "{missing}");
    assert!(
        missing["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("nope.json")),
        "the payload names the file the caller passed: {missing}"
    );

    let filtered_file = state_dir.path().join("filtered.json");
    let mut filtered_args = plan_args();
    filtered_args.only = vec!["files".to_string()];
    record_plan_file(&cli, &filtered_args, &filtered_file);
    let filtered =
        payload_of(&run_apply(&cli, &printer, &replay_args(&filtered_file)).unwrap_err());
    assert_eq!(filtered["error"], "no_saved_plan", "{filtered}");

    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);
    {
        let state = StateStore::open(&state_dir.path().join("state.db")).unwrap();
        state
            .record_apply("tiny", "deadbeef", ApplyStatus::Success, None)
            .unwrap();
    }
    let stale = payload_of(&run_apply(&cli, &printer, &replay_args(&plan_file)).unwrap_err());
    assert_eq!(stale["error"], "stale", "{stale}");
    assert_eq!(stale["serial"], 1, "{stale}");
    assert_eq!(
        stale["recordedSerial"], 0,
        "both serials reach the payload: {stale}"
    );
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

#[test]
fn a_plan_recorded_under_another_config_is_refused() {
    // A plan file names no config of its own, so a global `--config` beside
    // `--plan` would run one config's recorded actions while the header, the
    // resolved modules and the `applies` row all came from another.
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);

    // Both staleness facts are made true first, so the assertion below reads
    // the ORDER and not just the refusal: the config the derivation read is
    // moved, and an apply is recorded against the serial the plan carries.
    // With the identity question asked after either of them, a different
    // sentence prints.
    let profile = config_dir.path().join("profiles").join("tiny.yaml");
    let body = std::fs::read_to_string(&profile).unwrap();
    std::fs::write(&profile, format!("{body}# the operator edited this\n")).unwrap();
    {
        let state = StateStore::open(&state_dir.path().join("state.db")).unwrap();
        let id = state
            .record_apply("tiny", "deadbeef", ApplyStatus::Success, None)
            .unwrap();
        assert_eq!(id, 1, "the first recorded apply is #1");
    }

    let (other_dir, _other_state, other_target) = tiny_profile_setup();
    let foreign = cli_for(other_dir.path(), state_dir.path());

    let printer = test_printer();
    let err = run_apply(&foreign, &printer, &replay_args(&plan_file))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("is not a plan cfgd wrote for this config"),
        "a foreign config is a shape refusal, not a staleness one: {err}"
    );
    assert!(
        !err.contains("is stale"),
        "the shape refusal precedes both staleness facts, which are also true here: {err}"
    );
    assert!(
        err.contains(&other_dir.path().join("cfgd.yaml").display().to_string()),
        "the refusal names the config this run resolved, which both fixtures call \
         `cfgd.yaml`: {err}"
    );
    assert!(!target.exists(), "a refused plan runs nothing: {err}");
    assert!(
        !other_target.exists(),
        "nor against the other config: {err}"
    );
}

#[test]
fn a_relative_spelling_of_the_same_config_replays_the_plan() {
    // The identity question above is asked about the FILE, not about the
    // string: a `..` walking back through a component names the same config
    // the derivation read, and must not refuse.
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);

    // `files/` is created by the fixture, so the walk-back opens.
    let respelled = Cli {
        config: config_dir.path().join("files").join("..").join("cfgd.yaml"),
        ..cli_for(config_dir.path(), state_dir.path())
    };
    let printer = test_printer();
    let outcome = run_apply(&respelled, &printer, &replay_args(&plan_file)).unwrap();

    assert_eq!(outcome.status, ApplyStatus::Success);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello world");
}

#[test]
fn a_replay_records_its_applies_row_under_the_profile_the_plan_was_written_for() {
    // The config-identity refusal above is what makes this true: the replay
    // resolves the same config the plan was derived from, so the scope it
    // records cannot be another machine picture's profile.
    let (config_dir, state_dir, _target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    record_plan_file(&cli, &plan_args(), &plan_file);

    let printer = test_printer();
    run_apply(&cli, &printer, &replay_args(&plan_file)).unwrap();

    let state = StateStore::open(&state_dir.path().join("state.db")).unwrap();
    let recorded = state
        .last_apply()
        .unwrap()
        .expect("the replay recorded an apply");
    assert_eq!(recorded.profile, "tiny");
}

#[test]
fn a_replay_runs_under_the_context_the_plan_recorded() {
    // `--context` is clap-refused beside `--plan`, so the file is the only
    // thing that can answer it, and the answer decides which hooks run at
    // execute time.
    let (config_dir, state_dir, _target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let plan_file = state_dir.path().join("plan.json");
    let mut args = plan_args();
    args.context = "reconcile".to_string();
    record_plan_file(&cli, &args, &plan_file);

    let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
    let mut replay = replay_args(&plan_file);
    replay.dry_run = true;
    run_apply(&cli, &printer, &replay).unwrap();
    drop(printer);

    let payload = cap.json().expect("the replay emits a payload");
    assert_eq!(
        payload["context"],
        serde_json::json!("reconcile"),
        "the saved context wins over the flag's default: {payload}"
    );
}

#[test]
fn a_missing_plan_file_is_refused() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let absent = state_dir.path().join("nope.json");

    let printer = test_printer();
    let err = run_apply(&cli, &printer, &replay_args(&absent))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot read plan file") && err.contains("nope.json"),
        "the likeliest operator error names the path: {err}"
    );
    assert!(!target.exists(), "a refused plan runs nothing: {err}");
}

#[test]
fn an_unparsable_plan_file_is_refused() {
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let garbage = state_dir.path().join("garbage.json");
    std::fs::write(&garbage, "{\n").unwrap();

    let printer = test_printer();
    let err = run_apply(&cli, &printer, &replay_args(&garbage))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("is not the payload of `cfgd plan -o json`"),
        "{err}"
    );
    assert!(!target.exists(), "a refused plan runs nothing: {err}");
}

#[test]
fn a_json_document_that_is_no_plan_output_is_refused_as_one() {
    // Valid JSON cfgd never wrote. It earns its own sentence: the
    // filtered-run explanation names causes that cannot apply to it.
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    // The question reads BOTH keys, so a document carrying exactly one of them
    // is what holds the refusal's wording honest: "neither" is false of it.
    for (name, body) in [
        ("stranger.json", r#"{"context":"apply"}"#),
        ("one-key.json", r#"{"phases":[],"context":"apply"}"#),
    ] {
        let stranger = state_dir.path().join(name);
        std::fs::write(&stranger, body).unwrap();

        let printer = test_printer();
        let err = run_apply(&cli, &printer, &replay_args(&stranger))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("is not the payload of `cfgd plan -o json`"),
            "{err}"
        );
        assert!(
            err.contains(
                "it does not carry both a `phases` and a `totalActions` key, which every plan \
                 output has"
            ),
            "the refusal states the pair it needs, of a document holding one of them too: {err}"
        );
        assert!(
            !err.contains("carries no saved plan"),
            "only a plan cfgd wrote earns the filtered-run explanation: {err}"
        );
        assert!(!target.exists(), "a refused plan runs nothing: {err}");
    }
}

/// Every argument `cfgd apply` takes is either refused beside `--plan` or one
/// of the named execution knobs. `conflicts_with_all` and the enumeration in
/// `a_filter_is_refused_with_a_plan_file` are both hand lists, so a selector
/// ADDED to `ApplyArgs` would join neither and pass forever; this walks clap's
/// own argument list instead.
#[test]
fn every_apply_arg_is_refused_with_a_plan_file_or_is_an_execution_knob() {
    use clap::{CommandFactory, Parser};

    // These say HOW the run behaves, not WHAT it does, which is the file's to
    // say. `plan` itself is not an argument of the run. The list holds only
    // ids clap really declares here: `--yes` is global (`from_global`) and
    // `--help`/`--version` belong to the root command, so none of the three
    // reaches this walk, and naming them would exempt a future argument that
    // took one of those ids.
    const EXECUTION_KNOBS: [&str; 4] = ["plan", "dry_run", "shell", "on_conflict"];

    let command = ApplyArgs::command();
    let args: Vec<_> = command.get_arguments().collect();
    assert!(
        args.len() >= 12,
        "the walk read clap's real argument list: {}",
        args.len()
    );
    let mut refused = 0;
    let mut knobs_seen = Vec::new();
    for arg in args {
        let id = arg.get_id().as_str();
        if EXECUTION_KNOBS.contains(&id) {
            knobs_seen.push(id.to_string());
            continue;
        }
        let long = arg
            .get_long()
            .unwrap_or_else(|| panic!("`{id}` takes no long flag, so it cannot be refused"));
        let flag = format!("--{long}");
        let mut argv = vec!["cfgd", "apply", "--plan", "p.json", flag.as_str()];
        if matches!(
            arg.get_action(),
            clap::ArgAction::Set | clap::ArgAction::Append
        ) {
            argv.push("files");
        }
        let err = Cli::try_parse_from(&argv)
            .err()
            .unwrap_or_else(|| panic!("`{flag}` must be refused with --plan"));
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "`{flag}` is refused as a conflict, not for some other reason: {err}"
        );
        refused += 1;
    }
    assert!(
        refused >= 8,
        "every selector the command declares was walked: {refused}"
    );
    for knob in EXECUTION_KNOBS {
        assert!(
            knobs_seen.iter().any(|seen| seen == knob),
            "`{knob}` names no argument this walk saw, so it exempts nothing and \
             hides the next argument that takes its id: {knobs_seen:?}"
        );
    }
}
