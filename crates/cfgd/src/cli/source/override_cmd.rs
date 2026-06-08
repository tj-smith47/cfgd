use super::*;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_source_override(
    cli: &Cli,
    printer: &Printer,
    source_name: &str,
    action: SourceOverrideAction,
    path: &str,
    value: Option<&str>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    // Verify source exists in config
    if !cfg.spec.sources.iter().any(|s| s.name == source_name) {
        // Carry the typed SourceError::NotFound so the exit-code downcast resolves
        // to ExitCode::NotFound (6), uniform with every other named-resource miss.
        return Err(crate::cli::cli_error_ctx(
            cfgd_core::errors::CfgdError::Source(cfgd_core::errors::SourceError::NotFound {
                name: source_name.to_string(),
            })
            .into(),
            source_name,
            "not_found",
            format!("Source '{}' not found", source_name),
            serde_json::json!({}),
        ));
    }

    match action {
        SourceOverrideAction::Reject => {
            update_source_rejection(&config_path, source_name, path)?;
            printer.emit(
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
                    return Err(crate::cli::cli_error(
                        source_name,
                        "missing_value",
                        "'set' action requires a value",
                        serde_json::json!({ "path": path }),
                    ));
                }
            };
            update_source_override(&config_path, source_name, path, val)?;
            printer.emit(
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

/// Fold a `snake_case` token to `camelCase`. Used to map an override path's
/// leading ProfileSpec field name onto its serde wire name; a token with no
/// underscores is returned unchanged.
fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = false;
    for ch in s.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
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

        // The override map deserializes as a ProfileSpec, whose serde wire names
        // are camelCase (`rename_all = "camelCase"`). The dotted path's FIRST
        // segment is a ProfileSpec field, so a snake_case form must be folded to
        // camelCase (`env_scope` -> `envScope`); stored verbatim it would be
        // rejected cryptically by `deny_unknown_fields` at compose time. Deeper
        // segments are map keys (env var / alias / package names) and stay
        // verbatim — they are user data, not struct fields.
        let mut segments: Vec<String> = path.split('.').map(str::to_string).collect();
        if let Some(first) = segments.first_mut() {
            *first = snake_to_camel(first);
        }
        let normalized_path = segments.join(".");
        let first_segment = segments.first().map(String::as_str).unwrap_or("");

        // env/alias values are ALWAYS strings (ProfileSpec's EnvVar.value /
        // ShellAlias.command are `String`), so a literal like `true` or `8080`
        // must be stored verbatim — YAML-parsing it would yield a bool/number
        // that fails to deserialize at compose time. Every other field
        // (packages/system/modules) is typed, so its value IS parsed as YAML
        // (`[prettier]` → a sequence, not the literal string `"[prettier]"`),
        // falling back to a plain string for a non-YAML token.
        let parsed = if matches!(first_segment, "env" | "aliases") {
            serde_yaml::Value::String(value.to_string())
        } else {
            serde_yaml::from_str(value)
                .unwrap_or_else(|_| serde_yaml::Value::String(value.to_string()))
        };
        set_nested_yaml_value(overrides, &normalized_path, &parsed)?;
        Ok(())
    })
}
