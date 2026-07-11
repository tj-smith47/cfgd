use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

/// Prompt-gated removal of a payload-bearing directory. `--yes` (or a
/// confirmed prompt) removes it recursively; declining keeps it and notes
/// what was kept — the manifest deletion above already succeeded either way.
fn confirm_remove_payload_dir(printer: &Printer, yes: bool, dir: &Path) -> anyhow::Result<()> {
    if yes
        || printer.prompt_confirm(&format!(
            "Profile directory '{}' still contains payload files — remove it too?",
            dir.posix()
        ))?
    {
        std::fs::remove_dir_all(dir)?;
    } else {
        printer.status_simple(Role::Info, format!("Kept {}", dir.posix()));
    }
    Ok(())
}

pub fn cmd_profile_delete(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    yes: bool,
    ignore_not_found: bool,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    printer.heading(format!("Delete Profile: {}", name));

    let pdir = profiles_dir(cli);
    let profile_path = match cfgd_core::config::find_profile_path(&pdir, name) {
        Ok(p) => p,
        Err(e @ cfgd_core::errors::ConfigError::ProfileNotFound { .. }) => {
            if ignore_not_found {
                return crate::cli::emit_not_found_ignored(printer, "profile", name);
            }
            // Carry the typed ProfileNotFound so the exit-code downcast resolves
            // to ExitCode::NotFound (6). The active-profile guard below stays
            // exit 1 — it is a precondition failure, not a not-found.
            return Err(crate::cli::cli_error_ctx(
                cfgd_core::errors::CfgdError::Config(e).into(),
                name,
                "not_found",
                format!("Profile '{}' not found", name),
                serde_json::json!({}),
            ));
        }
        Err(e) => return Err(cfgd_core::errors::CfgdError::Config(e).into()),
    };
    let is_canonical = profile_path == cfgd_core::config::canonical_profile_path(&pdir, name);

    // Safety: refuse if active profile
    if cli.config.exists()
        && let Ok(cfg) = config::load_config(&cli.config)
        && cfg.spec.profile.as_deref() == Some(name)
    {
        return Err(crate::cli::cli_error(
            name,
            "active_profile",
            format!(
                "Cannot delete '{}' — it is the active profile. Switch first with: cfgd profile switch <other>",
                name
            ),
            serde_json::json!({}),
        ));
    }

    // Safety: refuse if inherited by other profiles
    let inheritors = profiles_inheriting(&pdir, name, printer)?;
    if !inheritors.is_empty() {
        return Err(crate::cli::cli_error(
            name,
            "inherited",
            format!(
                "Cannot delete '{}' — inherited by: {}",
                name,
                inheritors.join(", ")
            ),
            serde_json::json!({ "inheritors": inheritors }),
        ));
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

    // Clean up the profile's payload. `<name>/` belongs to the profile in both
    // layouts: canonical keeps profile.yaml inside it, legacy keeps files/.
    let profile_dir = pdir.join(name);
    if is_canonical {
        if profile_dir.is_dir() {
            let has_payload = std::fs::read_dir(&profile_dir)?.next().is_some();
            if !has_payload {
                std::fs::remove_dir(&profile_dir)?;
            } else {
                confirm_remove_payload_dir(printer, yes, &profile_dir)?;
            }
        }
    } else {
        // The payload is the same kind of thing in both layouts, so a
        // non-empty legacy files/ dir gets the same prompt gate as a
        // payload-bearing canonical dir; empty/absent stays silent cleanup.
        let files_dir = profile_dir.join("files");
        if files_dir.is_dir() {
            let has_payload = std::fs::read_dir(&files_dir)?.next().is_some();
            if !has_payload {
                std::fs::remove_dir(&files_dir)?;
            } else {
                confirm_remove_payload_dir(printer, yes, &files_dir)?;
            }
        }
        if profile_dir.is_dir() && std::fs::read_dir(&profile_dir)?.next().is_none() {
            std::fs::remove_dir(&profile_dir)?;
        }
    }

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Deleted profile '{}'", name))
            .with_data(serde_json::json!({
                "name": name,
                "cancelled": false,
            })),
    );

    update_workflow_best_effort(cli, printer);

    Ok(())
}
