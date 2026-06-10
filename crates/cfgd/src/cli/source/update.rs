use super::*;
use cfgd_core::output::{Doc, Printer, Role};

/// Build the `source update <name>` not-found error. Carries the typed
/// `SourceError::NotFound` in the chain so the exit-code downcast in `main.rs`
/// resolves to ExitCode::NotFound (6); the attached CliErrorMeta drives the
/// stable `{"error":"not_found",...}` payload.
fn source_not_found_error(name: &str) -> anyhow::Error {
    crate::cli::cli_error_ctx(
        cfgd_core::errors::CfgdError::Source(cfgd_core::errors::SourceError::NotFound {
            name: name.to_string(),
        })
        .into(),
        name,
        "not_found",
        format!("Source '{}' not found", name),
        serde_json::json!({}),
    )
}

pub fn cmd_source_update(cli: &Cli, printer: &Printer, name: Option<&str>) -> anyhow::Result<()> {
    let error_count = run_source_update(cli, printer, name)?;

    // A scripted consumer must be able to detect that a source failed to
    // update from the exit code alone. `run_source_update` already emitted the
    // summary Doc; exit nonzero directly here (mirroring cmd_status's
    // --exit-code path) so the failure isn't re-rendered as a second error
    // line by the central sink. Kept out of the core so the body above stays
    // unit-testable in-process (process::exit would abort the test binary).
    if error_count > 0 {
        cfgd_core::exit::ExitCode::Error.exit();
    }

    Ok(())
}

/// Core of `source update`: fetches each configured source, emits the summary
/// Doc, and returns the number of sources that failed to update.
pub(crate) fn run_source_update(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<usize> {
    printer.heading("Update Sources");

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        // A specific source was requested but the config has no sources: that is
        // a NotFound, not a success. Only the update-ALL form (name == None) is
        // an informational no-op here.
        if let Some(name) = name {
            return Err(source_not_found_error(name));
        }
        printer.emit(
            Doc::new()
                .status(Role::Info, "No sources configured")
                .with_data(serde_json::json!({ "sources": [] })),
        );
        return Ok(0);
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
        return Err(source_not_found_error(name));
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
                    // accept-confirm-then-success line nests under the same
                    // section header as the prompt context bullets. The
                    // section header phrasing pivots on whether permission
                    // changes were detected.
                    let source_sec = if perm_changes.is_empty() {
                        printer.section(format!("Source '{}'", source.name))
                    } else {
                        printer.section(format!(
                            "Source '{}' update changes permissions",
                            source.name
                        ))
                    };
                    for change in &perm_changes {
                        source_sec.status_simple(Role::Warn, change.description.clone());
                    }

                    let proceed = if !perm_changes.is_empty() {
                        match printer.prompt_confirm("Accept permission changes?") {
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

                        // Keep the sources lockfile in sync with the updated commit SHA.
                        if let Some(ref commit) = cached.last_commit {
                            let lock_entry = cfgd_core::config::SourceLockEntry {
                                name: source.name.clone(),
                                url: source.origin.url.clone(),
                                pin_version: source.sync.pin_version.clone(),
                                resolved_ref: cached.resolved_ref.clone(),
                                resolved_commit: commit.clone(),
                                locked_at: cfgd_core::utc_now_iso8601(),
                            };
                            let cfg_dir = config_dir(cli);
                            if let Err(e) =
                                cfgd_core::update_source_lock_entry(&cfg_dir, lock_entry)
                            {
                                source_sec.status_simple(
                                    Role::Warn,
                                    format!(
                                        "Could not update sources.lock: {}",
                                        cfgd_core::output::collapse_to_subject_line(&e)
                                    ),
                                );
                            }
                        }

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
                printer.status_simple(
                    Role::Fail,
                    format!(
                        "Failed to update source '{}': {}",
                        source.name,
                        cfgd_core::output::collapse_to_subject_line(&e),
                    ),
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

    printer.emit(
        Doc::new()
            .status(role, summary)
            .with_data(serde_json::json!({
                "sources": entries,
                "updated": updated_count,
                "skipped": skipped_count,
                "errors": error_count,
            })),
    );

    Ok(error_count)
}
