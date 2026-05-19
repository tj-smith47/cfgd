use super::*;

// --- File Watcher ---

pub(crate) fn setup_file_watcher(
    tx: mpsc::Sender<PathBuf>,
    managed_paths: &[PathBuf],
    config_dir: &Path,
) -> Result<RecommendedWatcher> {
    let sender = tx.clone();
    let mut watcher =
        notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
                        for path in event.paths {
                            match sender.try_send(path) {
                                Ok(()) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    tracing::debug!("file watcher channel full — event coalesced");
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "file watcher event dropped");
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .map_err(|e| DaemonError::WatchError {
            message: format!("failed to create file watcher: {}", e),
        })?;

    // Watch managed files
    for path in managed_paths {
        if path.exists() {
            if let Err(e) = watcher.watch(path, RecursiveMode::NonRecursive) {
                tracing::warn!(path = %path.display(), error = %e, "cannot watch path");
            }
        } else if let Some(parent) = path.parent() {
            // Watch parent directory so we detect file creation
            if parent.exists()
                && let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive)
            {
                tracing::warn!(path = %parent.display(), error = %e, "cannot watch path");
            }
        }
    }

    // Watch config directory for source changes
    if config_dir.exists()
        && let Err(e) = watcher.watch(config_dir, RecursiveMode::Recursive)
    {
        tracing::warn!(path = %config_dir.display(), error = %e, "cannot watch config dir");
    }

    Ok(watcher)
}

pub(crate) fn discover_managed_paths(
    config_path: &Path,
    profile_override: Option<&str>,
    hooks: &dyn DaemonHooks,
) -> Vec<PathBuf> {
    let cfg = match config::load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "cannot load config for file discovery");
            return Vec::new();
        }
    };

    let profiles_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profiles");
    let profile_name = match profile_override.or(cfg.spec.profile.as_deref()) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let resolved = match config::resolve_profile(profile_name, &profiles_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "cannot resolve profile for file discovery");
            return Vec::new();
        }
    };

    resolved
        .merged
        .files
        .managed
        .iter()
        .map(|f| hooks.expand_tilde(&f.target))
        .collect()
}

// --- Reconciliation Handler ---

/// Collaborators threaded into every `handle_reconcile` call. Bundled to keep
/// the function-arity clippy lint quiet.
pub(crate) struct ReconcileCtx<'a> {
    pub state: &'a Arc<Mutex<DaemonState>>,
    pub notifier: &'a Arc<Notifier>,
    pub notify_on_drift: bool,
    pub hooks: &'a dyn DaemonHooks,
    pub state_dir_override: Option<&'a Path>,
    pub printer: &'a crate::output_v2::Printer,
}

