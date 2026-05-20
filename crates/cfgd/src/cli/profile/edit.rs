use super::*;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_profile_edit(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    let profile_path = profiles_dir(cli).join(format!("{}.yaml", name));
    if !profile_path.exists() {
        printer.emit(cfgd_core::output::error_doc(
            name,
            "not_found",
            format!("Profile '{}' not found", name),
            serde_json::Value::Null,
        ));
        anyhow::bail!("Profile '{}' not found", name);
    }

    open_in_editor(&profile_path, printer)?;

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
                printer.status_simple(
                    Role::Fail,
                    format!("Profile '{}' has errors: {}", name, msg),
                );
                errors.push(msg);
                if !printer.prompt_confirm("Re-open in editor?")? {
                    break false;
                }
                open_in_editor(&profile_path, printer)?;
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
    printer.emit(doc);

    Ok(())
}
