use super::*;

// --- Config CRUD ---

pub(super) fn cmd_config_show(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let cfg = config::load_config(config_path)?;

    if printer.write_structured(&cfg) {
        return Ok(());
    }

    printer.header("Configuration");
    printer.key_value("File", &config_path.display().to_string());
    printer.key_value("Profile", cfg.spec.profile.as_deref().unwrap_or("(none)"));

    // Origins
    if !cfg.spec.origin.is_empty() {
        printer.newline();
        printer.subheader("Origins");
        for (i, origin) in cfg.spec.origin.iter().enumerate() {
            let label = if i == 0 { "Primary" } else { "Secondary" };
            printer.key_value(label, &format!("{:?} — {}", origin.origin_type, origin.url));
            printer.key_value("  Branch", &origin.branch);
        }
    }

    // Sources
    if !cfg.spec.sources.is_empty() {
        printer.newline();
        printer.subheader("Sources");
        for src in &cfg.spec.sources {
            printer.key_value(&src.name, &src.origin.url);
        }
    }

    // Module registries
    if let Some(ref mods) = cfg.spec.modules {
        if !mods.registries.is_empty() {
            printer.newline();
            printer.subheader("Module Registries");
            for ms in &mods.registries {
                printer.key_value(&ms.name, &ms.url);
            }
        }

        // Module security
        if let Some(ref sec) = mods.security {
            printer.newline();
            printer.subheader("Module Security");
            printer.key_value(
                "Require signatures",
                if sec.require_signatures { "yes" } else { "no" },
            );
        }
    }

    // Daemon
    if let Some(ref daemon) = cfg.spec.daemon {
        printer.newline();
        printer.subheader("Daemon");
        printer.key_value("Enabled", if daemon.enabled { "yes" } else { "no" });
        if let Some(ref reconcile) = daemon.reconcile {
            printer.key_value("  Reconcile interval", &reconcile.interval);
            printer.key_value(
                "  On change",
                if reconcile.on_change { "yes" } else { "no" },
            );
            printer.key_value(
                "  Auto apply",
                if reconcile.auto_apply { "yes" } else { "no" },
            );
        }
        if let Some(ref sync) = daemon.sync {
            printer.key_value("  Sync interval", &sync.interval);
        }
    }

    // Secrets
    if let Some(ref secrets) = cfg.spec.secrets {
        printer.newline();
        printer.subheader("Secrets");
        printer.key_value("Backend", &secrets.backend);
    }

    // Theme
    if let Some(ref theme) = cfg.spec.theme {
        printer.newline();
        printer.subheader("Theme");
        printer.key_value("Theme", &theme.name);
    }

    Ok(())
}

pub(super) fn cmd_config_edit(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    open_in_editor(config_path, printer)?;

    // Validate after editing — loop until valid or user cancels
    loop {
        match config::load_config(config_path) {
            Ok(_) => {
                printer.success("Configuration is valid");
                break;
            }
            Err(e) => {
                printer.error(&format!("Invalid configuration: {}", e));
                if !printer.prompt_confirm("Re-open in editor to fix?")? {
                    printer.warning("Saved with validation errors");
                    break;
                }
                open_in_editor(config_path, printer)?;
            }
        }
    }

    Ok(())
}

// --- Config get/set/unset ---

/// Walk a dotted key path through a YAML value, returning the leaf.
/// Use "." to return the root value itself.
pub(super) fn walk_yaml_path<'a>(
    value: &'a serde_yaml::Value,
    path: &str,
) -> anyhow::Result<&'a serde_yaml::Value> {
    if path == "." {
        return Ok(value);
    }
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        anyhow::bail!("invalid key path '{}': contains empty segment", path);
    }
    let mut current = value;

    for (i, segment) in segments.iter().enumerate() {
        match current {
            serde_yaml::Value::Mapping(map) => {
                let key = serde_yaml::Value::String((*segment).to_string());
                current = map.get(&key).ok_or_else(|| {
                    let partial = segments[..=i].join(".");
                    anyhow::anyhow!("key '{}' not found in config", partial)
                })?;
            }
            _ => {
                let partial = segments[..i].join(".");
                anyhow::bail!("'{}' is not a mapping", partial);
            }
        }
    }

    Ok(current)
}

