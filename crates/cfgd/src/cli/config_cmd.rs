use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2};

// --- Config CRUD ---

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

pub fn build_config_show_doc(cfg: &CfgdConfig, config_path: &Path) -> Doc {
    let mut doc = Doc::new()
        .heading("Configuration")
        .kv("File", config_path.display().to_string())
        .kv(
            "Profile",
            cfg.spec.profile.as_deref().unwrap_or("(none)").to_string(),
        );

    doc = doc.section_if_nonempty("Origins", &cfg.spec.origin, |s, origins| {
        origins.iter().enumerate().fold(s, |s, (i, origin)| {
            let label = if i == 0 { "Primary" } else { "Secondary" };
            s.subsection(
                format!("{}: {:?} — {}", label, origin.origin_type, origin.url),
                |sub| sub.kv("Branch", &origin.branch),
            )
        })
    });

    doc = doc.section_if_nonempty("Sources", &cfg.spec.sources, |s, sources| {
        sources
            .iter()
            .fold(s, |s, src| s.kv(&src.name, &src.origin.url))
    });

    if let Some(ref mods) = cfg.spec.modules {
        doc = doc.section_if_nonempty("Module Registries", &mods.registries, |s, regs| {
            regs.iter().fold(s, |s, ms| s.kv(&ms.name, &ms.url))
        });

        if let Some(ref sec) = mods.security {
            doc = doc.section("Module Security", |s| {
                s.kv("Require signatures", yes_no(sec.require_signatures))
            });
        }
    }

    if let Some(ref daemon) = cfg.spec.daemon {
        doc = doc.section("Daemon", |s| {
            let mut s = s.kv("Enabled", yes_no(daemon.enabled));
            if let Some(ref reconcile) = daemon.reconcile {
                s = s.subsection("Reconcile", |sub| {
                    sub.kv("Interval", &reconcile.interval)
                        .kv("On change", yes_no(reconcile.on_change))
                        .kv("Auto apply", yes_no(reconcile.auto_apply))
                });
            }
            if let Some(ref sync) = daemon.sync {
                s = s.subsection("Sync", |sub| sub.kv("Interval", &sync.interval));
            }
            s
        });
    }

    if let Some(ref secrets) = cfg.spec.secrets {
        doc = doc.section("Secrets", |s| s.kv("Backend", &secrets.backend));
    }

    if let Some(ref theme) = cfg.spec.theme {
        doc = doc.section("Theme", |s| s.kv("Theme", &theme.name));
    }

    doc.with_data(cfg)
}

pub(super) fn cmd_config_show(cli: &Cli, printer: &PrinterV2) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let cfg = config::load_config(config_path)?;
    printer.emit(build_config_show_doc(&cfg, config_path));
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

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::OutputFormat;

    fn test_cli_for(config_path: std::path::PathBuf) -> Cli {
        Cli {
            config: config_path,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: OutputFormatArg(OutputFormat::Table),
            jsonpath: None,
            state_dir: None,
            command: None,
        }
    }

    /// Minimal valid `Config` kind YAML that load_config will accept.
    const SAMPLE_CONFIG: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: work
  theme:
    name: monokai
