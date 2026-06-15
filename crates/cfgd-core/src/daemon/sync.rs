use super::*;
use crate::PathDisplayExt;

// --- Sync Handler ---

/// Returns true if changes were detected during sync.
///
/// Async because state mutation goes through `tokio::sync::Mutex`. The
/// blocking git operations (pull/push) are dispatched via `spawn_blocking`
/// internally so callers may invoke `handle_sync` from any async context
/// without deadlock risk.
pub(crate) async fn handle_sync(
    repo_path: &Path,
    auto_pull: bool,
    auto_push: bool,
    source_name: &str,
    state: &Arc<Mutex<DaemonState>>,
    require_signed_commits: bool,
    allow_unsigned: bool,
) -> bool {
    let timestamp = crate::utc_now_iso8601();
    let mut changes = false;

    if auto_pull {
        let repo = repo_path.to_path_buf();
        let pull_result = tokio::task::spawn_blocking(move || git_pull(&repo)).await;
        match pull_result {
            Ok(Ok(true)) => {
                // Verify signature on new HEAD after pull if required
                if require_signed_commits && !allow_unsigned {
                    let src = source_name.to_string();
                    let repo = repo_path.to_path_buf();
                    let verify_result = tokio::task::spawn_blocking(move || {
                        crate::sources::verify_head_signature(&src, &repo)
                    })
                    .await;
                    match verify_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            tracing::error!(
                                source = %source_name,
                                error = %e,
                                "sync: signature verification failed after pull"
                            );
                            // Don't treat this as "changes" — the content is untrusted
                            return false;
                        }
                        Err(e) => {
                            tracing::error!(
                                source = %source_name,
                                error = %e,
                                "sync: signature verification task panicked"
                            );
                            return false;
                        }
                    }
                }
                tracing::info!("sync: pulled new changes from remote");
                changes = true;
            }
            Ok(Ok(false)) => tracing::debug!("sync: already up to date"),
            Ok(Err(e)) => tracing::warn!(error = %e, "sync: pull failed"),
            Err(e) => tracing::error!(error = %e, "sync: pull task panicked"),
        }
    }

    if auto_push {
        let repo = repo_path.to_path_buf();
        let push_result = tokio::task::spawn_blocking(move || git_auto_commit_push(&repo)).await;
        match push_result {
            Ok(Ok(true)) => tracing::info!("sync: pushed local changes to remote"),
            Ok(Ok(false)) => tracing::debug!("sync: nothing to push"),
            Ok(Err(e)) => tracing::warn!(error = %e, "sync: push failed"),
            Err(e) => tracing::error!(error = %e, "sync: push task panicked"),
        }
    }

    {
        let mut st = state.lock().await;
        st.last_sync = Some(timestamp.clone());
        for s in &mut st.sources {
            if s.name == source_name {
                s.last_sync = Some(timestamp.clone());
            }
        }
    }

    changes
}

// --- Version Check Handler ---

