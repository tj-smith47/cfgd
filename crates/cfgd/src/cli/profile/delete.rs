use super::*;

pub(crate) fn cmd_profile_delete(
    cli: &Cli,
    printer: &Printer,
    v2_printer: &cfgd_core::output_v2::Printer,
    name: &str,
    yes: bool,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    printer.header(&format!("Delete Profile: {}", name));

    let config_dir = config_dir(cli);
    let pdir = profiles_dir(cli);
    let profile_path = pdir.join(format!("{}.yaml", name));

    if !profile_path.exists() {
        anyhow::bail!("Profile '{}' not found", name);
    }

    // Safety: refuse if active profile
    if cli.config.exists()
        && let Ok(cfg) = config::load_config(&cli.config)
        && cfg.spec.profile.as_deref() == Some(name)
    {
        anyhow::bail!(
            "Cannot delete '{}' — it is the active profile. Switch first with: cfgd profile switch <other>",
            name
        );
    }

    // Safety: refuse if inherited by other profiles
    let inheritors = profiles_inheriting(&pdir, name)?;
    if !inheritors.is_empty() {
        anyhow::bail!(
            "Cannot delete '{}' — inherited by: {}",
            name,
            inheritors.join(", ")
        );
    }

    if !yes && !printer.prompt_confirm(&format!("Delete profile '{}'?", name))? {
        printer.info("Cancelled");
        return Ok(());
    }

    std::fs::remove_file(&profile_path)?;

    // Clean up files directory if it exists (new layout)
    let files_dir = config_dir.join("profiles").join(name).join("files");
    if files_dir.exists() {
        std::fs::remove_dir_all(&files_dir)?;
    }

    printer.success(&format!("Deleted profile '{}'", name));

    maybe_update_workflow(cli, v2_printer)?;

    Ok(())
}
