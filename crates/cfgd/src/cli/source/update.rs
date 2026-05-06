use super::*;

pub(crate) fn cmd_source_update(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Update Sources");

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        printer.info("No sources configured");
        return Ok(());
    }

    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    let state = open_state_store(cli.state_dir.as_deref())?;

    let sources_to_update: Vec<&config::SourceSpec> = if let Some(name) = name {
        cfg.spec.sources.iter().filter(|s| s.name == name).collect()
    } else {
        cfg.spec.sources.iter().collect()
    };

    if sources_to_update.is_empty()
        && let Some(name) = name
    {
        anyhow::bail!("Source '{}' not found", name);
    }

    for source in &sources_to_update {
        // Capture old manifest before fetching (for permission change detection)
        let source_dir = cache_dir.join(&source.name);
        let old_manifest = if source_dir.exists() {
            mgr.parse_manifest(&source.name, &source_dir).ok()
        } else {
            None
        };

        match mgr.load_source(source, printer) {
            Ok(()) => {
                if let Some(cached) = mgr.get(&source.name) {
                    // Detect permission-expanding changes between old and new manifests
                    if let Some(ref old) = old_manifest {
                        let old_input = build_permission_input(&source.name, &old.spec.policy);
                        let new_input =
                            build_permission_input(&source.name, &cached.manifest.spec.policy);
                        let perm_changes =
                            composition::detect_permission_changes(&[old_input], &[new_input]);
                        if !perm_changes.is_empty() {
                            printer.newline();
                            printer.warning(&format!(
                                "Source '{}' update changes permissions:",
                                source.name
                            ));
                            for change in &perm_changes {
                                printer.warning(&format!("  - {}", change.description));
                            }
                            match printer.prompt_confirm("Accept permission changes?") {
                                Ok(true) => {}
                                Ok(false) => {
                                    printer.info(&format!(
                                        "Skipped source '{}' (permission changes rejected)",
                                        source.name
                                    ));
                                    continue;
                                }
                                Err(_) => {
                                    printer.info(&format!(
                                        "Skipped source '{}' (prompt cancelled)",
                                        source.name
                                    ));
                                    continue;
                                }
                            }
                        }
                    }

                    state.upsert_config_source(
                        &source.name,
                        &source.origin.url,
                        &source.origin.branch,
                        cached.last_commit.as_deref(),
                        cached.manifest.metadata.version.as_deref(),
                        source.sync.pin_version.as_deref(),
                    )?;
                    printer.success(&format!("Updated source '{}'", source.name));
                }
            }
            Err(e) => {
                printer.error(&format!("Failed to update source '{}': {}", source.name, e));
                state.update_config_source_status(&source.name, "error")?;
            }
        }
    }

    Ok(())
}
