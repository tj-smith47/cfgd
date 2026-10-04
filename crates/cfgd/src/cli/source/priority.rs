use super::*;
use cfgd_core::output::{Doc, OwnerLabel, Role};

pub fn cmd_source_priority(
    run: &RunContext<'_>,
    name: &str,
    value: Option<u32>,
) -> anyhow::Result<()> {
    let cli = run.cli();
    let printer = run.printer();
    let config_path = cli.config.clone();
    let cfg = run.config()?;

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
            checked_priority(new_priority, "[VALUE]")?;
            let old_priority = source.subscription.priority;
            // Update priority in cfgd.yaml
            with_source_config(&config_path, name, |source_entry| {
                subscription_mapping_mut(source_entry, &config_path, name)?.insert(
                    serde_yaml::Value::String("priority".into()),
                    serde_yaml::Value::Number(serde_yaml::Number::from(new_priority)),
                );
                Ok(())
            })?;

            printer.emit(
                Doc::new()
                    .status_owner_with(Role::Ok, OwnerLabel::new("source", name), |f| {
                        f.detail(format!(
                            "priority updated: {old_priority} {} {new_priority}",
                            printer.arrow()
                        ))
                    })
                    .hint(super::success_next_step(
                        super::Mutation::SourceReprioritized,
                    ))
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
                    .hint(format!(
                        "Change it with `cfgd source priority {name} <priority>` (local config is 1000)"
                    ))
                    .with_data(serde_json::json!({
                        "name": name,
                        "priority": source.subscription.priority,
                    })),
            );
        }
    }

    Ok(())
}
