use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

pub fn cmd_source_update(
    cli: &Cli,
    printer: &Printer,
    v2_printer: &PrinterV2,
    name: Option<&str>,
) -> anyhow::Result<()> {
    v2_printer.heading("Update Sources");

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        v2_printer.emit(
            Doc::new()
                .status(Role::Info, "No sources configured")
                .with_data(serde_json::json!({ "sources": [] })),
        );
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
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            name,
            "not_found",
            format!("Source '{}' not found", name),
            serde_json::Value::Null,
        ));
        anyhow::bail!("Source '{}' not found", name);
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct UpdateEntry {
        name: String,
        status: String,
        commit: Option<String>,
        perm_changes: usize,
    }
    let mut entries: Vec<UpdateEntry> = Vec::new();

    for source in &sources_to_update {
        // Capture old manifest before fetching (for permission change detection)
        let source_dir = cache_dir.join(&source.name);
        let old_manifest = if source_dir.exists() {
            mgr.parse_manifest(&source.name, &source_dir).ok()
        } else {
            None
        };

        // Hybrid lib-call: cfgd_core::sources keeps the v1 Printer until F4b.
        match mgr.load_source(source, printer) {
            Ok(()) => {
                if let Some(cached) = mgr.get(&source.name) {
                    // Detect permission-expanding changes between old and new manifests
                    let perm_changes = if let Some(ref old) = old_manifest {
                        let old_input = build_permission_input(&source.name, &old.spec.policy);
                        let new_input =
                            build_permission_input(&source.name, &cached.manifest.spec.policy);
                        composition::detect_permission_changes(&[old_input], &[new_input])
                    } else {
                        Vec::new()
                    };

                    // Per-source SectionGuard binds across both the prompt
                    // and the success emit so the canonical
                    // Accept-confirm-then-success line nests under the same
                    // section header as the prompt context bullets (F3
                    // README Accept-confirm-then-success pattern). The
                    // section header phrasing pivots on whether permission
                    // changes were detected.
                    let source_sec = if perm_changes.is_empty() {
                        v2_printer.section(format!("Source '{}'", source.name))
                    } else {
                        v2_printer.section(format!(
                            "Source '{}' update changes permissions",
                            source.name
                        ))
                    };
                    for change in &perm_changes {
                        source_sec.status_simple(Role::Warn, change.description.clone());
                    }

                    let proceed = if !perm_changes.is_empty() {
                        match v2_printer.prompt_confirm("Accept permission changes?") {
                            Ok(true) => true,
                            Ok(false) => {
                                source_sec.status_simple(
                                    Role::Info,
                                    format!(
                                        "Skipped source '{}' (permission changes rejected)",
                                        source.name
                                    ),
                                );
                                entries.push(UpdateEntry {
                                    name: source.name.clone(),
                                    status: "skipped".into(),
                                    commit: cached.last_commit.clone(),
                                    perm_changes: perm_changes.len(),
                                });
                                false
                            }
                            Err(_) => {
                                source_sec.status_simple(
                                    Role::Info,
                                    format!("Skipped source '{}' (prompt cancelled)", source.name),
                                );
                                entries.push(UpdateEntry {
                                    name: source.name.clone(),
                                    status: "cancelled".into(),
                                    commit: cached.last_commit.clone(),
                                    perm_changes: perm_changes.len(),
                                });
                                false
                            }
                        }
                    } else {
                        true
                    };

                    if proceed {
                        state.upsert_config_source(
                            &source.name,
                            &source.origin.url,
                            &source.origin.branch,
                            cached.last_commit.as_deref(),
                            cached.manifest.metadata.version.as_deref(),
                            source.sync.pin_version.as_deref(),
                        )?;
                        source_sec
                            .status_simple(Role::Ok, format!("Updated source '{}'", source.name));
                        entries.push(UpdateEntry {
                            name: source.name.clone(),
                            status: "updated".into(),
                            commit: cached.last_commit.clone(),
                            perm_changes: perm_changes.len(),
                        });
                    }
                }
            }
            Err(e) => {
                v2_printer.status_simple(
                    Role::Fail,
                    format!("Failed to update source '{}': {}", source.name, e),
                );
                state.update_config_source_status(&source.name, "error")?;
                entries.push(UpdateEntry {
                    name: source.name.clone(),
                    status: "error".into(),
                    commit: None,
                    perm_changes: 0,
                });
            }
        }
    }

    let updated_count = entries.iter().filter(|e| e.status == "updated").count();
    let error_count = entries.iter().filter(|e| e.status == "error").count();
    let skipped_count = entries
        .iter()
        .filter(|e| e.status == "skipped" || e.status == "cancelled")
        .count();
    let (role, summary) = match (updated_count, error_count, skipped_count) {
        (0, e, _) if e > 0 => (Role::Fail, format!("{} source(s) failed to update", e)),
        (_, 0, 0) => (Role::Ok, format!("Updated {} source(s)", updated_count)),
        _ => (
            Role::Warn,
            format!(
                "Updated {}, skipped {}, errored {}",
                updated_count, skipped_count, error_count
            ),
        ),
    };

    v2_printer.emit(
        Doc::new()
            .status(role, summary)
            .with_data(serde_json::json!({
                "sources": entries,
                "updated": updated_count,
                "skipped": skipped_count,
                "errors": error_count,
            })),
    );

    Ok(())
}
