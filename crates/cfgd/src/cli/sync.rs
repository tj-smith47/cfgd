use super::*;

pub(super) fn cmd_sync(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Sync");

    let (cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    printer.newline();
    printer.info("Syncing local repo with remote...");

    // Pull local config repo
    match cfgd_core::daemon::git_pull_sync(&config_dir) {
        Ok(true) => printer.success("Pulled new changes from remote"),
        Ok(false) => printer.success("Already up to date"),
        Err(e) => printer.warning(&format!("Pull failed: {}", e)),
    }

    // Sync all configured sources
    if !cfg.spec.sources.is_empty() {
        printer.newline();
        printer.subheader("Sources");

        let cache_dir = source_cache_dir(cli)?;
        let mut mgr = SourceManager::new(&cache_dir);
        mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
        let mut changes_detected = false;

        for source_spec in &cfg.spec.sources {
            printer.info(&format!("Syncing source '{}'...", source_spec.name));

            // Capture old manifest before syncing (for permission change detection)
            let source_dir = cache_dir.join(&source_spec.name);
            let old_manifest = if source_dir.exists() {
                mgr.parse_manifest(&source_spec.name, &source_dir).ok()
            } else {
                None
            };

            match mgr.load_source(source_spec, printer) {
                Ok(()) => {
                    if let Some(cached) = mgr.get(&source_spec.name) {
                        // Detect permission-expanding changes
                        if let Some(ref old) = old_manifest {
                            let old_input =
                                build_permission_input(&source_spec.name, &old.spec.policy);
                            let new_input = build_permission_input(
                                &source_spec.name,
                                &cached.manifest.spec.policy,
                            );
                            let perm_changes =
                                composition::detect_permission_changes(&[old_input], &[new_input]);
                            if !perm_changes.is_empty() {
                                printer.newline();
                                printer.warning(&format!(
                                    "Source '{}' update changes permissions:",
                                    source_spec.name
                                ));
                                for change in &perm_changes {
                                    printer.warning(&format!("  - {}", change.description));
                                }
                                match printer.prompt_confirm("Accept permission changes?") {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        printer.info(&format!(
                                            "Skipped source '{}' (permission changes rejected)",
                                            source_spec.name
                                        ));
                                        continue;
                                    }
                                    Err(_) => {
                                        printer.info(&format!(
                                            "Skipped source '{}' (prompt cancelled)",
                                            source_spec.name
                                        ));
                                        continue;
                                    }
                                }
                            }
                        }

                        let commit_short = cached
                            .last_commit
                            .as_deref()
                            .map(|c| &c[..c.len().min(12)])
                            .unwrap_or("unknown");
                        printer.success(&format!(
                            "'{}' synced (commit: {})",
                            source_spec.name, commit_short
                        ));
                        changes_detected = true;
                    }
                }
                Err(e) => {
                    printer.warning(&format!("Failed to sync '{}': {}", source_spec.name, e));
                }
            }
        }

        if changes_detected {
            printer.newline();
            printer.info(&format!("Sources updated. {}", MSG_RUN_APPLY));
        }
    }

    Ok(())
}
