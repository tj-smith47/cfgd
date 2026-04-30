use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusOutput {
    last_apply: Option<cfgd_core::state::ApplyRecord>,
    drift: Vec<cfgd_core::state::DriftEvent>,
    sources: Vec<cfgd_core::state::ConfigSourceRecord>,
    pending_decisions: Vec<cfgd_core::state::PendingDecision>,
    modules: Vec<ModuleStatusEntry>,
    managed_resources: Vec<cfgd_core::state::ManagedResource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModuleStatusEntry {
    name: String,
    packages: usize,
    files: usize,
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModuleStatus {
    name: String,
    packages: usize,
    files: usize,
    depends: Vec<String>,
    status: String,
    last_applied: Option<String>,
}

pub(super) fn cmd_status(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
) -> anyhow::Result<()> {
    if let Some(mod_name) = module_filter {
        return cmd_status_module(cli, printer, mod_name);
    }

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let state = open_state_store(cli.state_dir.as_deref())?;

    let last_apply = state.last_apply()?;
    let drift_events = state.unresolved_drift()?;
    let source_records = if !cfg.spec.sources.is_empty() {
        state.config_sources()?
    } else {
        vec![]
    };
    let pending = state.pending_decisions()?;
    let resources = state.managed_resources()?;

    // Build module status entries
    let config_dir = config_dir(cli);
    // default_module_cache_dir() can fail only if $HOME is unset; fall back to
    // empty PathBuf so the status display degrades gracefully instead of erroring.
    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
    // load_all_modules failure (e.g., malformed module YAML) should not abort a
    // read-only status query; degrade to an empty map so the rest still renders.
    let all_modules =
        modules::load_all_modules(&config_dir, &cache_base, printer).unwrap_or_default();
    let state_map = module_state_map(&state);
    let module_entries: Vec<ModuleStatusEntry> = resolved
        .merged
        .modules
        .iter()
        .map(|mod_ref| {
            let mod_name = modules::resolve_profile_module_name(mod_ref);
            let (pkg_count, file_count) = all_modules
                .get(mod_name)
                .map(|m| (m.spec.packages.len(), m.spec.files.len()))
                .unwrap_or((0, 0));
            let status = state_map
                .get(mod_name)
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "not applied".into());
            ModuleStatusEntry {
                name: mod_ref.clone(),
                packages: pkg_count,
                files: file_count,
                status,
            }
        })
        .collect();

    let has_drift = !drift_events.is_empty();

    if printer.is_structured() {
        printer.write_structured(&StatusOutput {
            last_apply,
            drift: drift_events,
            sources: source_records,
            pending_decisions: pending,
            modules: module_entries,
            managed_resources: resources,
        });
        if exit_code && has_drift {
            cfgd_core::exit::ExitCode::DriftDetected.exit();
        }
        return Ok(());
    }

    printer.header("Status");
    printer.newline();

    // Last apply
    if let Some(last) = &last_apply {
        printer.subheader("Last Apply");
        printer.key_value("Time", &last.timestamp);
        printer.key_value("Profile", &last.profile);
        printer.key_value(
            "Status",
            match last.status {
                cfgd_core::state::ApplyStatus::Success => "success",
                cfgd_core::state::ApplyStatus::Partial => "partial",
                cfgd_core::state::ApplyStatus::Failed => "failed",
                cfgd_core::state::ApplyStatus::InProgress => "in_progress",
            },
        );
        if let Some(ref summary) = last.summary {
            printer.key_value("Summary", summary);
        }
    } else {
        printer.info("No applies recorded yet");
    }

    // Drift summary
    printer.newline();
    printer.subheader("Drift");
    if drift_events.is_empty() {
        printer.success("No drift detected");
    } else {
        for event in &drift_events {
            let source_info = if event.source != "local" {
                format!(" [{}]", event.source)
            } else {
                String::new()
            };
            printer.warning(&format!(
                "{} {} — want: {}, have: {}{}",
                event.resource_type,
                event.resource_id,
                event.expected.as_deref().unwrap_or("?"),
                event.actual.as_deref().unwrap_or("?"),
                source_info,
            ));
        }
    }

    // Config sources
    if !cfg.spec.sources.is_empty() {
        printer.newline();
        printer.subheader("Config Sources");
        if source_records.is_empty() {
            for source in &cfg.spec.sources {
                printer.key_value(&source.name, "not yet fetched");
            }
        } else {
            let rows: Vec<Vec<String>> = source_records
                .iter()
                .map(|s| {
                    vec![
                        s.name.clone(),
                        s.status.clone(),
                        s.source_version.clone().unwrap_or_else(|| "-".into()),
                        s.last_fetched.clone().unwrap_or_else(|| "never".into()),
                    ]
                })
                .collect();
            printer.table(&["Source", "Status", "Version", "Last Fetched"], &rows);
        }
    }

    // Pending decisions
    if !pending.is_empty() {
        printer.newline();
        printer.subheader("Pending Decisions");
        display_pending_decisions(printer, &pending);
    }

    // Modules
    if !resolved.merged.modules.is_empty() {
        printer.newline();
        printer.subheader("Modules");

        for mod_ref in &resolved.merged.modules {
            let mod_name = modules::resolve_profile_module_name(mod_ref);
            let (pkg_count, file_count) = all_modules
                .get(mod_name)
                .map(|m| (m.spec.packages.len(), m.spec.files.len()))
                .unwrap_or((0, 0));

            let summary = format!("{} pkgs, {} files", pkg_count, file_count);
            if let Some(state_rec) = state_map.get(mod_name) {
                if state_rec.status == "installed" {
                    printer.success(&format!("{}: {}, {}", mod_ref, summary, state_rec.status));
                } else {
                    printer.warning(&format!("{}: {}, {}", mod_ref, summary, state_rec.status));
                }
            } else {
                printer.info(&format!("{}: {}, not yet applied", mod_ref, summary));
            }
        }
    }

    // Managed resources
    if !resources.is_empty() {
        printer.newline();
        printer.subheader("Managed Resources");
        printer.table(
            &["Type", "Resource", "Source"],
            &resources
                .iter()
                .map(|r| {
                    vec![
                        r.resource_type.clone(),
                        r.resource_id.clone(),
                        r.source.clone(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    if exit_code && has_drift {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

pub(super) fn cmd_status_module(
    cli: &Cli,
    printer: &Printer,
    mod_name: &str,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    // Propagate (vs. unwrap_or_default in cmd_status): the module-scoped path
    // queries a single named module, so a missing cache dir means we cannot
    // answer the user's specific question and should error rather than silently
    // claim "module not found".
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, printer)?;

    let module = match all_modules.get(mod_name) {
        Some(m) => m,
        None => {
            // Module not found — show empty status gracefully
            if printer.is_structured() {
                printer.write_structured(&ModuleStatus {
                    name: mod_name.to_string(),
                    packages: 0,
                    files: 0,
                    depends: vec![],
                    status: "not found".into(),
                    last_applied: None,
                });
            } else {
                printer.header(&format!("Status: {}", mod_name));
                printer.info(&format!("Module '{}' not found", mod_name));
            }
            return Ok(());
        }
    };

    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_rec = state.module_state_by_name(mod_name)?;

    if printer.is_structured() {
        let status = state_rec
            .as_ref()
            .map(|s| s.status.clone())
            .unwrap_or_else(|| "not applied".into());
        let last_applied = state_rec.as_ref().map(|s| s.installed_at.clone());
        printer.write_structured(&ModuleStatus {
            name: mod_name.to_string(),
            packages: module.spec.packages.len(),
            files: module.spec.files.len(),
            depends: module.spec.depends.clone(),
            status,
            last_applied,
        });
        return Ok(());
    }

    printer.header(&format!("Status: {}", mod_name));

    // Module info
    printer.key_value("Packages", &module.spec.packages.len().to_string());
    printer.key_value("Files", &module.spec.files.len().to_string());
    if !module.spec.depends.is_empty() {
        printer.key_value("Dependencies", &module.spec.depends.join(", "));
    }

    // State from DB
    if let Some(rec) = &state_rec {
        printer.key_value("Status", &rec.status);
        printer.key_value("Last applied", &rec.installed_at);
        printer.key_value("Packages hash", &rec.packages_hash);
        printer.key_value("Files hash", &rec.files_hash);
    } else {
        printer.key_value("Status", "not applied");
    }

    // Show deployed files from manifest
    let deployed_files = state.module_deployed_files(mod_name)?;
    if !deployed_files.is_empty() {
        printer.newline();
        printer.subheader("Deployed Files");
        for f in &deployed_files {
            let exists = std::path::Path::new(&f.file_path).exists();
            if exists {
                printer.success(&f.file_path);
            } else {
                printer.error(&format!("{} (missing)", f.file_path));
            }
        }
    }

    Ok(())
}
