use super::*;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};

/// Build the `cfgd alias list` Doc from a populated entries slice. Pure helper
/// so callers can assemble entries from disk and tests can drive the renderer
/// directly without touching the filesystem.
pub fn build_alias_list_doc(entries: &[AliasListEntry]) -> Doc {
    let mut doc = Doc::new().heading("CLI Aliases");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No aliases configured");
        return doc.with_data(entries);
    }

    let mut t = Table::new(["Name", "Command"]);
    for e in entries {
        t = t.row([e.name.clone(), e.command.clone()]);
    }
    doc = doc.table(t);

    doc.with_data(entries)
}

/// Doc emitted when no config file is present yet. Mirrors `source list`'s
/// missing-config branch — keeps human output informative without forcing the
/// caller to bail.
pub fn build_alias_list_no_config_doc() -> Doc {
    let empty: Vec<AliasListEntry> = Vec::new();
    Doc::new()
        .heading("CLI Aliases")
        .status(Role::Info, "No config file found")
        .with_data(&empty)
}

pub fn cmd_alias_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    if !config_path.exists() {
        if printer.is_structured() {
            printer.emit(Doc::new().with_data(Vec::<AliasListEntry>::new()));
            return Ok(());
        }
        printer.emit(build_alias_list_no_config_doc());
        return Ok(());
    }

    let cfg = config::load_config(&config_path)?;

    let mut entries: Vec<AliasListEntry> = cfg
        .spec
        .aliases
        .iter()
        .map(|(name, command)| AliasListEntry {
            name: name.clone(),
            command: command.clone(),
        })
        .collect();
    // HashMap iteration order is non-deterministic; sort by name so human
    // output, structured payloads, and pipe consumers all see stable rows.
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    printer.emit(build_alias_list_doc(&entries));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::{OutputFormat, Printer};
    use serde_json::json;

    fn test_cli_for(config_path: std::path::PathBuf) -> Cli {
        Cli {
            config: config_path,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: None,
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            system: false,
            command: None,
        }
    }

    #[test]
    fn build_alias_list_doc_with_entries_renders_table_rows_and_payload() {
        let entries = vec![
            AliasListEntry {
                name: "greet".to_string(),
                command: "status".to_string(),
            },
            AliasListEntry {
                name: "ll".to_string(),
                command: "profile list".to_string(),
            },
        ];
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_alias_list_doc(&entries));
        drop(printer);

        let rendered = cap.human();
        assert!(
            rendered.contains("greet"),
            "rendered output missing 'greet': {rendered}"
        );
        assert!(
            rendered.contains("status"),
            "rendered output missing 'status': {rendered}"
        );
        assert!(
            rendered.contains("ll"),
            "rendered output missing 'll': {rendered}"
        );
        assert!(
            rendered.contains("profile list"),
            "rendered output missing 'profile list': {rendered}"
        );

        let json = cap.json().expect("alias list doc must carry data payload");
        let arr = json.as_array().expect("payload must be an array");
        assert_eq!(arr.len(), 2, "expected 2 entries: {json}");
        assert_eq!(arr[0]["name"], json!("greet"));
        assert_eq!(arr[0]["command"], json!("status"));
        assert_eq!(arr[1]["name"], json!("ll"));
        assert_eq!(arr[1]["command"], json!("profile list"));
    }

    #[test]
    fn build_alias_list_doc_empty_says_no_aliases() {
        let entries: Vec<AliasListEntry> = Vec::new();
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_alias_list_doc(&entries));
        drop(printer);

        let rendered = cap.human();
        assert!(
            rendered.contains("No aliases configured"),
            "expected empty-state message in: {rendered}"
        );

        let json = cap.json().expect("empty list doc must carry data payload");
        let arr = json.as_array().expect("payload must be an array");
        assert!(arr.is_empty(), "expected empty array: {json}");
    }

    #[test]
    fn alias_list_entry_serializes_to_camel_case() {
        let v = AliasListEntry {
            name: "greet".to_string(),
            command: "status".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json, json!({"name": "greet", "command": "status"}));
    }

    #[test]
    fn cmd_alias_list_missing_config_structured_emits_empty_array() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg_path = tmp.path().join("cfgd.yaml");
        let cli = test_cli_for(cfg_path);
        let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
        cmd_alias_list(&cli, &printer).expect("cmd_alias_list ok on missing config");
        drop(printer);

        let json = cap
            .json()
            .expect("structured output must carry data payload");
        let arr = json.as_array().expect("payload must be an array");
        assert!(arr.is_empty(), "expected empty array: {json}");
    }

    #[test]
    fn cmd_alias_list_sorts_entries_by_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg_path = tmp.path().join("cfgd.yaml");
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  aliases:\n    zeta: status\n    alpha: plan\n    mid: diff\n";
        std::fs::write(&cfg_path, yaml).expect("write cfg");
        let cli = test_cli_for(cfg_path);
        let (printer, cap) = Printer::for_test_doc();
        cmd_alias_list(&cli, &printer).expect("cmd_alias_list ok");
        drop(printer);

        let json = cap.json().expect("payload");
        let arr = json.as_array().expect("array");
        let names: Vec<&str> = arr.iter().map(|v| v["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            vec!["alpha", "mid", "zeta"],
            "entries must be sorted by name"
        );
    }
}
