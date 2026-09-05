use super::*;

use cfgd_core::daemon::PullOutcome;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_pull(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.heading("Pull");

    let (_cfg, _profile_name, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    let outcome = cfgd_core::daemon::git_pull_sync(&config_dir);
    let refused = matches!(outcome, PullOutcome::Failed(_));
    render_pull(printer, outcome);

    // The same operation `cfgd sync` runs as one of its legs, so the two
    // commands answer a refused pull with the same exit code; a wrapper
    // chaining on `cfgd pull &&` must not read a refusal as a completed pull.
    if refused {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}

/// Render the streaming spinner + buffered Doc for a pull outcome. Heading is
/// emitted by the caller so this helper composes inside both real `cmd_pull`
/// and snapshot tests that stub the outcome.
pub fn render_pull(printer: &Printer, outcome: PullOutcome) {
    // A directory under no version control has nothing to fetch, so nothing
    // animates: the spinner opens only over work that can happen.
    if outcome == PullOutcome::NotARepository {
        printer.status_simple(Role::Skipped, MSG_NOT_A_REPOSITORY);
        printer.emit(build_pull_doc(&PullOutput {
            status: "not_a_repository".to_string(),
            error: None,
        }));
        return;
    }

    let sp = printer.spinner("Pulling from remote");
    let (status, err) = match outcome {
        PullOutcome::Moved(_) => {
            sp.finish_ok("Pulled new changes from remote");
            ("pulled", None)
        }
        // verdict-row-ok: nothing was fetched; this reports the checkout's state
        PullOutcome::UpToDate => {
            sp.finish_ok("Already up to date");
            ("up_to_date", None)
        }
        PullOutcome::Failed(e) => {
            sp.finish_warn("Pull failed")
                .detail(cfgd_core::daemon::pull_failure_summary(&e.message));
            printer.hint(local_pull_next_step(&e, "cfgd pull"));
            ("failed", Some(e.message))
        }
        // Returned above; the arm exists so a new variant is classified here.
        PullOutcome::NotARepository => ("not_a_repository", None),
    };

    printer.emit(build_pull_doc(&PullOutput {
        status: status.to_string(),
        error: err,
    }));
}

/// Build the buffered `Doc` that carries the final `PullOutput` payload.
/// Pure function so snapshot tests can drive the JSON path without standing
/// up a git remote.
pub fn build_pull_doc(output: &PullOutput) -> Doc {
    Doc::new().with_data(output)
}
