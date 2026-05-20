use super::*;
use cfgd_core::output::{Doc, Printer as PrinterV2, Role};

pub fn cmd_source_override(
    cli: &Cli,
    v2_printer: &PrinterV2,
    source_name: &str,
    action: SourceOverrideAction,
    path: &str,
    value: Option<&str>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    // Verify source exists in config
    if !cfg.spec.sources.iter().any(|s| s.name == source_name) {
        v2_printer.emit(cfgd_core::output::error_doc(
            source_name,
            "not_found",
            format!("Source '{}' not found", source_name),
            serde_json::Value::Null,
        ));
        anyhow::bail!("Source '{}' not found", source_name);
    }

    match action {
        SourceOverrideAction::Reject => {
            update_source_rejection(&config_path, source_name, path)?;
            v2_printer.emit(
                Doc::new()
                    .status(
                        Role::Ok,
                        format!("Rejected '{}' from '{}'", path, source_name),
                    )
                    .with_data(serde_json::json!({
                        "sourceName": source_name,
                        "path": path,
                        "action": "reject",
                    })),
            );
        }
        SourceOverrideAction::Set => {
            let val = match value {
                Some(v) => v,
                None => {
                    v2_printer.emit(cfgd_core::output::error_doc(
                        source_name,
                        "missing_value",
                        "'set' action requires a value",
                        serde_json::json!({ "path": path }),
                    ));
                    anyhow::bail!("'set' action requires a value");
                }
            };
            update_source_override(&config_path, source_name, path, val)?;
            v2_printer.emit(
                Doc::new()
                    .status(
                        Role::Ok,
                        format!("Override set: {} = {} for '{}'", path, val, source_name),
                    )
                    .with_data(serde_json::json!({
                        "sourceName": source_name,
                        "path": path,
                        "value": val,
                        "action": "set",
                    })),
            );
        }
    }

    Ok(())
}

fn update_source_rejection(
    config_path: &Path,
    source_name: &str,
    path: &str,
) -> anyhow::Result<()> {
    with_source_config(config_path, source_name, |source| {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let sub_map = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?;
        let reject = sub_map
            .entry(serde_yaml::Value::String("reject".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        // Replace null with empty mapping (serde serializes default Value::Null)
        if reject.is_null() {
            *reject = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }

        set_nested_yaml_value(reject, path, &serde_yaml::Value::Null)?;
        Ok(())
    })
}

fn update_source_override(
    config_path: &Path,
    source_name: &str,
    path: &str,
    value: &str,
) -> anyhow::Result<()> {
    with_source_config(config_path, source_name, |source| {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let sub_map = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?;
        let overrides = sub_map
            .entry(serde_yaml::Value::String("overrides".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        // Replace null with empty mapping (serde serializes default Value::Null)
        if overrides.is_null() {
            *overrides = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }

        set_nested_yaml_value(
            overrides,
            path,
            &serde_yaml::Value::String(value.to_string()),
        )?;
        Ok(())
    })
}
