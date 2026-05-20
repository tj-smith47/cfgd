use super::*;
use cfgd_core::output::{Doc, Printer as PrinterV2, Role};

pub fn cmd_source_edit(cli: &Cli, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let source_path = config_dir.join("cfgd-source.yaml");
    if !source_path.exists() {
        v2_printer.emit(cfgd_core::output::error_doc(
            "cfgd-source.yaml",
            "no_config",
            format!(
                "No cfgd-source.yaml found in {} — run 'cfgd source create' to scaffold one",
                config_dir.display()
            ),
            serde_json::json!({ "dir": config_dir.display().to_string() }),
        ));
        anyhow::bail!(
            "No cfgd-source.yaml found in {} — run 'cfgd source create' to scaffold one",
            config_dir.display()
        );
    }

    open_in_editor_v2(&source_path, v2_printer)?;

    // Validate after editing — loop until valid or user cancels
    loop {
        let contents = std::fs::read_to_string(&source_path)?;
        match config::parse_config_source(&contents) {
            Ok(_) => {
                v2_printer.emit(
                    Doc::new()
                        .status(Role::Ok, "Source manifest is valid")
                        .with_data(serde_json::json!({
                            "path": source_path.display().to_string(),
                            "valid": true,
                        })),
                );
                break;
            }
            Err(e) => {
                v2_printer.status_simple(Role::Fail, format!("Invalid source manifest: {}", e));
                if !v2_printer.prompt_confirm("Re-open in editor to fix?")? {
                    v2_printer.emit(
                        Doc::new()
                            .status(Role::Warn, "Saved with validation errors")
                            .with_data(serde_json::json!({
                                "path": source_path.display().to_string(),
                                "valid": false,
                            })),
                    );
                    break;
                }
                open_in_editor_v2(&source_path, v2_printer)?;
            }
        }
    }

    Ok(())
}
