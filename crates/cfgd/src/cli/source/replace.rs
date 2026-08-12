use super::*;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::reconciler::Owner;

pub fn cmd_source_replace(
    cli: &Cli,
    printer: &Printer,
    old_name: &str,
    new_url: &str,
) -> anyhow::Result<()> {
    // Resolved here as well as in `cmd_source_add` (resolution is idempotent) so
    // the success line and its structured payload report the URL that was
    // actually subscribed to, not the shorthand.
    let new_url = &*cfgd_core::resolve_repo_reference(new_url);
    printer.heading(format!("Replace {}", Owner::source(old_name).token()));

    // Capture old source's profile and priority before removing
    let config_path = cli.config.clone();
    let mut old_cfg = config::load_config(&config_path)?;
    drain_config_deprecations(printer, &mut old_cfg);
    let old_source = old_cfg.spec.sources.iter().find(|s| s.name == old_name);
    let old_profile = old_source.and_then(|s| s.subscription.profile.clone());
    let old_priority = old_source.map(|s| s.subscription.priority).unwrap_or(500);

    // Remove old source (keeping resources)
    cmd_source_remove(cli, printer, old_name, true, false, false)?;

    // Add new source with same name, carrying over profile and priority
    cmd_source_add(
        cli,
        printer,
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
            pin_version: None,
            yes: true,
        },
    )?;

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("replaced with {}", new_url))
            .with_data(serde_json::json!({
                "oldName": old_name,
                "newUrl": new_url,
            })),
    );
    Ok(())
}
