use super::*;

use cfgd_core::output::{Doc, OwnerLabel, Role};

pub fn cmd_sync(cli: &Cli, printer: &cfgd_core::output::Printer) -> anyhow::Result<()> {
    // A leg that refused must not read as success to a CI `&&` chain, the same
    // reason `apply` exits nonzero on a partial run. The rows and the payload
    // are already flushed by `run_sync`, so this exits directly rather than
    // returning an error nothing new could say.
    if sync_refused(&run_sync(cli, printer)?) {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}

/// Whether a leg of the run refused, which is what the process exit reports.
///
/// A source the reader declined at the permission prompt is an answered
/// question, not a refusal: the run did what they said. A source that failed,
/// a local repository that could not be pulled and a configuration that could
/// not be resolved at all are the three outcomes nobody chose.
pub fn sync_refused(payload: &SyncOutput) -> bool {
    payload.local_pull_error.is_some()
        || payload.config_resolution_error.is_some()
        || payload.sources.iter().any(|s| s.status.refused())
}

/// Whether a header-resolution failure is the stale pre-fetch reading that this
/// very run repairs.
///
/// The header composes the configuration as the command FOUND it, offline from
/// the source cache, so a failure it raises off a CACHED checkout is a reading
/// of bytes the `Sources` section below is about to replace. The fetch leg
/// discards and re-clones a checkout that records no origin or the wrong one,
/// and fast-forwards the rest, so a commit whose signature the subscription now
/// refuses, a manifest that will not parse and a manifest offering nothing are
/// starting points rather than verdicts: reported as refusals they would fail
/// the very run that fixes them.
///
/// What makes that arm safe is that the loop RE-JUDGES each of the three:
/// `load_source` runs the identical parse and signature check on the fetched
/// tree, so a fault the fetch does not clear comes back as a failed
/// `source:<name>` row and the run still exits nonzero. A kind nothing in the
/// loop re-judges must stay a refusal however cache-sourced it looks, or a
/// permanent fault exits 0 behind a `Role::Info` line that `-o json` swallows.
///
/// The other kinds are not the cache's to fix. A source the config does not
/// declare, an invalid source name, a cache directory that cannot be created,
/// and a fetch or pin failure the section below reports on its own row all keep
/// the nonzero exit they had before the header stopped propagating with `?`, as
/// does everything outside `SourceError` (a source whose constraints
/// `ConstraintMode::Enforce` refuses; a `ModuleError` for a module body a stale
/// cache does not hold, which the fetch WOULD deliver but nothing in the loop
/// re-resolves, so it is the same ceiling the subscription-profile arm states).
///
/// Judged on the error's own KIND, and matched with no wildcard so a new
/// `SourceError` variant is classified before this compiles. A message match
/// would break the moment a sentence is reworded, and the sentences are display
/// text.
pub(super) fn resolution_failure_the_fetch_rejudges(e: &anyhow::Error) -> bool {
    use cfgd_core::errors::SourceError;
    let Some(cfgd_core::errors::CfgdError::Source(source)) =
        e.downcast_ref::<cfgd_core::errors::CfgdError>()
    else {
        return false;
    };
    match source {
        SourceError::SignatureVerificationFailed { .. }
        | SourceError::InvalidManifest { .. }
        | SourceError::EmptyProvides { .. } => true,
        // `ProfileNotFound` is cache-sourced and still refuses: nothing in the
        // `Sources` loop re-resolves `subscription.profile` after the fetch, so
        // a permanent typo would exit 0 behind a `Role::Info` line that
        // `-o json` swallows. The reachable miss is
        // `ConfigError::ProfileNotFound` (raised by
        // `SourceManager::compose`), which the let-else above already refuses;
        // this variant only becomes live if `load_source_profile` is wired up,
        // and it must not change the answer when it is.
        SourceError::ProfileNotFound { .. }
        | SourceError::NotFound { .. }
        | SourceError::FetchFailed { .. }
        | SourceError::PinRefNotFound { .. }
        | SourceError::CacheError { .. }
        | SourceError::GitError { .. } => false,
    }
}

/// Drive the sync and return the payload it settled, so a caller can map a
/// refused leg onto a nonzero process exit and a test can read the outcome
/// without the process leaving under it.
pub fn run_sync(cli: &Cli, printer: &cfgd_core::output::Printer) -> anyhow::Result<SyncOutput> {
    printer.heading("Sync");

    // The configuration as this command FOUND it. The body below reports what
    // the pull changed, and the plan the closing hint invites reads the new
    // set — so the header describes the starting point, exactly as `Config`
    // and `Profile` beside it do.
    // `fetching_sources`: the two advisories this resolution can raise both
    // tell the reader to run `cfgd sync` to fetch a stale or missing checkout,
    // which is the command printing them, three lines above the section that
    // does the fetching. Everything else the composition has to say — a
    // constraint violation, a conflict, the `allowScripts` disclosure — is
    // exactly what this verb is the right place to hear.
    let ctx = RunContext::new(cli, printer).fetching_sources();
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    // The starting point is a header FACT, never a gate on the run. This
    // resolution reads the source cache offline, and a cached checkout can be
    // unusable in exactly the way `cfgd sync` exists to repair — a refused
    // signature after the subscription tightened its demand, a manifest whose
    // fix has already been pushed. Propagated, that refusal aborted the run
    // before its `Sources` section opened, so the one command that could
    // replace the offending commit was the one that commit locked out, and the
    // source stayed refused however many signed commits landed upstream. The
    // reason is reported and the header names what it still can; the fetch
    // below is what re-judges it.
    let desired = resolve_desired_state(
        &ctx,
        cfg,
        local_resolved,
        &[],
        false,
        printer,
        false,
        composition::ConstraintMode::Report,
    );
    // The failure the header reports, kept for the payload below so a `-o json`
    // consumer sees what the human line said: `-o json` forces Quiet, which
    // swallows every role but `Fail`, and a CI run reading only `sources` would
    // otherwise be told the whole run succeeded over a configuration that
    // cannot resolve at all.
    let mut config_resolution_error = None;
    // The `Sources` row names what the config SUBSCRIBES to, which a refused
    // resolution does not change — and this run is the fetch those very
    // subscriptions are about, so the row is most load-bearing exactly where
    // the composition failed.
    let declared = cfgd_core::reconciler::ComposedSource::from_declared(&cfg.spec.sources);
    let (header_sources, header_modules) = match &desired {
        Ok(desired) => (
            desired.sources.as_slice(),
            cfgd_core::output::HeaderModule::of_resolved(&desired.modules),
        ),
        Err(e) => {
            let detail = cfgd_core::output::collapse_to_subject_line(e);
            if resolution_failure_the_fetch_rejudges(e) {
                // A side report, not a verdict: the same sentence opens the run
                // that then repairs it, and a warning about an unsigned HEAD
                // three lines above a green `✓ Synced` reads as a contradiction.
                // The `source:<name>` row below is the only thing that judges.
                printer
                    .status(
                        Role::Info,
                        "Starting point could not be resolved from the cached checkout",
                    )
                    .detail(format!("{detail}; the fetch below judges the new head"));
            } else {
                printer
                    .status(
                        Role::Warn,
                        "Could not resolve the configuration as it stands",
                    )
                    .detail(&detail);
                config_resolution_error = Some(detail);
            }
            (declared.as_slice(), Vec::new())
        }
    };
    // A refused resolution named nothing to inherit either — the chain came
    // out of the same composition the `Err` arm above already gave up on.
    let profile_inherits: Vec<String> = desired
        .as_ref()
        .map(|d| d.resolved.inherits_chain())
        .unwrap_or_default();
    printer.kv_rows(cfgd_core::output::config_header_rows(
        &cfgd_core::output::ConfigHeader {
            config_path: Some(&cli.config),
            sources: header_sources,
            profile: Some(profile_name),
            profile_inherits: &profile_inherits,
            modules: &header_modules,
            arrow: printer.arrow(),
        },
    ));

    let config_dir = ctx.config_dir().to_path_buf();

    let mut sync_payload = SyncOutput {
        local_pulled: false,
        local_pull_error: None,
        config_resolution_error,
        sources: Vec::new(),
    };

    // A config directory under no version control has nothing to pull, so it
    // opens no section at all — the pull is one leg of this run among several.
    if cfgd_core::daemon::is_git_repository(&config_dir) {
        // The section keeps only its pull outcome: the header's `Config` row
        // already names this location, and stating it again three lines later
        // makes one fact read as two.
        let repo_sec = printer.section("Local Repo");
        let sp = repo_sec.spinner("Pulling from remote");
        match cfgd_core::daemon::git_pull_sync(&config_dir) {
            cfgd_core::daemon::PullOutcome::Moved(movement) => {
                sp.finish_ok("Pulled new changes from remote")
                    .detail(format!(
                        "commit: {} {} {}",
                        short_commit(&movement.from),
                        printer.arrow(),
                        short_commit(&movement.to)
                    ));
                sync_payload.local_pulled = true;
            }
            cfgd_core::daemon::PullOutcome::UpToDate => {
                sp.finish_ok("Already up to date");
            }
            cfgd_core::daemon::PullOutcome::Failed(e) => {
                sp.finish_warn("Pull failed")
                    .detail(cfgd_core::daemon::pull_failure_summary(&e.message));
                repo_sec.hint(local_pull_next_step(&e, "cfgd sync"));
                sync_payload.local_pull_error = Some(e.message);
            }
            // The probe above said otherwise, so the checkout went away
            // between the two reads — the section is already open, and the
            // same sentence `cfgd pull` closes on is what it came to.
            cfgd_core::daemon::PullOutcome::NotARepository => {
                sp.finish_skipped(MSG_NOT_A_REPOSITORY);
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
                                status: SourceOutcome::Synced,
                                commit: cached.last_commit.clone(),
                            });
                        } else {
                            sync_payload.sources.push(SourceSyncOutput {
                                name: source_spec.name.clone(),
                                status: SourceOutcome::Skipped,
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
                            status: SourceOutcome::Failed,
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
                        status: SourceOutcome::Failed,
                        commit: None,
                    });
                }
            }
        }
    }

    let (verdict_role, verdict, verdict_detail) = sync_verdict(&sync_payload);
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
        Doc::new().hint(MSG_RUN_APPLY)
    } else {
        Doc::new()
    };
    printer.emit(doc.with_data(&sync_payload));

    Ok(sync_payload)
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
///
/// The local pull is one of the run's legs, so a pull that refused withholds
/// the success verdict: `✓ Synced` two lines under `⚠ Pull failed` claimed the
/// very thing the row above it denied. A configuration the header could not
/// resolve withholds it for the same reason, and is the same nonzero exit.
fn sync_verdict(payload: &SyncOutput) -> (Role, &'static str, Option<String>) {
    let sources = &payload.sources;
    let total = sources.len();
    let failed = sources.iter().filter(|s| s.status.refused()).count();
    let skipped = sources.iter().filter(|s| s.status.declined()).count();
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
    let unpulled = payload.local_pull_error.is_some();
    let unresolved = payload.config_resolution_error.is_some();
    if skipped > 0 || unpulled || unresolved {
        let mut detail = Vec::new();
        if total > 0 {
            detail.push(if skipped > 0 {
                format!("{synced} of {total} {noun}, {skipped} skipped")
            } else {
                format!("{total} {noun} synced")
            });
        }
        if unpulled {
            detail.push("local repo not pulled".to_string());
        }
        if unresolved {
            detail.push("configuration not resolved".to_string());
        }
        return (
            // no-next-step: the failed pull, the unresolvable configuration and
            // each skipped source hinted their own next step above
            Role::Warn,
            // A pull nothing verified cannot be reported as one: the word
            // says what the run came to, not what it set out to do. A
            // configuration that will not resolve is the same withholding —
            // and it is what the nonzero exit reports.
            if unpulled || unresolved {
                "Sync incomplete"
            } else {
                "Synced"
            },
            Some(detail.join(", ")),
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
