use super::*;

use cfgd_core::output::{Doc, Printer as PrinterV2, Role};

pub fn cmd_pull(cli: &Cli, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    v2_printer.heading("Pull");

    let (_cfg, _profile_name, _resolved) = load_config_and_profile_v2(cli)?;
    let config_dir = config_dir(cli);

    let pull_result = cfgd_core::daemon::git_pull_sync(&config_dir);
    render_pull(v2_printer, &pull_result);

    Ok(())
}

/// Render the streaming spinner + buffered Doc for a `git_pull_sync` result.
/// Heading is emitted by the caller so this helper composes inside both real
/// `cmd_pull` and snapshot tests that stub the result.
pub fn render_pull(v2_printer: &PrinterV2, pull_result: &Result<bool, String>) {
    let sp = v2_printer.spinner("Pulling from remote");
    let (role, msg, status, err) = pull_status_from_result(pull_result);
    match role {
        Role::Ok => {
            sp.finish_ok(msg.to_string());
        }
        Role::Warn => {
            sp.finish_warn(msg.to_string())
                .detail(err.clone().unwrap_or_default());
        }
        _ => unreachable!(),
    }

    v2_printer.emit(build_pull_doc(&PullOutput {
        status: status.to_string(),
        error: err,
    }));
}

fn pull_status_from_result(
    result: &Result<bool, String>,
) -> (Role, &'static str, &'static str, Option<String>) {
    match result {
        Ok(true) => (Role::Ok, "Pulled new changes from remote", "pulled", None),
        Ok(false) => (Role::Ok, "Already up to date", "up_to_date", None),
        Err(e) => (Role::Warn, "Pull failed", "failed", Some(e.clone())),
    }
}

/// Build the buffered `Doc` that carries the final `PullOutput` payload.
/// Pure function so snapshot tests can drive the JSON path without standing
/// up a git remote.
pub fn build_pull_doc(output: &PullOutput) -> Doc {
    Doc::new().with_data(output)
}
