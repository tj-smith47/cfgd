use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role, renderer::Table as TableV2};

pub fn cmd_source_remove(
    cli: &Cli,
    printer: &Printer,
    v2_printer: &PrinterV2,
    name: &str,
    keep_all: bool,
    remove_all: bool,
) -> anyhow::Result<()> {
    if keep_all && remove_all {
        v2_printer.emit(build_source_error_doc(
            name,
            "conflicting_flags",
            "cannot use --keep-all and --remove-all together",
            serde_json::Value::Null,
        ));
        anyhow::bail!("cannot use --keep-all and --remove-all together");
    }

    v2_printer.heading(format!("Remove Source: {}", name));

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if !cfg.spec.sources.iter().any(|s| s.name == name) {
        v2_printer.emit(build_source_error_doc(
            name,
            "not_found",
            format!("Source '{}' not found in config", name),
            serde_json::Value::Null,
        ));
        anyhow::bail!("Source '{}' not found in config", name);
    }

    let state = open_state_store(cli.state_dir.as_deref())?;
    let resources = state.managed_resources_by_source(name)?;

    let mut disposition = "removed";
    let mut managed_count = resources.len();

    if !resources.is_empty() && !keep_all && !remove_all {
        // Interactive: ask for each resource or batch
        {
            let res_sec = v2_printer.section(format!(
                "This source manages {} resource(s)",
                resources.len()
            ));
            let mut t = TableV2::new(["Type", "Resource"]);
            for r in &resources {
                t = t.row([r.resource_type.clone(), r.resource_id.clone()]);
            }
            res_sec.table(t);
        }

        let options = vec![
            "Keep all (resources become locally managed)".to_string(),
            "Remove all".to_string(),
        ];
        let choice = v2_printer.prompt_select("What to do with these resources?", &options)?;

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
            v2_printer.status_simple(Role::Info, "Resources transferred to local management");
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
        // Surface cache-removal failure to the v1 printer (matches the
        // pre-migration silent-on-error behavior — `let _ = …` previously).
        // Tracing-only because this side-effect is best-effort cleanup.
        let _ = printer;
        tracing::debug!("source cache removal failed for '{}': {}", name, e);
    }

    v2_printer.emit(
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
