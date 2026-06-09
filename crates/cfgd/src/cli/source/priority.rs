use super::*;
use cfgd_core::config::validate_source_priority;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_source_priority(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    value: Option<u32>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source = match cfg.spec.sources.iter().find(|s| s.name == name) {
        Some(s) => s,
        None => {
            // Carry the typed SourceError::NotFound so the exit-code downcast
            // resolves to ExitCode::NotFound (6), uniform with other misses.
            return Err(crate::cli::cli_error_ctx(
                cfgd_core::errors::CfgdError::Source(cfgd_core::errors::SourceError::NotFound {
                    name: name.to_string(),
                })
                .into(),
                name,
                "not_found",
                format!("source '{}' not found", name),
                serde_json::json!({}),
            ));
        }
    };

    match value {
        Some(new_priority) => {
            validate_source_priority(new_priority).map_err(|m| anyhow::anyhow!(m))?;
            let old_priority = source.subscription.priority;
            // Update priority in cfgd.yaml
            with_source_config(&config_path, name, |source_entry| {
                let subscription = source_entry.get_mut("subscription").ok_or_else(|| {
                    anyhow::anyhow!("source '{}' has no subscription block", name)
                })?;

                if let Some(mapping) = subscription.as_mapping_mut() {
                    mapping.insert(
                        serde_yaml::Value::String("priority".into()),
                        serde_yaml::Value::Number(serde_yaml::Number::from(new_priority)),
                    );
                }
                Ok(())
            })?;

            printer.emit(
                Doc::new()
                    .status(
                        Role::Ok,
                        format!(
                            "Source '{}' priority updated: {} -> {}",
                            name, old_priority, new_priority
                        ),
                    )
                    .with_data(serde_json::json!({
                        "name": name,
                        "priority": new_priority,
                        "previousPriority": old_priority,
                    })),
            );
        }
        None => {
            printer.emit(
                Doc::new()
                    .kv("Source", name)
                    .kv("Priority", source.subscription.priority.to_string())
                    .hint("Local config priority is 1000")
                    .with_data(serde_json::json!({
                        "name": name,
                        "priority": source.subscription.priority,
                    })),
            );
        }
    }

    Ok(())
}
