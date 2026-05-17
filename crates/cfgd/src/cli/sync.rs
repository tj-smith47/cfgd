use super::*;

use cfgd_core::output_v2::{Doc, Role};

pub fn cmd_sync(
    cli: &Cli,
    printer: &Printer,
    v2_printer: &cfgd_core::output_v2::Printer,
) -> anyhow::Result<()> {
    v2_printer.heading("Sync");

    let (cfg, profile_name, _resolved) = load_config_and_profile_v2(cli)?;
    v2_printer.kv_block([
        ("Config".to_string(), cli.config.display().to_string()),
        ("Profile".to_string(), profile_name),
    ]);

    let config_dir = config_dir(cli);

    let mut sync_payload = SyncOutput {
        local_pulled: false,
        sources: Vec::new(),
    };

    {
        let repo_sec = v2_printer.section("Local repo");
        let sp = repo_sec.spinner("Pulling from remote");
        match cfgd_core::daemon::git_pull_sync(&config_dir) {
            Ok(true) => {
                sp.finish_ok("Pulled new changes from remote");
                sync_payload.local_pulled = true;
            }
            Ok(false) => {
                sp.finish_ok("Already up to date");
            }
            Err(e) => {
                sp.finish_warn("Pull failed").detail(e.to_string());
            }
        }
    }

    let mut changes_detected = false;

    if !cfg.spec.sources.is_empty() {
        let sources_sec = v2_printer.section("Sources");
        let cache_dir = source_cache_dir(cli)?;
        let mut mgr = SourceManager::new(&cache_dir);
        mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));

        for source_spec in &cfg.spec.sources {
            let source_dir = cache_dir.join(&source_spec.name);
            let old_manifest = if source_dir.exists() {
                mgr.parse_manifest(&source_spec.name, &source_dir).ok()
            } else {
                None
            };

            let sp = sources_sec.spinner(format!("Syncing source '{}'", source_spec.name));
            let load_result = mgr.load_source(source_spec, printer);
            match load_result {
                Ok(()) => {
                    if let Some(cached) = mgr.get(&source_spec.name) {
                        let perm_changes = old_manifest.as_ref().and_then(|old| {
                            let old_input =
                                build_permission_input(&source_spec.name, &old.spec.policy);
                            let new_input = build_permission_input(
                                &source_spec.name,
                                &cached.manifest.spec.policy,
                            );
                            let changes =
                                composition::detect_permission_changes(&[old_input], &[new_input]);
                            if changes.is_empty() {
                                None
                            } else {
                                Some(changes)
                            }
                        });

                        if let Some(perm_changes) = perm_changes {
                            sp.finish_warn(format!(
                                "'{}' has permission changes",
                                source_spec.name
                            ));
                            {
                                let perm_sec = sources_sec
                                    .section(format!("'{}' permission changes", source_spec.name));
                                for change in &perm_changes {
                                    perm_sec.bullet(change.description.clone());
                                }
                            }
                            match v2_printer.prompt_confirm("Accept permission changes?") {
                                Ok(true) => {
                                    sources_sec.status_simple(
                                        Role::Info,
                                        format!("Accepted '{}'", source_spec.name),
                                    );
                                }
                                Ok(false) => {
                                    sources_sec.status_simple(
                                        Role::Info,
                                        format!(
                                            "Skipped source '{}' (permission changes rejected)",
                                            source_spec.name
                                        ),
                                    );
                                    sync_payload.sources.push(SourceSyncOutput {
                                        name: source_spec.name.clone(),
                                        status: "skipped".to_string(),
                                        commit: None,
                                    });
                                    continue;
                                }
                                Err(_) => {
                                    sources_sec.status_simple(
                                        Role::Info,
                                        format!(
                                            "Skipped source '{}' (prompt cancelled)",
                                            source_spec.name
                                        ),
                                    );
                                    sync_payload.sources.push(SourceSyncOutput {
                                        name: source_spec.name.clone(),
                                        status: "skipped".to_string(),
                                        commit: None,
                                    });
                                    continue;
                                }
                            }
                        } else {
                            let commit_short = cached
                                .last_commit
                                .as_deref()
                                .map(|c| &c[..c.len().min(12)])
                                .unwrap_or("unknown");
                            sp.finish_ok(format!("'{}' synced", source_spec.name))
                                .detail(format!("commit: {}", commit_short));
                        }

                        changes_detected = true;
                        sync_payload.sources.push(SourceSyncOutput {
                            name: source_spec.name.clone(),
                            status: "synced".to_string(),
                            commit: cached.last_commit.clone(),
                        });
                    }
                }
                Err(e) => {
                    sp.finish_fail(format!("Failed to sync '{}'", source_spec.name))
                        .detail(e.to_string());
                    sync_payload.sources.push(SourceSyncOutput {
                        name: source_spec.name.clone(),
                        status: "failed".to_string(),
                        commit: None,
                    });
                }
            }
        }
    }

    let doc = if changes_detected {
        Doc::new()
            .status(Role::Info, format!("Sources updated. {}", MSG_RUN_APPLY))
            .with_data(&sync_payload)
    } else {
        Doc::new().with_data(&sync_payload)
    };
    v2_printer.emit(doc);

    Ok(())
}

/// Build the buffered `Doc` that carries the final `SyncOutput` payload.
/// Pure function so snapshot tests can drive the JSON path without standing
/// up a SourceManager.
pub fn build_sync_doc(output: &SyncOutput) -> Doc {
    Doc::new().with_data(output)
}
