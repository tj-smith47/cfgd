use super::*;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_profile_delete(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    yes: bool,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    printer.heading(format!("Delete Profile: {}", name));

    let config_dir = config_dir(cli);
    let pdir = profiles_dir(cli);
    let profile_path = pdir.join(format!("{}.yaml", name));

    if !profile_path.exists() {
        printer.emit(cfgd_core::output::error_doc(
            name,
            "not_found",
            format!("Profile '{}' not found", name),
            serde_json::Value::Null,
        ));
        anyhow::bail!("Profile '{}' not found", name);
    }

    // Safety: refuse if active profile
    if cli.config.exists()
        && let Ok(cfg) = config::load_config(&cli.config)
        && cfg.spec.profile.as_deref() == Some(name)
    {
        printer.emit(cfgd_core::output::error_doc(
            name,
            "active_profile",
            format!(
                "Cannot delete '{}' — it is the active profile. Switch first with: cfgd profile switch <other>",
                name
            ),
            serde_json::Value::Null,
        ));
        anyhow::bail!(
            "Cannot delete '{}' — it is the active profile. Switch first with: cfgd profile switch <other>",
            name
        );
    }

    // Safety: refuse if inherited by other profiles
    let inheritors = profiles_inheriting(&pdir, name)?;
    if !inheritors.is_empty() {
        printer.emit(cfgd_core::output::error_doc(
            name,
            "inherited",
            format!(
                "Cannot delete '{}' — inherited by: {}",
                name,
                inheritors.join(", ")
            ),
            serde_json::json!({ "inheritors": inheritors }),
        ));
        anyhow::bail!(
            "Cannot delete '{}' — inherited by: {}",
            name,
            inheritors.join(", ")
        );
    }

    if !yes && !printer.prompt_confirm(&format!("Delete profile '{}'?", name))? {
        printer.emit(
            Doc::new()
                .status(Role::Info, "Cancelled")
                .with_data(serde_json::json!({
                    "name": name,
                    "cancelled": true,
                })),
        );
        return Ok(());
    }

    std::fs::remove_file(&profile_path)?;

    // Clean up files directory if it exists (new layout)
    let files_dir = config_dir.join("profiles").join(name).join("files");
    if files_dir.exists() {
        std::fs::remove_dir_all(&files_dir)?;
    }

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Deleted profile '{}'", name))
            .with_data(serde_json::json!({
                "name": name,
                "cancelled": false,
            })),
    );

    maybe_update_workflow(cli, printer)?;

    Ok(())
}
