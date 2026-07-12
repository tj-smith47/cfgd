use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_source_create(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    description: Option<&str>,
    version: Option<&str>,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let source_path = config_dir.join("cfgd-source.yaml");
    if source_path.exists() {
        return Err(crate::cli::cli_error(
            "cfgd-source.yaml",
            "already_exists",
            format!(
                "cfgd-source.yaml already exists at {} — use 'cfgd source edit' to modify it",
                source_path.posix()
            ),
            serde_json::json!({ "path": cfgd_core::to_posix_string(&source_path) }),
        ));
    }

    // Interactive mode if no flags provided
    let is_interactive = name.is_none() && description.is_none() && version.is_none();

    // Determine name: flag > interactive prompt > directory name
    let source_name = match name {
        Some(n) => n.to_string(),
        None => {
            let dir_name = config_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("my-config");
            if is_interactive {
                printer.prompt_text("Source name", dir_name)?
            } else {
                dir_name.to_string()
            }
        }
    };

    let source_description = match description {
        Some(d) => d.to_string(),
        None => {
            if is_interactive {
                printer.prompt_text("Description", "Team configuration source")?
            } else {
                "Team configuration source".to_string()
            }
        }
    };

    let source_version = match version {
        Some(v) => v.to_string(),
        None => "0.1.0".to_string(),
    };

    let profile_names = scan_profile_names(&config_dir.join("profiles"), printer)?;
    let module_names = scan_module_names(&config_dir.join("modules"), printer)?;

    // Build profiles YAML block
    let profiles_yaml = if profile_names.is_empty() {
        "    profiles: []".to_string()
    } else {
        let mut lines = vec!["    profiles:".to_string()];
        for p in &profile_names {
            lines.push(format!("      - {}", p));
        }
        lines.join("\n")
    };

    // Build modules YAML block
    let modules_yaml = if module_names.is_empty() {
        "    modules: []".to_string()
    } else {
        let mut lines = vec!["    modules:".to_string()];
        for m in &module_names {
            lines.push(format!("      - {}", m));
        }
        lines.join("\n")
    };

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\n\
         kind: ConfigSource\n\
         metadata:\n\
         \x20 name: {}\n\
         \x20 version: \"{}\"\n\
         \x20 description: \"{}\"\n\
         spec:\n\
         \x20 provides:\n\
         {}\n\
         {}\n\
         \x20 policy:\n\
         \x20   required:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   recommended:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   optional:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   constraints:\n\
         \x20     noScripts: true\n\
         \x20     noSecretsRead: true\n",
        source_name, source_version, source_description, profiles_yaml, modules_yaml,
    );

    let yaml = cfgd_core::config::with_schema_modeline(
        cfgd_core::config::SchemaDocKind::ConfigSource,
        env!("CARGO_PKG_VERSION"),
        &yaml,
    );
    cfgd_core::atomic_write_str(&source_path, &yaml)?;

    let mut doc = Doc::new().status(
        Role::Ok,
        format!("Created cfgd-source.yaml at {}", source_path.posix()),
    );
    if !profile_names.is_empty() {
        doc = doc.status(
            Role::Info,
            format!(
                "Included {} profile(s): {}",
                profile_names.len(),
                profile_names.join(", ")
            ),
        );
    }
    if !module_names.is_empty() {
        doc = doc.status(
            Role::Info,
            format!(
                "Included {} module(s): {}",
                module_names.len(),
                module_names.join(", ")
            ),
        );
    }
    doc = doc
        .hint("Edit the file to configure policy tiers and platform-profiles")
        .with_data(serde_json::json!({
            "name": source_name,
            "path": source_path.display().to_string(),
            "version": source_version,
            "profiles": profile_names,
            "modules": module_names,
        }));
    printer.emit(doc);

    Ok(())
}