pub(crate) fn handle_reconcile(
    config_path: &Path,
    profile_override: Option<&str>,
    ctx: ReconcileCtx<'_>,
) {
    let ReconcileCtx {
        state,
        notifier,
        notify_on_drift,
        hooks,
        state_dir_override,
        printer,
    } = ctx;
    tracing::info!("running reconciliation check");

    // Try to acquire the apply lock (non-blocking). If a CLI apply is in
    // progress, skip this reconciliation tick.
    let state_dir = match state_dir_override {
        Some(d) => d.to_path_buf(),
        None => match crate::state::default_state_dir() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "reconcile: cannot determine state directory");
                return;
            }
        },
    };
    let _lock = match crate::acquire_apply_lock(&state_dir) {
        Ok(guard) => guard,
        Err(crate::errors::CfgdError::State(crate::errors::StateError::ApplyLockHeld {
            ref holder,
        })) => {
            tracing::debug!(holder = %holder, "reconcile: skipping — apply lock held");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "reconcile: cannot acquire apply lock");
            return;
        }
    };

    let cfg = match config::load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: config load failed");
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
            tracing::error!("no profile configured — skipping reconciliation");
            return;
        }
    };

    let resolved = match config::resolve_profile(profile_name, &profiles_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: profile resolution failed");
            return;
        }
    };

    // Check for drift by generating a plan
    let mut registry = hooks.build_registry(&cfg);
    hooks.extend_registry_custom_managers(&mut registry, &resolved.merged.packages);
    let store = match state_dir_override {
        Some(d) => {
            std::fs::create_dir_all(d).ok();
            match StateStore::open(&d.join("cfgd.db")) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "reconcile: state store error");
                    return;
                }
            }
        }
        None => match StateStore::open_default() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "reconcile: state store error");
                return;
            }
        },
    };

    // Process auto-apply decisions for source items
    let auto_apply = cfg
        .spec
        .daemon
        .as_ref()
        .and_then(|d| d.reconcile.as_ref())
        .map(|r| r.auto_apply)
        .unwrap_or(false);

    let pending_exclusions = if auto_apply && !cfg.spec.sources.is_empty() {
        let default_policy = AutoApplyPolicyConfig::default();
        let policy = cfg
            .spec
            .daemon
            .as_ref()
            .and_then(|d| d.reconcile.as_ref())
            .and_then(|r| r.policy.as_ref())
            .unwrap_or(&default_policy);

        let mut all_excluded = HashSet::new();
        for source_spec in &cfg.spec.sources {
            let excluded = process_source_decisions(
                &store,
                &source_spec.name,
                &resolved.merged,
                policy,
                notifier,
            );
            all_excluded.extend(excluded);
        }

        // Auto-resolve pending decisions for removed sources
        let source_names: HashSet<&str> =
            cfg.spec.sources.iter().map(|s| s.name.as_str()).collect();
        if let Ok(all_pending) = store.pending_decisions() {
            for decision in &all_pending {
                if !source_names.contains(decision.source.as_str())
                    && let Err(e) = store.resolve_decisions_for_source(&decision.source, "rejected")
                {
                    tracing::warn!(
                        source = %decision.source,
                        error = %e,
                        "failed to auto-reject decisions for removed source"
                    );
                }
            }
        }

        all_excluded
    } else {
        HashSet::new()
    };

    let reconciler = crate::reconciler::Reconciler::new(&registry, &store);

    let available_managers = registry.available_package_managers();
    let pkg_actions = match hooks.plan_packages(&resolved.merged, &available_managers) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: package planning failed");
            return;
        }
    };

    let file_actions = match hooks.plan_files(&config_dir, &resolved) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: file planning failed");
            return;
        }
    };

    // Resolve modules from profile + lockfile
    let resolved_modules = if !resolved.merged.modules.is_empty() {
        let platform = crate::platform::Platform::detect();
        let mgr_map: std::collections::HashMap<String, &dyn PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| (m.name().to_string(), m.as_ref() as &dyn PackageManager))
            .collect();
        let cache_base = crate::modules::default_module_cache_dir()
            .unwrap_or_else(|_| config_dir.join(".module-cache"));
        match crate::modules::resolve_modules(
            &resolved.merged.modules,
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
            printer,
        ) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "reconcile: module resolution failed");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let resolved_modules_ref = resolved_modules.clone();
    let plan = match reconciler.plan(
        &resolved,
        file_actions,
        pkg_actions,
        resolved_modules,
        crate::reconciler::ReconcileContext::Reconcile,
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: plan generation failed");
            return;
        }
    };

    // Filter out pending decision items from the plan when auto-applying
    let effective_total = if pending_exclusions.is_empty() {
        plan.total_actions()
    } else {
        let mut count = 0usize;
        for phase in &plan.phases {
            for action in &phase.actions {
                let (_rtype, rid) = action_resource_info(action);
                if !pending_exclusions.contains(&rid) {
                    count += 1;
                }
            }
        }
        count
    };

    let timestamp = crate::utc_now_iso8601();

    // Update daemon state
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async {
        let mut st = state.lock().await;
        st.last_reconcile = Some(timestamp.clone());
        if let Some(source) = st.sources.first_mut() {
            source.last_reconcile = Some(timestamp);
        }
    });

    if effective_total == 0 {
        tracing::debug!("reconcile: no drift detected");
    } else {
        tracing::info!(actions = effective_total, "reconcile: drift detected");

        // Record drift events
        for phase in &plan.phases {
            for action in &phase.actions {
                let (rtype, rid) = action_resource_info(action);
                // Skip pending decision items when recording drift
                if pending_exclusions.contains(&rid) {
                    continue;
                }
                if let Err(e) =
                    store.record_drift(&rtype, &rid, None, Some("drift detected"), "local")
                {
                    tracing::warn!(error = %e, "failed to record drift");
                }
            }
        }

        // Execute onDrift scripts from resolved profile
        if !resolved.merged.scripts.on_drift.is_empty() {
            let scripts = &resolved.merged.scripts;
            tracing::info!(count = scripts.on_drift.len(), "running onDrift script(s)");
            let script_env = crate::reconciler::build_script_env(
                &config_dir,
                profile_name,
                crate::reconciler::ReconcileContext::Reconcile,
                &crate::reconciler::ScriptPhase::OnDrift,
                None,
                None,
            );
            let default_timeout = crate::PROFILE_SCRIPT_TIMEOUT;
            for entry in &scripts.on_drift {
                match crate::reconciler::execute_script(
                    entry,
                    &config_dir,
                    &script_env,
                    default_timeout,
                    printer,
                ) {
                    Ok((desc, _, _)) => {
                        tracing::info!(script = %desc, "onDrift script completed");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "onDrift script failed");
                    }
                }
            }
        }

        // Update drift count
        rt.block_on(async {
            let mut st = state.lock().await;
            st.drift_count += effective_total as u32;
            if let Some(source) = st.sources.first_mut() {
                source.drift_count += effective_total as u32;
            }
        });

        // Check drift policy to decide whether to auto-apply or just notify
        let drift_policy = cfg
            .spec
            .daemon
            .as_ref()
            .and_then(|d| d.reconcile.as_ref())
            .map(|r| r.drift_policy.clone())
            .unwrap_or_default();

        match drift_policy {
            config::DriftPolicy::Auto => {
                tracing::info!(
                    actions = effective_total,
                    "drift policy is Auto — applying actions"
                );
                match reconciler.apply(
                    &plan,
                    &resolved,
                    &config_dir,
                    printer,
                    None,
                    &resolved_modules_ref,
                    crate::reconciler::ReconcileContext::Reconcile,
                    false,
                ) {
                    Ok(result) => {
                        let succeeded = result.succeeded();
                        let failed = result.failed();
                        tracing::info!(
                            succeeded = succeeded,
                            failed = failed,
                            "auto-apply complete"
                        );
                        if failed > 0 && notify_on_drift {
                            notifier.notify(
                                "cfgd: auto-apply partial failure",
                                &format!(
                                    "{} action(s) succeeded, {} failed. Run `cfgd status` for details.",
                                    succeeded, failed
                                ),
                            );
                        } else if notify_on_drift {
                            notifier.notify(
                                "cfgd: auto-apply succeeded",
                                &format!("{} action(s) applied successfully.", succeeded),
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "auto-apply failed");
                        if notify_on_drift {
                            notifier.notify(
                                "cfgd: auto-apply failed",
                                &format!("Auto-apply failed: {}. Run `cfgd apply` manually.", e),
                            );
                        }
                    }
                }
            }
            config::DriftPolicy::NotifyOnly | config::DriftPolicy::Prompt => {
                tracing::info!("drift policy is NotifyOnly — recording drift, not applying");
                if notify_on_drift {
                    notifier.notify(
                        "cfgd: drift detected",
                        &format!(
                            "{} resource(s) have drifted from desired state. Run `cfgd apply` to reconcile.",
                            effective_total
                        ),
                    );
                }
            }
        }
    }

    // Server check-in after reconciliation
    let changed = try_server_checkin(&cfg, &resolved);
    if changed {
        tracing::info!(
            "reconcile: server reports config has changed — will reconcile on next tick"
        );
    }

    // Consume any pending server-pushed config (saved by CLI checkin or enrollment)
    match crate::state::load_pending_server_config() {
        Ok(Some(pending)) => {
            let keys: Vec<String> = pending
                .as_object()
                .map(|obj| obj.keys().cloned().collect())
                .unwrap_or_default();
            tracing::info!(
                keys = ?keys,
                "consumed pending server config — next reconcile will pick up changes"
            );
            if let Err(e) = crate::state::clear_pending_server_config() {
                tracing::warn!(error = %e, "failed to clear pending server config");
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, "failed to load pending server config");
        }
    }
}

