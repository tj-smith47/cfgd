use super::*;

pub(crate) fn cmd_profile_edit(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    let profile_path = profiles_dir(cli).join(format!("{}.yaml", name));
    if !profile_path.exists() {
        anyhow::bail!("Profile '{}' not found", name);
    }

    open_in_editor(&profile_path, printer)?;

    // Validate — loop until valid or user cancels
    loop {
        let contents = std::fs::read_to_string(&profile_path)?;
        match serde_yaml::from_str::<config::ProfileDocument>(&contents) {
            Ok(_) => {
                printer.success(&format!("Profile '{}' is valid", name));
                break;
            }
            Err(e) => {
                printer.error(&format!("Profile '{}' has errors: {}", name, e));
                if !printer.prompt_confirm("Re-open in editor?")? {
                    printer.warning("Saved with validation errors");
                    break;
                }
                open_in_editor(&profile_path, printer)?;
            }
        }
    }

    Ok(())
}
