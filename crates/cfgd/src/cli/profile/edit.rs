use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

pub(crate) fn cmd_profile_edit(
    cli: &Cli,
    v2_printer: &PrinterV2,
    name: &str,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    let profile_path = profiles_dir(cli).join(format!("{}.yaml", name));
    if !profile_path.exists() {
        anyhow::bail!("Profile '{}' not found", name);
    }

    open_in_editor_v2(&profile_path, v2_printer)?;

    let mut errors: Vec<String> = Vec::new();
    let valid = loop {
        let contents = std::fs::read_to_string(&profile_path)?;
        match serde_yaml::from_str::<config::ProfileDocument>(&contents) {
            Ok(_) => {
                errors.clear();
                break true;
            }
            Err(e) => {
                let msg = e.to_string();
                v2_printer.status_simple(
                    Role::Fail,
                    format!("Profile '{}' has errors: {}", name, msg),
                );
                errors.push(msg);
                if !v2_printer.prompt_confirm("Re-open in editor?")? {
                    break false;
                }
                open_in_editor_v2(&profile_path, v2_printer)?;
            }
        }
    };

    let doc = if valid {
        Doc::new()
            .status(Role::Ok, format!("Profile '{}' is valid", name))
            .with_data(serde_json::json!({
                "name": name,
                "valid": true,
                "errors": Vec::<String>::new(),
            }))
    } else {
        Doc::new()
            .status(Role::Warn, "Saved with validation errors")
            .with_data(serde_json::json!({
                "name": name,
                "valid": false,
                "errors": errors,
            }))
    };
    v2_printer.emit(doc);

    Ok(())
}
