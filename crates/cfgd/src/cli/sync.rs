use super::*;

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Role};

pub fn cmd_sync(cli: &Cli, printer: &cfgd_core::output::Printer) -> anyhow::Result<()> {
    printer.heading("Sync");

    let (cfg, profile_name, _resolved) = load_config_and_profile(cli)?;
    printer.kv_block([
        ("Config".to_string(), cli.config.display_posix()),
        ("Profile".to_string(), profile_name),
    ]);

    let config_dir = config_dir(cli);

    let mut sync_payload = SyncOutput {
        local_pulled: false,
        sources: Vec::new(),
    };

    {
        let repo_sec = printer.section("Local repo");
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
                sp.finish_warn("Pull failed")
                    .detail(cfgd_core::output::collapse_to_subject_line(e));
            }
        }
    }

    let mut changes_detected = false;

    if !cfg.spec.sources.is_empty() {
        let sources_sec = printer.section("Sources");
        let cache_dir = source_cache_dir(cli)?;
        let mut mgr = SourceManager::new(&cache_dir);
        mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
        let silent_printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);

        // Multi-source sync emits N undifferentiated status streams. The
        // per-source `secondary` line acts as a structural pivot — color
        // groups each block visually so the eye can jump source-to-source
        // without re-reading the spinner labels. Single-source runs still
        // emit the header (consistent shape > one-off branches).
        let multi_source = cfg.spec.sources.len() > 1;
        for source_spec in &cfg.spec.sources {
            let source_dir = cache_dir.join(&source_spec.name);
            let old_manifest = if source_dir.exists() {
                match mgr.parse_manifest(&source_spec.name, &source_dir) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        tracing::debug!(
                            source = %source_spec.name,
                            error = %e,
                            "could not parse existing source manifest; treating as no prior state"
                        );
                        None
                    }
                }
            } else {
                None
            };

            if multi_source {
                sources_sec.status_simple(Role::Secondary, format!("Source: {}", source_spec.name));
            }
            let sp = sources_sec.spinner(format!("Syncing source '{}'", source_spec.name));
            let load_result = mgr.load_source(source_spec, &silent_printer);
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

                        let commit_short = cached
                            .last_commit
                            .as_deref()
                            .map(|c| &c[..c.len().min(12)])
                            .unwrap_or("unknown")
                            .to_string();

                        let had_perm_changes = perm_changes.is_some();
                        let proceed = if let Some(perm_changes) = perm_changes {
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
                            match printer.prompt_confirm("Accept permission changes?") {
                                Ok(true) => true,
                                Ok(false) => {
                                    sources_sec.status_simple(
                                        Role::Info,
                                        format!(
                                            "Skipped source '{}' (permission changes rejected)",
                                            source_spec.name
                                        ),
                                    );
                                    false
                                }
                                Err(_) => {
                                    sources_sec.status_simple(
                                        Role::Info,
                                        format!(
                                            "Skipped source '{}' (prompt cancelled)",
                                            source_spec.name
                                        ),
                                    );
                                    false
                                }
                            }
                        } else {
                            sp.finish_ok(format!("'{}' synced", source_spec.name))
                                .detail(format!("commit: {}", commit_short));
                            true
                        };

                        if proceed {
                            // Accept-after-prompt path: spinner already
                            // finished as Warn. Emit the canonical success
                            // line so human consumers see "'X' synced —
                            // commit: <hash>".
                            if had_perm_changes {
                                sources_sec
                                    .status(Role::Ok, format!("'{}' synced", source_spec.name))
                                    .detail(format!("commit: {}", commit_short));
                            }

                            // Record the resolved commit in sources.lock.
                            if let Some(ref commit) = cached.last_commit {
                                let lock_entry = cfgd_core::config::SourceLockEntry {
                                    name: source_spec.name.clone(),
                                    url: source_spec.origin.url.clone(),
                                    pin_version: source_spec.sync.pin_version.clone(),
                                    resolved_ref: cached.resolved_ref.clone(),
                                    resolved_commit: commit.clone(),
                                    locked_at: cfgd_core::utc_now_iso8601(),
                                };
                                if let Err(e) =
                                    cfgd_core::update_source_lock_entry(&config_dir, lock_entry)
                                {
                                    sources_sec.status_simple(
                                        cfgd_core::output::Role::Warn,
                                        crate::cli::source::sources_lock_update_warning(&e),
                                    );
                                }
                            }

                            changes_detected = true;
                            sync_payload.sources.push(SourceSyncOutput {
                                name: source_spec.name.clone(),
                                status: "synced".to_string(),
                                commit: cached.last_commit.clone(),
                            });
                        } else {
                            sync_payload.sources.push(SourceSyncOutput {
                                name: source_spec.name.clone(),
                                status: "skipped".to_string(),
                                commit: None,
                            });
                        }
                    }
                }
                Err(e) => {
                    sp.finish_fail(format!("Failed to sync '{}'", source_spec.name))
                        .detail(cfgd_core::output::collapse_to_subject_line(e));
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
    printer.emit(doc);

    Ok(())
}

/// Build the buffered `Doc` that carries the final `SyncOutput` payload.
/// Pure function so snapshot tests can drive the JSON path without standing
/// up a SourceManager.
pub fn build_sync_doc(output: &SyncOutput) -> Doc {
    Doc::new().with_data(output)
}