pub(crate) fn action_resource_info(action: &crate::reconciler::Action) -> (String, String) {
    use crate::providers::{FileAction, PackageAction, SecretAction};
    use crate::reconciler::Action;

    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, .. } => ("file".to_string(), target.display().to_string()),
            FileAction::Update { target, .. } => ("file".to_string(), target.display().to_string()),
            FileAction::Delete { target, .. } => ("file".to_string(), target.display().to_string()),
            FileAction::SetPermissions { target, .. } => {
                ("file".to_string(), target.display().to_string())
            }
            FileAction::Skip { target, .. } => ("file".to_string(), target.display().to_string()),
        },
        Action::Package(pa) => match pa {
            PackageAction::Bootstrap { manager, .. } => {
                ("package".to_string(), format!("{}:bootstrap", manager))
            }
            PackageAction::Install {
                manager, packages, ..
            } => (
                "package".to_string(),
                format!("{}:{}", manager, packages.join(",")),
            ),
            PackageAction::Uninstall {
                manager, packages, ..
            } => (
                "package".to_string(),
                format!("{}:{}", manager, packages.join(",")),
            ),
            PackageAction::Skip { manager, .. } => ("package".to_string(), manager.clone()),
        },
        Action::Secret(sa) => match sa {
            SecretAction::Decrypt { target, .. } => {
                ("secret".to_string(), target.display().to_string())
            }
            SecretAction::Resolve { reference, .. } => ("secret".to_string(), reference.clone()),
            SecretAction::ResolveEnv { envs, .. } => {
                ("secret".to_string(), format!("env:[{}]", envs.join(",")))
            }
            SecretAction::Skip { source, .. } => ("secret".to_string(), source.clone()),
        },
        Action::System(sa) => {
            use crate::reconciler::SystemAction;
            match sa {
                SystemAction::SetValue {
                    configurator, key, ..
                } => ("system".to_string(), format!("{}:{}", configurator, key)),
                SystemAction::Skip { configurator, .. } => {
                    ("system".to_string(), configurator.clone())
                }
            }
        }
        Action::Script(sa) => {
            use crate::reconciler::ScriptAction;
            match sa {
                ScriptAction::Run { entry, .. } => {
                    ("script".to_string(), entry.run_str().to_string())
                }
            }
        }
        Action::Module(ma) => ("module".to_string(), ma.module_name.clone()),
        Action::Env(ea) => {
            use crate::reconciler::EnvAction;
            match ea {
                EnvAction::WriteEnvFile { path, .. } => {
                    ("env".to_string(), path.display().to_string())
                }
                EnvAction::InjectSourceLine { rc_path, .. } => {
                    ("env-rc".to_string(), rc_path.display().to_string())
                }
            }
        }
    }
}

