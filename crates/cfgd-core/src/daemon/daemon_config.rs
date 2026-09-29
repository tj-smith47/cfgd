use super::*;

// --- Parsed Daemon Config ---

/// Parsed daemon configuration values with defaults applied.
pub(crate) struct ParsedDaemonConfig {
    pub(crate) reconcile_interval: Duration,
    pub(crate) sync_interval: Duration,
    pub(crate) auto_pull: bool,
    pub(crate) auto_push: bool,
    pub(crate) on_change_reconcile: bool,
    pub(crate) notify_on_drift: bool,
    pub(crate) notify_method: NotifyMethod,
    pub(crate) webhook_url: Option<String>,
    pub(crate) auto_apply: bool,
}

pub(crate) fn parse_daemon_config(daemon_cfg: &config::DaemonConfig) -> ParsedDaemonConfig {
    let reconcile = daemon_cfg.reconcile_effective();
    let sync = daemon_cfg.sync_effective();
    let notify = daemon_cfg.notify_effective();
    ParsedDaemonConfig {
        reconcile_interval: parse_duration_or_default(&reconcile.interval),
        sync_interval: parse_duration_or_default(&sync.interval),
        auto_pull: sync.auto_pull,
        auto_push: sync.auto_push,
        on_change_reconcile: reconcile.on_change,
        notify_on_drift: notify.drift,
        notify_method: notify.method.clone(),
        webhook_url: notify.webhook_url.clone(),
        auto_apply: reconcile.auto_apply,
    }
}

/// Build the list of per-module and default reconcile tasks from daemon config and resolved
/// profile.
///
/// For each module in the resolved profile, checks if reconcile patches produce effective
/// settings that differ from the global config. If so, creates a dedicated per-module task.
/// Always appends a `__default__` task for non-patched resources.
pub(crate) fn build_reconcile_tasks(
    daemon_cfg: &config::DaemonConfig,
    resolved: Option<&config::ResolvedProfile>,
    profile_chain: &[&str],
    reconcile_interval: Duration,
    auto_apply: bool,
) -> Vec<ReconcileTask> {
    let reconcile_patches = &daemon_cfg.reconcile_effective().patches[..];

    let mut tasks: Vec<ReconcileTask> = Vec::new();

    if !reconcile_patches.is_empty() {
        // Warn on duplicate patches
        let mut seen_patches: HashMap<(String, Option<String>), usize> = HashMap::new();
        for (i, patch) in reconcile_patches.iter().enumerate() {
            let key = (format!("{:?}", patch.kind), patch.name.clone());
            if let Some(prev) = seen_patches.insert(key, i) {
                tracing::warn!(
                    kind = ?patch.kind,
                    name = %patch.name.as_deref().unwrap_or("(all)"),
                    prev_position = prev,
                    position = i,
                    "duplicate reconcile patch — last wins"
                );
            }
        }

        // Build per-module tasks for modules that have effective overrides
        if let Some(resolved) = resolved
            && let Some(reconcile_cfg) = daemon_cfg.reconcile.as_ref()
        {
            for mod_ref in &resolved.merged.modules {
                let mod_name = crate::modules::resolve_profile_module_name(mod_ref);
                let eff =
                    crate::resolve_effective_reconcile(mod_name, profile_chain, reconcile_cfg);

                // Only create a dedicated task if the effective settings differ from global
                if eff.interval != reconcile_cfg.interval
                    || eff.auto_apply != reconcile_cfg.auto_apply
                    || eff.drift_policy != reconcile_cfg.drift_policy
                {
                    tasks.push(ReconcileTask {
                        entity: mod_name.to_string(),
                        interval: parse_duration_or_default(&eff.interval),
                        auto_apply: eff.auto_apply,
                        drift_policy: eff.drift_policy,
                        last_reconciled: None,
                    });
                }
            }
        }
    }

    // Default task for everything not covered by module-specific tasks
    tasks.push(ReconcileTask {
        entity: "__default__".to_string(),
        interval: reconcile_interval,
        auto_apply,
        drift_policy: daemon_cfg.reconcile_effective().drift_policy.clone(),
        last_reconciled: None,
    });

    tasks
}

/// Build sync tasks for local config and each configured source.
///
/// Creates one task for the local config directory (always present), plus one task
/// per configured source whose cache directory exists on disk.
pub(crate) fn build_sync_tasks(
    config_dir: &Path,
    parsed: &ParsedDaemonConfig,
    sources: &[config::SourceSpec],
    allow_unsigned: bool,
    source_cache_dir: &Path,
    manifest_detector: impl Fn(&Path) -> Option<bool>,
) -> Vec<SyncTask> {
    let mut tasks: Vec<SyncTask> = vec![SyncTask {
        source_name: config::LOCAL_LAYER.to_string(),
        repo_path: config_dir.to_path_buf(),
        auto_pull: parsed.auto_pull,
        auto_push: parsed.auto_push,
        auto_apply: true,
        interval: parsed.sync_interval,
        last_synced: None,
        require_signed_commits: false,
        allow_unsigned,
    }];

    for source_spec in sources {
        let source_dir = source_cache_dir.join(&source_spec.name);
        if source_dir.exists() {
            // The subscriber's own flag ORed with the manifest's, through the
            // one derivation the load paths use. Read from the manifest alone,
            // this asks the cached clone whether its own contents should be
            // checked.
            let require_signed = source_spec
                .requires_signed_commits(manifest_detector(&source_dir).unwrap_or(false));
            tasks.push(SyncTask {
                source_name: source_spec.name.clone(),
                repo_path: source_dir,
                auto_pull: true,
                auto_push: false,
                auto_apply: source_spec.sync.auto_apply,
                interval: parse_duration_or_default(&source_spec.sync.interval),
                last_synced: None,
                require_signed_commits: require_signed,
                allow_unsigned,
            });
        }
    }

    tasks
}