/// Policy-driven self-update check for the daemon's periodic version tick.
///
/// The daemon is non-interactive, so the policy collapses to: `Manual` skips
/// the check entirely; `Auto` downloads + installs the update and restarts the
/// daemon; `Notify` and `Prompt` both surface a desktop notification (once per
/// version) without applying — `Prompt` degrades to notify because there is no
/// TTY to confirm against in the background.
///
/// Async because state mutation goes through `tokio::sync::Mutex` and the
/// blocking HTTP/install work is dispatched via `spawn_blocking` internally.
pub(crate) async fn handle_version_check(
    update_cfg: &config::UpdateConfig,
    state: &Arc<Mutex<DaemonState>>,
    notifier: &Arc<Notifier>,
) {
    use crate::config::UpdatePolicy;
    use crate::upgrade::UpdateAction;

    let policy = update_cfg.policy;
    let interval = crate::upgrade::resolved_interval(update_cfg);
    let now = crate::unix_secs_now();

    // Interval/Manual-gate first so a Manual policy or within-interval tick is a
    // no-op with no network call (the pump cadence is the upper bound; the
    // persisted timestamp gates across daemon restarts).
    if !crate::upgrade::should_check(policy, interval, now, crate::upgrade::last_checked_secs()) {
        tracing::debug!(?policy, "version check gated (Manual or within interval)");
        return;
    }

    tracing::info!("checking for cfgd updates");

    // Propagate the test-home thread-local across the spawn_blocking boundary;
    // the cache lookup reads it to redirect $HOME away from the real filesystem
    // during tests. No-op in production.
    let channel = update_cfg.channel.clone();
    let test_home = crate::test_home_override();
    let check_result = tokio::task::spawn_blocking(move || {
        let _guard = test_home.as_deref().map(crate::with_test_home_guard);
        crate::upgrade::check_latest(None, channel.as_deref(), None)
    })
    .await;

    crate::upgrade::record_check_at(now);

    let check = match check_result {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "version check failed");
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "version check task panicked");
            return;
        }
    };

    if !check.update_available {
        tracing::debug!(version = %check.current, "cfgd is up to date");
        // Binary current → the §9 consolidated skill-stale surface may apply
        // (rule 3). Rule 1 means this only runs when no binary update is
        // pending, so the two surfaces can never both fire.
        surface_stale_skills(update_cfg, state, notifier).await;
        return;
    }

    let version_str = check.latest.to_string();
    tracing::info!(current = %check.current, latest = %check.latest, "update available");

    // Binary update pending → binary surface only (rule 1 suppresses the skill
    // surface). The Auto apply path's ride-along refreshes user-scope skills in
    // the same action via `install_release`.
    match crate::upgrade::resolve_action(policy, false, false) {
        UpdateAction::Apply if policy == UpdatePolicy::Auto => {
            apply_daemon_update(update_cfg, &check, &version_str, state, notifier).await;
        }
        // Interactive Prompt cannot apply in the daemon; resolve_action already
        // degraded a non-interactive Prompt to Surface, so this arm only covers
        // the defensive case — surface rather than silently apply.
        UpdateAction::Apply | UpdateAction::Surface => {
            notify_update_available(&check, &version_str, state, notifier).await;
        }
        UpdateAction::Skip => {}
    }
}

/// Surface an available update via the notifier, deduped so the daemon notifies
/// at most once per version (tracked in `state.update_available`).
async fn notify_update_available(
    check: &crate::upgrade::UpdateCheck,
    version_str: &str,
    state: &Arc<Mutex<DaemonState>>,
    notifier: &Arc<Notifier>,
) {
    let already_notified = {
        let mut st = state.lock().await;
        let already = st.update_available.as_deref() == Some(version_str);
        st.update_available = Some(version_str.to_string());
        already
    };

    if !already_notified {
        notifier.notify(
            "cfgd: update available",
            &format!(
                "Version {} is available (current: {}). Run 'cfgd upgrade' to update.",
                version_str, check.current
            ),
        );
    }
}

/// Emit the §9 consolidated skill-stale surface in the daemon when the binary is
/// current (rule 3). The decision + effectful orchestration (the scope table,
/// `Auto` refresh → re-aggregate → project-only remainder) is single-sourced in
/// [`run_standalone_skill_action`](crate::upgrade::run_standalone_skill_action);
/// this function only renders the returned outcome via the notifier:
///
/// * **Auto / Inherit→Auto** → user-scope already re-rendered; a project-only
///   remainder is notified (project-scope is never written).
/// * **Notify / Prompt** → one consolidated notifier message covering both
///   scopes (`Prompt` cannot prompt in the daemon, so it surfaces like `Notify`
///   per the §9 "≤1 surface" headline).
/// * **Manual** → silent.
///
/// Deduped via `state.skills_stale_notified` so the notice fires at most once per
/// distinct staleness signature, not on every check tick.
async fn surface_stale_skills(
    update_cfg: &config::UpdateConfig,
    state: &Arc<Mutex<DaemonState>>,
    notifier: &Arc<Notifier>,
) {
    use crate::upgrade::StandaloneSkillOutcome;

    // Binary is current here (caller gates on `!update_available`), so
    // binary_available is false and rule 3 governs.
    if let StandaloneSkillOutcome::NoticeNeeded(staleness) =
        crate::upgrade::run_standalone_skill_action(update_cfg, false)
    {
        notify_stale_skills_once(staleness, state, notifier).await;
    }
}

