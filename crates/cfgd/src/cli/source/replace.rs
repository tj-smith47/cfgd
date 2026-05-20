use super::*;
use cfgd_core::output::{Doc, Printer as PrinterV2, Role};

pub fn cmd_source_replace(
    cli: &Cli,
    v2_printer: &PrinterV2,
    old_name: &str,
    new_url: &str,
) -> anyhow::Result<()> {
    v2_printer.heading(format!("Replace Source: {}", old_name));

    // Capture old source's profile and priority before removing
    let config_path = cli.config.clone();
    let old_cfg = config::load_config(&config_path)?;
    let old_source = old_cfg.spec.sources.iter().find(|s| s.name == old_name);
    let old_profile = old_source.and_then(|s| s.subscription.profile.clone());
    let old_priority = old_source.map(|s| s.subscription.priority).unwrap_or(500);

    // Remove old source (keeping resources)
    cmd_source_remove(cli, v2_printer, old_name, true, false)?;

    // Add new source with same name, carrying over profile and priority
    cmd_source_add(
        cli,
        v2_printer,
        &SourceAddArgs {
            url: new_url.to_string(),
            name: Some(old_name.to_string()),
            branch: None,
            profile: old_profile,
            accept_recommended: false,
            priority: Some(old_priority),
            opt_in: vec![],
            sync_interval: None,
            auto_apply: false,
            version_pin: None,
            yes: true,
        },
    )?;

    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Source '{}' replaced with {}", old_name, new_url),
            )
            .with_data(serde_json::json!({
                "oldName": old_name,
                "newUrl": new_url,
            })),
    );
    Ok(())
}
