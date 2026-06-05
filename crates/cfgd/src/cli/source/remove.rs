use super::*;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

pub fn cmd_source_remove(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    keep_all: bool,
    remove_all: bool,
) -> anyhow::Result<()> {
    if keep_all && remove_all {
        return Err(crate::cli::cli_error(
            name,
            "conflicting_flags",
            "cannot use --keep-all and --remove-all together",
            serde_json::json!({}),
        ));
    }

    printer.heading(format!("Remove Source: {}", name));

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if !cfg.spec.sources.iter().any(|s| s.name == name) {
        return Err(crate::cli::cli_error(
            name,
            "not_found",
            format!("Source '{}' not found in config", name),
            serde_json::json!({}),
        ));
    }

    let state = open_state_store(cli.state_dir.as_deref())?;
    let resources = state.managed_resources_by_source(name)?;

    let mut disposition = "removed";
    let mut managed_count = resources.len();

    if !resources.is_empty() && !keep_all && !remove_all {
        // Interactive: Keep / Remove / Cancel
        {
            let res_sec = printer.section(format!(
                "This source manages {} resource(s)",
                resources.len()
            ));
            let mut t = Table::new(["Type", "Resource"]);
            for r in &resources {
                t = t.row([r.resource_type.clone(), r.resource_id.clone()]);
            }
            res_sec.table(t);
        }

        let options = vec![
            "Keep all (resources become locally managed)".to_string(),
            "Remove all".to_string(),
            "Cancel (abort remove)".to_string(),
        ];
        let choice = printer.prompt_select("What to do with these resources?", &options)?;

        if choice.starts_with("Cancel") {
            printer.emit(
                Doc::new()
                    .status(Role::Info, "Cancelled — source not removed")
                    .with_data(serde_json::json!({
                        "name": name,
                        "managedResources": managed_count,
                        "cancelled": true,
                    })),
            );
            return Ok(());
        }

        if choice.starts_with("Keep") {
            // Re-assign resources to local
            for r in &resources {
                state.upsert_managed_resource(
                    &r.resource_type,
                    &r.resource_id,
                    "local",
                    r.last_hash.as_deref(),
                    r.last_applied,
                )?;
            }
            printer.status_simple(Role::Info, "Resources transferred to local management");
            disposition = "kept";
        } else {
            disposition = "purged";
        }
        // If "Remove all", they'll be cleaned up when state is updated
    } else if keep_all {
        for r in &resources {
            state.upsert_managed_resource(
                &r.resource_type,
                &r.resource_id,
                "local",
                r.last_hash.as_deref(),
                r.last_applied,
            )?;
        }
        disposition = "kept";
    } else if remove_all {
        disposition = "purged";
    } else {
        // No managed resources — neutral disposition
        managed_count = 0;
    }

    // Remove from config
    remove_source_from_config(&config_path, name)?;

    // Remove from state
    state.remove_config_source(name)?;
    state.remove_source_config_hash(name)?;

    // Remove cached data
    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    if let Err(e) = mgr.remove_source(name) {
        // Best-effort cleanup: cache-removal failure is non-fatal because
        // the config + state mutations already landed. Surface to tracing
        // so operators can investigate stuck cache dirs without polluting
        // the user-visible removal-success Doc.
        tracing::debug!("source cache removal failed for '{}': {}", name, e);
    }

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Source '{}' removed", name))
            .with_data(serde_json::json!({
                "name": name,
                "managedResources": managed_count,
                "disposition": disposition,
                "cancelled": false,
            })),
    );
    Ok(())
}