/// Walk a dotted key path, creating intermediate mappings as needed.
/// Returns a mutable reference to the *parent* mapping and the leaf key name.
pub(super) fn walk_yaml_path_mut<'a>(
    value: &'a mut serde_yaml::Value,
    path: &str,
) -> anyhow::Result<(&'a mut serde_yaml::Mapping, String)> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.is_empty() || segments.iter().any(|s| s.is_empty()) {
        anyhow::bail!("invalid key path '{}': contains empty segment", path);
    }

    let mut current = value;
    // Walk to the parent of the final segment, creating intermediate maps
    for segment in &segments[..segments.len() - 1] {
        let key = serde_yaml::Value::String((*segment).to_string());
        if !current.as_mapping().is_some_and(|m| m.contains_key(&key)) {
            // Create intermediate mapping
            let map = current
                .as_mapping_mut()
                .ok_or_else(|| anyhow::anyhow!("cannot traverse into non-mapping"))?;
            map.insert(
                key.clone(),
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            );
        }
        current = current
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("cannot traverse into non-mapping"))?
            .get_mut(&key)
            .ok_or_else(|| anyhow::anyhow!("failed to create intermediate mapping"))?;
    }

    let parent = current
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("parent is not a mapping"))?;
    let leaf = segments
        .last()
        .ok_or_else(|| anyhow::anyhow!("empty key path"))?
        .to_string();
    Ok((parent, leaf))
}

/// Parse a string value into the most appropriate YAML type.
pub(super) fn parse_yaml_value(s: &str) -> serde_yaml::Value {
    match s {
        "true" => serde_yaml::Value::Bool(true),
        "false" => serde_yaml::Value::Bool(false),
        "null" | "~" => serde_yaml::Value::Null,
        _ => {
            // Try integer, then float, then string
            if let Ok(n) = s.parse::<i64>() {
                serde_yaml::Value::Number(n.into())
            } else if let Ok(f) = s.parse::<f64>() {
                serde_yaml::Value::Number(serde_yaml::Number::from(f))
            } else {
                serde_yaml::Value::String(s.to_string())
            }
        }
    }
}

pub(super) fn cmd_config_get(cli: &Cli, printer: &Printer, key: &str) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let contents = std::fs::read_to_string(config_path)?;
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    let spec = raw
        .get("spec")
        .ok_or_else(|| anyhow::anyhow!("config has no 'spec' section"))?;

    let value = walk_yaml_path(spec, key)?;

    if printer.is_structured() {
        // Convert serde_yaml::Value to serde_json::Value for structured output
        let json_value: serde_json::Value =
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        printer.write_structured(&json_value);
        return Ok(());
    }

    match value {
        serde_yaml::Value::Null => {} // key exists but null — print nothing
        serde_yaml::Value::String(s) => printer.stdout_line(s),
        serde_yaml::Value::Bool(b) => printer.stdout_line(&b.to_string()),
        serde_yaml::Value::Number(n) => printer.stdout_line(&n.to_string()),
        other => {
            let yaml = serde_yaml::to_string(other)?;
            let trimmed = yaml.strip_prefix("---\n").unwrap_or(&yaml);
            printer.stdout_line(trimmed.trim_end());
        }
    }

    Ok(())
}

pub(super) fn cmd_config_set(
    cli: &Cli,
    printer: &Printer,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    mutate_config_yaml(config_path, true, |raw| {
        let spec = raw
            .get_mut("spec")
            .ok_or_else(|| anyhow::anyhow!("config has no 'spec' section"))?;
        let (parent, leaf_key) = walk_yaml_path_mut(spec, key)?;
        let yaml_key = serde_yaml::Value::String(leaf_key);
        parent.insert(yaml_key, parse_yaml_value(value));
        Ok(())
    })?;
    printer.success(&format!("Set {} = {}", key, value));
    Ok(())
}

pub(super) fn cmd_config_unset(cli: &Cli, printer: &Printer, key: &str) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    mutate_config_yaml(config_path, true, |raw| {
        let spec = raw
            .get_mut("spec")
            .ok_or_else(|| anyhow::anyhow!("config has no 'spec' section"))?;
        let (parent, leaf_key) = walk_yaml_path_mut(spec, key)?;
        let yaml_key = serde_yaml::Value::String(leaf_key.clone());
        if parent.remove(&yaml_key).is_none() {
            anyhow::bail!("key '{}' not found in config", key);
        }
        Ok(())
    })?;
    printer.success(&format!("Unset {}", key));
    Ok(())
}
