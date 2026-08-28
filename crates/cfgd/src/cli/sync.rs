use super::*;

use cfgd_core::output::{Doc, OwnerLabel, Role};

pub fn cmd_sync(cli: &Cli, printer: &cfgd_core::output::Printer) -> anyhow::Result<()> {
    printer.heading("Sync");

    let (cfg, profile_name, _resolved) = load_config_and_profile(cli, printer)?;
    printer.kv_rows(cfgd_core::output::config_profile_rows(
        Some(&cli.config),
        Some(&profile_name),
    ));

    let config_dir = config_dir(cli);

    let mut sync_payload = SyncOutput {
        local_pulled: false,
        sources: Vec::new(),
    };

    {
        // The section keeps only its pull outcome: the header's `Config` row
        // already names this location, and stating it again three lines later
        // makes one fact read as two.
        let repo_sec = printer.section("Local Repo");
        let sp = repo_sec.spinner("Pulling from remote");
        match cfgd_core::daemon::git_pull_sync(&config_dir) {
            Ok(Some(movement)) => {
                sp.finish_ok("Pulled new changes from remote")
                    .detail(format!(
                        "commit: {} {} {}",
                        short_commit(&movement.from),
                        printer.arrow(),
                        short_commit(&movement.to)
                    ));
                sync_payload.local_pulled = true;
            }
            Ok(None) => {
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
        let sources_sec = printer.section(super::source::list::SOURCES_SECTION);
        let cache_dir = source_cache_dir(cli)?;
        let mut mgr = SourceManager::new(&cache_dir);
        mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
        let silent_printer = printer.at_verbosity(cfgd_core::output::Verbosity::Quiet);
        // Opened once: every open runs the full migration chain, and the loop
        // below records a fetch per source. Best-effort — the cache refreshes
        // either way, so a read-only state dir must not turn a successful sync
        // into a failure; it costs the freshness ledger, not the sync.
        let state = match open_state_store(cli.state_dir.as_deref(), cli.scope()) {
            Ok(state) => Some(state),
            Err(e) => {
                sources_sec
                    .status(Role::Warn, "Source fetches will not be recorded")
                    .detail(cfgd_core::output::collapse_to_subject_line(&e));
                None
            }
        };

        for source_spec in &cfg.spec.sources {
            let source_dir = cache_dir.join(&source_spec.name);
            // Read before the fetch: afterwards the checkout holds only where it
            // landed, and the line below has to say where it came from.
            let old_commit = SourceManager::head_commit(&source_dir);
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

            // One `source:<name>` group per source, opened whether or not
            // there is a second source: a stream of undifferentiated status
            // lines is what the owner grammar exists to remove, and a
            // one-source run reading differently from a two-source run is a
            // shape the reader has to learn twice. Every line below names its
            // outcome only — the group heading already says whose it is.
            let owner = sources_sec.section_owner(&OwnerLabel::new("source", &source_spec.name));
            // The owner's header would otherwise stay deferred until its first
            // committed child line — but its first action is a spinner (a live
            // region), and `load_source` runs under a DERIVED Quiet printer
            // whose `alert()`/Fail statuses still reach the shared live region
            // (see output-module.md, "LiveBarState is shared by every
            // renderer"). Committing now is exactly the case
            // `SectionGuard::commit_header` documents: without it, one of
            // those emissions can land above a header nothing has written yet.
            owner.commit_header();
            let sp = owner.spinner("Syncing");
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

                        // A ref that moved says so; one that did not names the
                        // commit it stayed on, rather than an arrow from a
                        // hash to itself.
                        let commit_detail =
                            match (old_commit.as_deref(), cached.last_commit.as_deref()) {
                                (Some(old), Some(new)) if old != new => format!(
                                    "commit: {} {} {}",
                                    short_commit(old),
                                    printer.arrow(),
                                    short_commit(new)
                                ),
                                (_, Some(new)) => format!("commit: {}", short_commit(new)),
                                (_, None) => "commit: unknown".to_string(),
                            };

                        let had_perm_changes = perm_changes.is_some();
                        let proceed = if let Some(perm_changes) = perm_changes {
                            sp.finish_warn("Permission changes need approval");
                            {
                                let perm_sec = owner.section("Permission Changes");
                                for change in &perm_changes {
                                    // The confirm below approves exactly the
                                    // text on this row, so the row escapes
                                    // rather than leaving it to the renderer's
                                    // fold, which STRIPS: the module review
                                    // screen's policy, applied to the other
                                    // surface that asks a yes/no about a
                                    // remote's own words.
                                    perm_sec.bullet(cfgd_core::escape_control_chars(
                                        &change.description,
                                    ));
                                }
                            }
                            match printer.prompt_confirm("Accept permission changes?") {
                                Ok(true) => true,
                                Ok(false) => {
                                    owner.status_simple(
                                        Role::Info,
                                        "Skipped (permission changes rejected)",
                                    );
                                    false
                                }
                                Err(_) => {
                                    owner.status_simple(Role::Info, "Skipped (prompt cancelled)");
                                    false
                                }
                            }
                        } else {
                            sp.finish_ok("Synced").detail(commit_detail.clone());
                            true
                        };

                        if proceed {
                            // Accept-after-prompt path: spinner already
                            // finished as Warn. Emit the canonical success
                            // line so human consumers see "'X' synced —
                            // commit: <hash>".
                            if had_perm_changes {
                                owner.status(Role::Ok, "Synced").detail(commit_detail);
                            }

                            // Record the fetch in the state store, the same way
                            // `source add` / `source update` do. Without this a
                            // `cfgd status` immediately after a successful sync
                            // reports the source as "not yet fetched": the
                            // freshness ledger only ever heard from the two
                            // `source` subcommands, never from the command
                            // whose whole job is refreshing sources.
                            if let Some(ref state) = state
                                && let Err(e) = state.upsert_config_source(
                                    &cfgd_core::state::ConfigSourceUpsert {
                                        name: &source_spec.name,
                                        origin_url: &source_spec.origin.url,
                                        origin_branch: &source_spec.origin.branch,
                                        last_commit: cached.last_commit.as_deref(),
                                        source_version: cached.manifest.metadata.version.as_deref(),
                                        pinned_version: source_spec.sync.pin_version.as_deref(),
                                        last_commit_signed: cached.head_signed,
                                    },
                                )
                            {
                                owner
                                    .status(Role::Warn, "Could not record the fetch")
                                    .detail(cfgd_core::output::collapse_to_subject_line(&e));
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
                                    owner.status_simple(
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
                    } else {
                        // `load_source` reported success but the cache holds
                        // nothing for this source — an internal inconsistency
                        // the spinner must still settle rather than leak.
                        sp.finish_fail("Sync failed").detail(
                            "load_source reported success but the source is not in the cache",
                        );
                        owner.hint(format!(
                            "Discard the cached checkout and retry with `cfgd source update {}`",
                            source_spec.name
                        ));
                        sync_payload.sources.push(SourceSyncOutput {
                            name: source_spec.name.clone(),
                            status: "failed".to_string(),
                            commit: None,
                        });
                    }
                }
                Err(e) => {
                    sp.finish_fail("Sync failed")
                        .detail(crate::cli::source::source_failure_detail(&e));
                    owner.hint(crate::cli::source::source_failure_next_step(
                        &e,
                        &source_spec.name,
                    ));
                    sync_payload.sources.push(SourceSyncOutput {
                        name: source_spec.name.clone(),
                        status: "failed".to_string(),
                        commit: None,
                    });
                }
            }
        }
    }

    let (verdict_role, verdict, verdict_detail) = sync_verdict(&sync_payload.sources);
    match verdict_detail {
        Some(detail) => {
            printer.status(verdict_role, verdict).detail(detail);
        }
        None => printer.status_simple(verdict_role, verdict),
    }

    let doc = if changes_detected {
        // The `source:<name>` rows above already said the sources updated; all
        // that is left to say is what to run next, in the one spelling every
        // other command uses for it.
        Doc::new().hint(MSG_RUN_APPLY).with_data(&sync_payload)
    } else {
        Doc::new().with_data(&sync_payload)
    };
    printer.emit(doc);

    Ok(())
}

/// The one line `cfgd sync` closes on, whichever way the run went.
///
/// A run whose sources all refused ended on the last `✗ Sync failed` row under
/// the last owner heading and said nothing about the run as a whole, while a
/// successful run closed with a verdict — so the only transcript with no
/// summary was the one a reader most needs summarized. The counts come from the
/// payload rows, which is the same set `-o json` carries, so the sentence and
/// the machine answer cannot disagree.
///
/// A run with no subscribed sources has only the local pull to report, and
/// there the verdict carries no count.
fn sync_verdict(sources: &[SourceSyncOutput]) -> (Role, &'static str, Option<String>) {
    let total = sources.len();
    let failed = sources.iter().filter(|s| s.status == "failed").count();
    let skipped = sources.iter().filter(|s| s.status == "skipped").count();
    let synced = total - failed - skipped;
    let noun = cfgd_core::plural_noun(total, "source");
    if failed > 0 {
        return (
            // no-next-step: each failed source hinted its own next step above
            Role::Fail,
            "Sync failed",
            Some(format!("{failed} of {total} {noun} refused")),
        );
    }
    if skipped > 0 {
        return (
            Role::Warn,
            "Synced",
            Some(format!("{synced} of {total} {noun}, {skipped} skipped")),
        );
    }
    if total == 0 {
        return (Role::Ok, "Synced", None);
    }
    (
        Role::Ok,
        "Synced",
        Some(cfgd_core::pluralize(total, "source")),
    )
}

/// Build the buffered `Doc` that carries the final `SyncOutput` payload.
/// Pure function so snapshot tests can drive the JSON path without standing
/// up a SourceManager.
pub fn build_sync_doc(output: &SyncOutput) -> Doc {
    Doc::new().with_data(output)
}
