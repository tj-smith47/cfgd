use super::*;

pub(crate) fn cmd_source_priority(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    value: Option<u32>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source = cfg
        .spec
        .sources
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("source '{}' not found", name))?;

    match value {
        Some(new_priority) => {
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

            printer.success(&format!(
                "Source '{}' priority updated: {} -> {}",
                name, source.subscription.priority, new_priority
            ));
        }
        None => {
            printer.key_value("Source", name);
            printer.key_value("Priority", &source.subscription.priority.to_string());
            printer.info("Local config priority is 1000");
        }
    }

    Ok(())
}
