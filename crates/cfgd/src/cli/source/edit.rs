use std::path::Path;

use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_source_edit(printer: &Printer, dir: &Path) -> anyhow::Result<()> {
    let source_path = dir.join("cfgd-source.yaml");
    if !source_path.exists() {
        return Err(crate::cli::cli_error(
            "cfgd-source.yaml",
            "no_config",
            format!(
                "No cfgd-source.yaml found in {} — run 'cfgd source create' to scaffold one",
                dir.posix()
            ),
            serde_json::json!({ "dir": cfgd_core::to_posix_string(dir) }),
        ));
    }

    open_in_editor(&source_path, printer)?;

    // Validate after editing — loop until valid or user cancels
    loop {
        let contents = std::fs::read_to_string(&source_path)?;
        match config::parse_config_source(&contents) {
            Ok(_) => {
                printer.emit(
                    Doc::new()
                        .status(Role::Ok, "Source manifest is valid")
                        .with_data(serde_json::json!({
                            "path": cfgd_core::to_posix_string(&source_path),
                            "valid": true,
                        })),
                );
                break;
            }
            Err(e) => {
                printer.status_simple(
                    Role::Fail,
                    format!(
                        "Invalid source manifest: {}",
                        cfgd_core::output::collapse_to_subject_line(&e),
                    ),
                );
                if !printer.prompt_confirm("Re-open in editor to fix?")? {
                    printer.emit(
                        Doc::new()
                            .status(Role::Warn, "Saved with validation errors")
                            .with_data(serde_json::json!({
                                "path": cfgd_core::to_posix_string(&source_path),
                                "valid": false,
                            })),
                    );
                    break;
                }
                open_in_editor(&source_path, printer)?;
            }
        }
    }

    Ok(())
}