"#;

    fn write_sample_config(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("cfgd.yaml");
        std::fs::write(&path, SAMPLE_CONFIG).unwrap();
        path
    }

    // --- parse_yaml_value ---

    #[test]
    fn parse_yaml_value_dispatches_each_type() {
        assert_eq!(parse_yaml_value("true"), serde_yaml::Value::Bool(true));
        assert_eq!(parse_yaml_value("false"), serde_yaml::Value::Bool(false));
        assert_eq!(parse_yaml_value("null"), serde_yaml::Value::Null);
        assert_eq!(parse_yaml_value("~"), serde_yaml::Value::Null);
        assert_eq!(
            parse_yaml_value("42"),
            serde_yaml::Value::Number(42i64.into())
        );
        assert!(matches!(
            parse_yaml_value("3.14"),
            serde_yaml::Value::Number(_)
        ));
        assert_eq!(
            parse_yaml_value("hello"),
            serde_yaml::Value::String("hello".into())
        );
        // Empty string falls through to String, not anything else.
        assert_eq!(parse_yaml_value(""), serde_yaml::Value::String("".into()));
    }

    // --- walk_yaml_path ---

    #[test]
    fn walk_yaml_path_dot_returns_root() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let leaf = walk_yaml_path(&yaml, ".").unwrap();
        // Root is the whole mapping
        assert!(leaf.is_mapping());
    }

    #[test]
    fn walk_yaml_path_nested_segments_resolve_leaf() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("theme:\n  name: monokai\n").unwrap();
        let leaf = walk_yaml_path(&yaml, "theme.name").unwrap();
        assert_eq!(leaf, &serde_yaml::Value::String("monokai".into()));
    }

    #[test]
    fn walk_yaml_path_missing_key_errs_with_partial_path() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let err = walk_yaml_path(&yaml, "a.b.c").unwrap_err();
        let msg = err.to_string();
        // 'a' exists but is not a mapping → error mentions the partial prefix
        assert!(
            msg.contains("not a mapping") && msg.contains("a"),
            "expected non-mapping error mentioning prefix 'a', got: {msg}"
        );
    }

    #[test]
    fn walk_yaml_path_empty_segment_errs() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a:\n  b: 1\n").unwrap();
        let err = walk_yaml_path(&yaml, "a..b").unwrap_err();
        assert!(
            err.to_string().contains("empty segment"),
            "expected 'empty segment' error, got: {err}"
        );
    }

    #[test]
    fn walk_yaml_path_unknown_top_level_key_errs() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let err = walk_yaml_path(&yaml, "nope").unwrap_err();
        assert!(
            err.to_string().contains("'nope' not found"),
            "expected key-not-found error, got: {err}"
        );
    }

    // --- walk_yaml_path_mut ---

    #[test]
    fn walk_yaml_path_mut_creates_intermediate_maps() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("existing: 1\n").unwrap();
        let (parent, leaf) = walk_yaml_path_mut(&mut yaml, "a.b.c").unwrap();
        assert_eq!(leaf, "c");
        // Insert and verify the chain was materialized
        parent.insert(
            serde_yaml::Value::String("c".into()),
            serde_yaml::Value::Bool(true),
        );
        let leaf_val = walk_yaml_path(&yaml, "a.b.c").unwrap();
        assert_eq!(leaf_val, &serde_yaml::Value::Bool(true));
    }

    #[test]
    fn walk_yaml_path_mut_empty_segment_errs() {
        let mut yaml: serde_yaml::Value = serde_yaml::from_str("a: 1\n").unwrap();
        let err = walk_yaml_path_mut(&mut yaml, "a..b").unwrap_err();
        assert!(
            err.to_string().contains("empty segment"),
            "expected 'empty segment' error, got: {err}"
        );
    }

    // --- cmd_config_show ---

    #[test]
    fn cmd_config_show_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(dir.path().join("does-not-exist.yaml"));
        let printer = PrinterV2::new(cfgd_core::output_v2::Verbosity::Quiet);

        let err = cmd_config_show(&cli, &printer).unwrap_err();
        assert_eq!(err.to_string(), MSG_NO_CONFIG);
    }

    #[test]
    fn cmd_config_show_table_renders_header_and_profile() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, cap) = PrinterV2::for_test_doc();

        cmd_config_show(&cli, &printer).unwrap();
        printer.flush();
        drop(printer);

        let output = cap.human();
        assert!(
            output.contains("Configuration"),
            "should print 'Configuration' header, got: {output}"
        );
        assert!(
            output.contains("work"),
            "should print profile value 'work', got: {output}"
        );
    }

    #[test]
    fn cmd_config_show_json_emits_parseable_object() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, buf) =
            PrinterV2::for_test_with_format(cfgd_core::output_v2::OutputFormat::Json);

        cmd_config_show(&cli, &printer).unwrap();

        let captured = buf.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(parsed["apiVersion"], "cfgd.io/v1alpha1");
        assert_eq!(parsed["kind"], "Config");
        assert_eq!(parsed["spec"]["profile"], "work");
    }

    // --- cmd_config_get ---

    #[test]
    fn cmd_config_get_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(dir.path().join("does-not-exist.yaml"));
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_config_get(&cli, &printer, "profile").unwrap_err();
        assert_eq!(err.to_string(), MSG_NO_CONFIG);
    }

    #[test]
    fn cmd_config_get_scalar_prints_value_only() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, buf) = Printer::for_test();

        cmd_config_get(&cli, &printer, "profile").unwrap();

        let captured = buf.lock().unwrap().clone();
        assert_eq!(
            captured.trim(),
            "work",
            "scalar get should print bare value, got: {captured:?}"
        );
    }

    #[test]
    fn cmd_config_get_nested_key_prints_value() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, buf) = Printer::for_test();

        cmd_config_get(&cli, &printer, "theme.name").unwrap();

        let captured = buf.lock().unwrap().clone();
        assert_eq!(captured.trim(), "monokai");
    }

    #[test]
    fn cmd_config_get_unknown_key_errs() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_config_get(&cli, &printer, "missing").unwrap_err();
        assert!(
            err.to_string().contains("'missing' not found"),
            "expected key-not-found error, got: {err}"
        );
    }

    #[test]
    fn cmd_config_get_no_spec_section_errs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nospec.yaml");
        std::fs::write(&path, "apiVersion: cfgd.io/v1alpha1\nkind: Config\n").unwrap();
        let cli = test_cli_for(path);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_config_get(&cli, &printer, "profile").unwrap_err();
        assert!(
            err.to_string().contains("no 'spec' section"),
            "expected 'no spec section' error, got: {err}"
        );
    }

    #[test]
    fn cmd_config_get_json_emits_parseable_value() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(write_sample_config(dir.path()));
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);

        cmd_config_get(&cli, &printer, "theme").unwrap();

        let captured = buf.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(parsed["name"], "monokai");
    }

    // --- cmd_config_set ---

    #[test]
    fn cmd_config_set_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(dir.path().join("does-not-exist.yaml"));
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_config_set(&cli, &printer, "profile", "dev").unwrap_err();
        assert_eq!(err.to_string(), MSG_NO_CONFIG);
    }

    #[test]
    fn cmd_config_set_overwrites_scalar_and_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        cmd_config_set(&cli, &printer, "profile", "dev").unwrap();

        // Round-trip via the same parser to confirm the write survived validation
        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded["spec"]["profile"],
            serde_yaml::Value::String("dev".into())
        );
    }

    #[test]
    fn cmd_config_set_special_chars_round_trip_as_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let weird = "value with: colon, # hash, and 'quote'";

        cmd_config_set(&cli, &printer, "profile", weird).unwrap();

        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded["spec"]["profile"],
            serde_yaml::Value::String(weird.into())
        );
    }

    #[test]
    fn cmd_config_set_empty_value_writes_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        cmd_config_set(&cli, &printer, "profile", "").unwrap();

        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            reloaded["spec"]["profile"],
            serde_yaml::Value::String("".into())
        );
    }

    #[test]
    fn cmd_config_set_invalid_key_path_errs() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_config_set(&cli, &printer, "a..b", "x").unwrap_err();
        assert!(
            err.to_string().contains("empty segment"),
            "expected 'empty segment' error, got: {err}"
        );
    }

    // --- cmd_config_unset ---

    #[test]
    fn cmd_config_unset_missing_file_bails_with_no_config_msg() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(dir.path().join("does-not-exist.yaml"));
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_config_unset(&cli, &printer, "profile").unwrap_err();
        assert_eq!(err.to_string(), MSG_NO_CONFIG);
    }

    #[test]
    fn cmd_config_unset_removes_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path.clone());
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        cmd_config_unset(&cli, &printer, "profile").unwrap();

        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            reloaded["spec"].get("profile").is_none(),
            "profile key should be removed, got: {reloaded:?}"
        );
    }

    #[test]
    fn cmd_config_unset_unknown_key_errs() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample_config(dir.path());
        let cli = test_cli_for(path);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let err = cmd_config_unset(&cli, &printer, "missingKey").unwrap_err();
        assert!(
            err.to_string().contains("'missingKey' not found"),
            "expected key-not-found error, got: {err}"
        );
    }
}