/// Send the consolidated skill-stale notifier message, deduped per distinct
/// staleness signature via `state.skills_stale_notified`. Async like
/// [`notify_update_available`] so the dedup bookkeeping runs under
/// `.lock().await` and is never skipped on lock contention.
async fn notify_stale_skills_once(
    staleness: crate::upgrade::SkillStaleness,
    state: &Arc<Mutex<DaemonState>>,
    notifier: &Arc<Notifier>,
) {
    let signature = format!("user:{},project:{}", staleness.user, staleness.project);
    let already_notified = {
        let mut st = state.lock().await;
        let already = st.skills_stale_notified.as_deref() == Some(signature.as_str());
        st.skills_stale_notified = Some(signature);
        already
    };
    if !already_notified {
        notifier.notify(
            "cfgd: skills are stale",
            &crate::upgrade::consolidated_skill_stale_message(staleness),
        );
    }
}

/// Download, verify, and install an available update under `Auto` policy, then
/// restart the daemon so the new binary takes over. Records the version in
/// `state.update_available` so a failed install still surfaces once.
async fn apply_daemon_update(
    update_cfg: &config::UpdateConfig,
    check: &crate::upgrade::UpdateCheck,
    version_str: &str,
    state: &Arc<Mutex<DaemonState>>,
    notifier: &Arc<Notifier>,
) {
    let Some(release) = check.release.clone() else {
        tracing::warn!("auto-update: release info unavailable; surfacing instead");
        notify_update_available(check, version_str, state, notifier).await;
        return;
    };

    let test_home = crate::test_home_override();
    // Clone to cross the spawn_blocking boundary; the shared apply path runs the
    // user-scope skill ride-along gated by this config's effective skills policy.
    let cfg = update_cfg.clone();
    let install = tokio::task::spawn_blocking(move || {
        let _guard = test_home.as_deref().map(crate::with_test_home_guard);
        let asset = crate::upgrade::find_asset_for_platform(&release)?;
        // Shared apply path: install + invalidate cache + ride-along skill
        // refresh + restart a running daemon onto the new binary (one owner of
        // that ordering invariant).
        crate::upgrade::install_release(&release, asset, false, &cfg, None)
    })
    .await;

    match install {
        Ok(Ok(applied)) => {
            tracing::info!(
                version = %version_str,
                daemon_restarted = applied.daemon_restarted,
                "auto-update installed",
            );
            let restart_note = if applied.daemon_restarted {
                "; restarting daemon."
            } else {
                "."
            };
            notifier.notify(
                "cfgd: updated",
                &format!("Auto-updated to {version_str}{restart_note}"),
            );
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "auto-update install failed; surfacing instead");
            notify_update_available(check, version_str, state, notifier).await;
        }
        Err(e) => {
            tracing::error!(error = %e, "auto-update task panicked");
        }
    }
}

// --- Compliance Snapshot Handler ---

