use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_module_export(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    format: &super::ExportFormat,
    output_dir: Option<&str>,
) -> anyhow::Result<()> {
    match format {
        super::ExportFormat::Devcontainer => export_devcontainer(cli, printer, name, output_dir),
    }
}

/// The package family a DevContainer feature installs through. A feature runs
/// inside a Debian-based image, and this one name resolves BOTH the per-package
/// alias lookup and the install script, so the export cannot look packages up
/// under one manager and install them with another.
const DEVCONTAINER_MANAGER: &str = "apt";

pub(super) fn export_devcontainer(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    output_dir: Option<&str>,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let cache_base = module_cache_dir(cli)?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, &[], printer)?;

    let module = match all_modules.get(name) {
        Some(m) => m,
        None => {
            // Carry the typed ModuleError::NotFound so the exit-code downcast
            // resolves to ExitCode::NotFound (6), uniform with other misses.
            return Err(crate::cli::cli_error_ctx(
                cfgd_core::errors::CfgdError::Module(cfgd_core::errors::ModuleError::NotFound {
                    name: name.to_string(),
                })
                .into(),
                name,
                "not_found",
                format!("Module '{}' not found", name),
                serde_json::json!({}),
            ));
        }
    };

    let out = PathBuf::from(output_dir.unwrap_or("."));
    let feature_dir = out.join(name);
    std::fs::create_dir_all(&feature_dir)?;

    // Build install.sh
    let mut install_lines = Vec::new();
    install_lines.push("#!/bin/sh".to_string());
    install_lines.push("set -e".to_string());
    install_lines.push(String::new());
    install_lines.push(format!("echo \"Installing cfgd module: {}\"", name));
    install_lines.push(String::new());

    // Package install commands. A DevContainer feature runs in a Debian-based
    // image, so `apt` is the family both the alias lookup and the install
    // script resolve against — ONE binding, so the two cannot name different
    // managers.
    let apt_packages: Vec<String> = module
        .spec
        .packages
        .iter()
        .filter_map(|p| {
            // Use apt alias if available, otherwise use canonical name
            if let Some(apt_name) = p.aliases.get(DEVCONTAINER_MANAGER) {
                Some(apt_name.clone())
            } else if p.platforms.is_empty() || p.platforms.iter().any(|pl| pl == "linux") {
                Some(p.name.clone())
            } else {
                None
            }
        })
        .collect();

    if !apt_packages.is_empty() {
        // Composed from the family's own `install_cmd` / `update_cmd`, never
        // written out here: an export that resolves a different package set
        // than `cfgd apply` does from the same `module.yaml` is exactly the
        // divergence this command exists to prevent.
        let script = crate::packages::manager_install_script(DEVCONTAINER_MANAGER, &apt_packages)
            .ok_or_else(|| {
            anyhow::anyhow!("no install command declared for manager '{DEVCONTAINER_MANAGER}'")
        })?;
        if let Some(update) = script.update {
            install_lines.push(update);
        }
        install_lines.push(script.install);
        install_lines.push("rm -rf /var/lib/apt/lists/*".to_string());
        install_lines.push(String::new());
    }

    // Script-based packages
    for pkg in &module.spec.packages {
        if let Some(ref script) = pkg.script {
            install_lines.push(format!("# Install {} via script", pkg.name));
            install_lines.push(script.clone());
            install_lines.push(String::new());
        }
    }

    // Environment variables
    for ev in &module.spec.env {
        install_lines.push(format!(
            "echo 'export {}=\"{}\"' >> /etc/profile.d/cfgd-{}.sh",
            ev.name,
            cfgd_core::shell_escape_value(&ev.value),
            name
        ));
    }

    // Post-apply scripts
    if let Some(ref scripts) = module.spec.scripts {
        for script in &scripts.post_apply {
            install_lines.push(String::new());
            // `run_str()` returns the raw script body: for a multi-line
            // inline script, interpolating it straight after `# ` would only
            // comment out the first line, leaving the rest as live shell that
            // runs once here and again via the push below.
            install_lines.push(format!(
                "# Post-apply: {}",
                cfgd_core::output::condense_script_label(script.run_str())
            ));
            install_lines.push(script.run_str().to_string());
        }
    }

    let install_path = feature_dir.join("install.sh");
    let mut install_content = install_lines.join("\n");
    install_content.push('\n');
    cfgd_core::atomic_write_str(&install_path, &install_content)?;
    cfgd_core::set_file_permissions(&install_path, 0o755)?;

    // Build devcontainer-feature.json
    let mut options = serde_json::Map::new();
    for ev in &module.spec.env {
        options.insert(
            ev.name.clone(),
            serde_json::json!({
                "type": "string",
                "default": ev.value,
                "description": format!("Environment variable: {}", ev.name)
            }),
        );
    }

    // Try to get description from module.yaml metadata
    let description = load_module_document(&config_dir, name)
        .ok()
        .and_then(|(doc, _)| doc.metadata.description)
        .unwrap_or_else(|| {
            format!(
                "cfgd module: {}",
                module
                    .spec
                    .packages
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });

    let feature = serde_json::json!({
        "id": name,
        "version": "1.0.0",
        "name": name,
        "description": description,
        "options": options,
        "installsAfter": module.spec.depends.iter()
            .map(|d| format!("ghcr.io/cfgd-org/features/{}", d))
            .collect::<Vec<_>>(),
    });

    let feature_json = serde_json::to_string_pretty(&feature)?;
    let feature_path = feature_dir.join("devcontainer-feature.json");
    cfgd_core::atomic_write_str(&feature_path, &feature_json)?;

    let install_path_str = install_path.display_posix();
    let feature_path_str = feature_path.display_posix();
    let out_sec = printer.section(format!(
        "Exported module '{}' as DevContainer Feature to {}",
        name,
        feature_dir.posix()
    ));
    out_sec.bullet(install_path_str.clone());
    out_sec.bullet(feature_path_str.clone());
    drop(out_sec);

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Exported module '{}' as DevContainer Feature", name),
            )
            .with_data(serde_json::json!({
                "name": name,
                "format": "devcontainer",
                "outputDir": feature_dir.display().to_string(),
                "installScript": install_path_str,
                "featureJson": feature_path_str,
            })),
    );

    Ok(())
}
