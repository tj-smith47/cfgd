use super::*;

// --- Sync Handler ---

/// Returns true if changes were detected during sync.
pub(crate) fn handle_sync(
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
        match git_pull(repo_path) {
            Ok(true) => {
                // Verify signature on new HEAD after pull if required
                if require_signed_commits
                    && !allow_unsigned
                    && let Err(e) = crate::sources::verify_head_signature(source_name, repo_path)
                {
                    tracing::error!(
                        source = %source_name,
                        error = %e,
                        "sync: signature verification failed after pull"
                    );
                    // Don't treat this as "changes" — the content is untrusted
                    return false;
                }
                tracing::info!("sync: pulled new changes from remote");
                changes = true;
            }
            Ok(false) => tracing::debug!("sync: already up to date"),
            Err(e) => tracing::warn!(error = %e, "sync: pull failed"),
        }
    }

    if auto_push {
        match git_auto_commit_push(repo_path) {
            Ok(true) => tracing::info!("sync: pushed local changes to remote"),
            Ok(false) => tracing::debug!("sync: nothing to push"),
            Err(e) => tracing::warn!(error = %e, "sync: push failed"),
        }
    }

    let rt = tokio::runtime::Handle::current();
    let source = source_name.to_string();
    let ts = timestamp.clone();
    rt.block_on(async {
        let mut st = state.lock().await;
        st.last_sync = Some(timestamp);
        for s in &mut st.sources {
            if s.name == source {
                s.last_sync = Some(ts.clone());
            }
        }
    });

    changes
}

// --- Version Check Handler ---

pub(crate) fn handle_version_check(state: &Arc<Mutex<DaemonState>>, notifier: &Arc<Notifier>) {
    tracing::info!("checking for cfgd updates");

    match crate::upgrade::check_with_cache(None, None) {
        Ok(check) => {
            if check.update_available {
                let version_str = check.latest.to_string();
                tracing::info!(
                    current = %check.current,
                    latest = %check.latest,
                    "update available"
                );

                // Check if we already notified about this version
                let rt = tokio::runtime::Handle::current();
                let already_notified = rt.block_on(async {
                    let st = state.lock().await;
                    st.update_available.as_deref() == Some(version_str.as_str())
                });

                // Update state
                let vs = version_str.clone();
                let st = Arc::clone(state);
                rt.block_on(async {
                    let mut st = st.lock().await;
                    st.update_available = Some(vs);
                });

                // Notify once per version
                if !already_notified {
                    notifier.notify(
                        "cfgd: update available",
                        &format!(
                            "Version {} is available (current: {}). Run 'cfgd upgrade' to update.",
                            version_str, check.current
                        ),
                    );
                }
            } else {
                tracing::debug!(
                    version = %check.current,
                    "cfgd is up to date"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "version check failed");
        }
    }
}

// --- Compliance Snapshot Handler ---

pub(crate) fn handle_compliance_snapshot(
    config_path: &Path,
    profile_override: Option<&str>,
    hooks: &dyn DaemonHooks,
    compliance_cfg: &config::ComplianceConfig,
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

    let resolved = match config::resolve_profile(profile_name, &profiles_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "compliance: profile resolution failed");
            return;
        }
    };

    let mut registry = hooks.build_registry(&cfg);
    hooks.extend_registry_custom_managers(&mut registry, &resolved.merged.packages);

    let source_names: Vec<String> = std::iter::once("local".to_string())
        .chain(cfg.spec.sources.iter().map(|s| s.name.clone()))
        .collect();

    let snapshot = match crate::compliance::collect_snapshot(
        profile_name,
        &resolved.merged,
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

    let store = match StateStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "compliance: state store error");
            return;
        }
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
            tracing::info!(path = %file_path.display(), "compliance snapshot exported");
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