// --- Auto-apply decision handling ---

/// Extract resource identifiers from a merged profile for change detection.
/// Returns a set of dot-notation resource paths (e.g. "packages.brew.ripgrep").
pub(crate) fn extract_source_resources(merged: &MergedProfile) -> HashSet<String> {
    let mut resources = HashSet::new();

    let pkgs = &merged.packages;
    if let Some(ref brew) = pkgs.brew {
        for f in &brew.formulae {
            resources.insert(format!("packages.brew.{}", f));
        }
        for c in &brew.casks {
            resources.insert(format!("packages.brew.{}", c));
        }
    }
    if let Some(ref apt) = pkgs.apt {
        for p in &apt.packages {
            resources.insert(format!("packages.apt.{}", p));
        }
    }
    if let Some(ref cargo) = pkgs.cargo {
        for p in &cargo.packages {
            resources.insert(format!("packages.cargo.{}", p));
        }
    }
    for p in &pkgs.pipx {
        resources.insert(format!("packages.pipx.{}", p));
    }
    for p in &pkgs.dnf {
        resources.insert(format!("packages.dnf.{}", p));
    }
    if let Some(ref npm) = pkgs.npm {
        for p in &npm.global {
            resources.insert(format!("packages.npm.{}", p));
        }
    }

    for file in &merged.files.managed {
        resources.insert(format!("files.{}", file.target.display()));
    }

    for ev in &merged.env {
        resources.insert(format!("env.{}", ev.name));
    }

    for k in merged.system.keys() {
        resources.insert(format!("system.{}", k));
    }

    resources
}

/// Compute a hash of the resource set for change detection.
pub(crate) fn hash_resources(resources: &HashSet<String>) -> String {
    let mut sorted: Vec<&String> = resources.iter().collect();
    sorted.sort();
    let combined: String = sorted.iter().map(|r| format!("{}\n", r)).collect();
    crate::sha256_hex(combined.as_bytes())
}