pub(crate) fn handle_compliance_snapshot(
    config_path: &Path,
    profile_override: Option<&str>,
    hooks: &dyn DaemonHooks,
    compliance_cfg: &config::ComplianceConfig,
    state_dir_override: Option<&Path>,
    scope: crate::Scope,
) {
    tracing::info!("running compliance snapshot");

    let cfg = match config::load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "compliance: config load failed");
            return;
        }
    };

    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let profiles_dir = config_dir.join("profiles");
    let profile_name = match profile_override.or(cfg.spec.profile.as_deref()) {
        Some(p) => p,
        None => {
            tracing::error!("compliance: no profile configured — skipping");
            return;
        }
    };

    let local_resolved = match config::resolve_profile(profile_name, &profiles_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "compliance: profile resolution failed");
            return;
        }
    };

    // Compose with sources CACHE-ONLY so the daemon-stored snapshot reflects the
    // same source-composed desired state every other surface does, without
    // touching the network in the compliance tick.
    //
    // FAIL-CLOSED: a real compose error (malformed/constraint-violating cached
    // manifest, failed signature) skips this snapshot rather than recording a
    // degraded local-only compliance picture that would falsely report
    // source-delivered resources as missing. Mirrors the resolve_profile arm
    // above (error + return). A benign never-synced cache-miss is warn+skip inside
    // the resolver, not an Err, so it still snapshots local-only.
    let printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
    let (resolved, source_module_roots) = match super::compose_daemon_desired_state(
        &cfg,
        &local_resolved,
        &printer,
        scope,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                error = %e,
                "compliance: source composition failed — skipping snapshot to avoid recording a degraded desired state"
            );
            return;
        }
    };

    let mut registry = hooks.build_registry(&cfg);
    hooks.extend_registry_custom_managers(&mut registry, &resolved.merged.packages);

    // Resolve the profile's modules (incl. source-delivered roots) so module
    // files/packages/system appear in the daemon-stored snapshot, matching the CLI
    // compliance surface.
    let resolved_modules = super::resolve_daemon_modules(
        &registry,
        &resolved,
        &config_dir,
        &source_module_roots,
        &printer,
        scope,
    );

    // Wire a content-aware file manager when this binary provides one. The
    // workstation agent does; absent it (default hook), daemon file checks stay
    // existence + permissions only (honest degradation), while the daemon's own
    // drift action is already content + module aware via the reconcile plan.
    match hooks.build_file_manager(&config_dir, &resolved) {
        Ok(fm) => registry.file_manager = fm,
        Err(e) => {
            tracing::warn!(error = %e, "compliance: file manager build failed — file checks degrade to existence + permissions");
        }
    }

    let source_names: Vec<String> = std::iter::once("local".to_string())
        .chain(cfg.spec.sources.iter().map(|s| s.name.clone()))
        .collect();

    let snapshot = match crate::compliance::collect_snapshot(
        profile_name,
        &resolved.merged,
        &resolved_modules,
        &config_dir,
        &registry,
        &compliance_cfg.scope,
        &source_names,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "compliance: snapshot collection failed");
            return;
        }
    };

    // Serialize for hashing and storage
    let json = match serde_json::to_string_pretty(&snapshot) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "compliance: snapshot serialization failed");
            return;
        }
    };

    let hash = crate::sha256_hex(json.as_bytes());

    let store = match state_dir_override {
        Some(d) => match StateStore::open_in_dir(d) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "compliance: state store error");
                return;
            }
        },
        None => match StateStore::open_default() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "compliance: state store error");
                return;
            }
        },
    };

    // Only store if state actually changed
    let latest_hash = match store.latest_compliance_hash() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "compliance: failed to query latest hash");
            None
        }
    };

    if latest_hash.as_deref() == Some(&hash) {
        tracing::debug!("compliance: no state change, skipping snapshot");
        return;
    }

    // Store the new snapshot
    if let Err(e) = store.store_compliance_snapshot(&snapshot, &hash) {
        tracing::error!(error = %e, "compliance: failed to store snapshot");
        return;
    }

    tracing::info!(
        compliant = snapshot.summary.compliant,
        warning = snapshot.summary.warning,
        violation = snapshot.summary.violation,
        "compliance snapshot stored"
    );

    // Export if configured
    match crate::compliance::export_snapshot_to_file(&snapshot, &compliance_cfg.export) {
        Ok(file_path) => {
            tracing::info!(path = %file_path.posix(), "compliance snapshot exported");
        }
        Err(e) => {
            tracing::error!(error = %e, "compliance: failed to export snapshot");
            return;
        }
    }

    // Prune old snapshots based on retention
    if let Ok(retention_dur) = crate::parse_duration_str(&compliance_cfg.retention) {
        let cutoff_secs = crate::unix_secs_now().saturating_sub(retention_dur.as_secs());
        let cutoff_str = crate::unix_secs_to_iso8601(cutoff_secs);
        match store.prune_compliance_snapshots(&cutoff_str) {
            Ok(deleted) if deleted > 0 => {
                tracing::info!(deleted = deleted, "compliance: pruned old snapshots");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "compliance: failed to prune snapshots");
            }
        }
    }
}
