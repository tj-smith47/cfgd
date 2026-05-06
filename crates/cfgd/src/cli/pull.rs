use super::*;

pub(super) fn cmd_pull(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Pull");

    let (_cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    printer.newline();

    match cfgd_core::daemon::git_pull_sync(&config_dir) {
        Ok(true) => printer.success("Pulled new changes from remote"),
        Ok(false) => printer.success("Already up to date"),
        Err(e) => printer.warning(&format!("Pull failed: {}", e)),
    }

    Ok(())
}