/// Process auto-apply decisions for source items. Returns the set of resource paths
/// that should be excluded from the plan (pending decisions).
pub(crate) fn process_source_decisions(
    store: &StateStore,
    source_name: &str,
    merged: &MergedProfile,
    policy: &AutoApplyPolicyConfig,
    notifier: &Notifier,
) -> HashSet<String> {
    let current_resources = extract_source_resources(merged);
    let current_hash = hash_resources(&current_resources);

    // Check if the source config has changed since last merge
    let previous_hash = store
        .source_config_hash(source_name)
        .ok()
        .flatten()
        .map(|h| h.config_hash);

    if previous_hash.as_deref() == Some(&current_hash) {
        // No change — check for existing pending decisions to exclude
        return pending_resource_paths(store);
    }

    // Config changed — detect new items
    let previous_resources: HashSet<String> = if previous_hash.is_some() {
        // We don't store the old resource set, only the hash. So we use the
        // pending decisions + managed resources as a proxy for "known items".
        let mut known = HashSet::new();
        if let Ok(managed) = store.managed_resources_by_source(source_name) {
            for r in &managed {
                known.insert(format!("{}.{}", r.resource_type, r.resource_id));
            }
        }
        // Also include previously pending (resolved) decisions
        if let Ok(decisions) = store.pending_decisions_for_source(source_name) {
            for d in &decisions {
                known.insert(d.resource.clone());
            }
        }
        known
    } else {
        // First time seeing this source — all items are "new"
        HashSet::new()
    };

    let new_items: Vec<&String> = current_resources
        .iter()
        .filter(|r| !previous_resources.contains(*r))
        .collect();

    let mut new_pending_count = 0u32;

    for resource in &new_items {
        // Determine the tier: check if it's in recommended, optional, or locked
        // For simplicity, infer tier from the policy action mapping:
        // - Items that already exist in config are "update" (locked-conflict)
        // - New items default to "recommended" tier
        let tier = infer_item_tier(resource);
        let policy_action = match tier {
            "recommended" => &policy.new_recommended,
            "optional" => &policy.new_optional,
            "locked" => &policy.locked_conflict,
            _ => &policy.new_recommended,
        };

        match policy_action {
            PolicyAction::Accept => {
                // Include in plan normally — no action needed
            }
            PolicyAction::Reject | PolicyAction::Ignore => {
                // Skip silently — no record, no notification
            }
            PolicyAction::Notify => {
                let summary = format!("{} {} (from {})", tier, resource, source_name);
                if let Err(e) =
                    store.upsert_pending_decision(source_name, resource, tier, "install", &summary)
                {
                    tracing::warn!(error = %e, "failed to record pending decision");
                } else {
                    new_pending_count += 1;
                }
            }
        }
    }

    // Notify about new pending decisions (once per batch, not per item)
    if new_pending_count > 0 {
        notifier.notify(
            "cfgd: pending decisions",
            &format!(
                "Source \"{}\" has {} new {} item{} pending your review.\n\
                 Run `cfgd status` to see details, `cfgd decide accept --source {}` to accept all.",
                source_name,
                new_pending_count,
                if new_pending_count == 1 {
                    "recommended"
                } else {
                    "recommended/optional"
                },
                if new_pending_count == 1 { "" } else { "s" },
                source_name,
            ),
        );
    }

    // Update the stored hash
    if let Err(e) = store.set_source_config_hash(source_name, &current_hash) {
        tracing::warn!(error = %e, "failed to store source config hash");
    }

    // Return resources that are pending and should be excluded from the plan
    pending_resource_paths(store)
}

/// Get all pending (unresolved) decision resource paths as a set.
pub(crate) fn pending_resource_paths(store: &StateStore) -> HashSet<String> {
    store
        .pending_decisions()
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.resource)
        .collect()
}

/// Infer the policy tier for a resource based on naming conventions.
/// In a full implementation this would check the source manifest's policy tiers.
/// For daemon auto-apply, we use a heuristic: resources from sources are
/// "recommended" by default.
pub(crate) fn infer_item_tier(resource: &str) -> &'static str {
    // Files with "security" or "policy" in the path tend to be locked/required
    if resource.contains("security") || resource.contains("policy") || resource.contains("locked") {
        "locked"
    } else {
        "recommended"
    }
}
