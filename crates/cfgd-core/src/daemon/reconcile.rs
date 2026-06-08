use super::*;
use crate::PathDisplayExt;
use crate::to_posix_string;

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
                tracing::warn!(path = %path.posix(), error = %e, "cannot watch path");
            }
        } else if let Some(parent) = path.parent() {
            // Watch parent directory so we detect file creation
            if parent.exists()
                && let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive)
            {
                tracing::warn!(path = %parent.posix(), error = %e, "cannot watch path");
            }
        }
    }

    // Watch config directory for source changes
    if config_dir.exists()
        && let Err(e) = watcher.watch(config_dir, RecursiveMode::Recursive)
    {
        tracing::warn!(path = %config_dir.posix(), error = %e, "cannot watch config dir");
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
    pub printer: &'a crate::output::Printer,
    /// When set, restrict reconcile to actions targeting this module name.
    /// Used by per-module reconcile ticks fired from `ReconcilePatch` entries;
    /// the plan is filtered to retain only `Action::Module` entries whose
    /// `module_name` matches, plus `auto_apply_override` and
    /// `drift_policy_override` take effect when present so the per-module patch
    /// fields (`autoApply`, `driftPolicy`) actually drive behavior.
    pub module_filter: Option<&'a str>,
    /// Override for `cfg.spec.daemon.reconcile.auto_apply`. Only consulted when
    /// `module_filter` is set; otherwise the global config wins.
    pub auto_apply_override: Option<bool>,
    /// Override for `cfg.spec.daemon.reconcile.drift_policy`. Only consulted
    /// when `module_filter` is set; otherwise the global config wins.
    pub drift_policy_override: Option<config::DriftPolicy>,
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
        module_filter,
        auto_apply_override,
        drift_policy_override,
    } = ctx;
    if let Some(name) = module_filter {
        tracing::info!(module = %name, "running per-module reconciliation check");
    } else {
        tracing::info!("running reconciliation check");
    }

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
        Some(d) => match StateStore::open_in_dir(d) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "reconcile: state store error");
                return;
            }
        },
        None => match StateStore::open_default() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "reconcile: state store error");
                return;
            }
        },
    };

    // Process auto-apply decisions for source items
    let auto_apply = auto_apply_override.unwrap_or_else(|| {
        cfg.spec
            .daemon
            .as_ref()
            .and_then(|d| d.reconcile.as_ref())
            .map(|r| r.auto_apply)
            .unwrap_or(false)
    });

    // Source-decision processing is profile-wide; skip it when we're scoped to
    // a single module so a per-module tick doesn't accidentally
    // accept/reject items from sources unrelated to the patched module.
    let pending_exclusions =
        if module_filter.is_none() && auto_apply && !cfg.spec.sources.is_empty() {
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
                        && let Err(e) =
                            store.resolve_decisions_for_source(&decision.source, "rejected")
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
    // The daemon is a full, unscoped reconcile, so it prunes: feed the real
    // cfgd-tracked set as `"<manager>/<identity>"` entries.
    let cfgd_installed: HashSet<String> = store
        .managed_package_ids()
        .unwrap_or_default()
        .into_iter()
        .map(|(mgr, pkg)| format!("{mgr}/{pkg}"))
        .collect();
    let pkg_actions =
        match hooks.plan_packages(&resolved.merged, &available_managers, &cfgd_installed) {
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
    let resolved_modules =
        super::resolve_daemon_modules(&registry, &resolved, &config_dir, printer);
    let resolved_modules_ref = resolved_modules.clone();
    let mut plan = match reconciler.plan(
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

    // Per-module reconcile: prune every action that is not a Module action
    // targeting the filter name. This keeps the apply call below focused on
    // just that one module's packages/files/scripts and avoids reaching into
    // unrelated profile state.
    if let Some(name) = module_filter {
        for phase in &mut plan.phases {
            phase.actions.retain(|a| match a {
                crate::reconciler::Action::Module(ma) => ma.module_name == name,
                _ => false,
            });
        }
    }

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

    // Update daemon state. For a per-module tick we only touch
    // `module_last_reconcile` so the profile-wide "last reconcile" timestamp
    // continues to reflect the default reconcile cadence.
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async {
        let mut st = state.lock().await;
        if let Some(name) = module_filter {
            st.module_last_reconcile.insert(name.to_string(), timestamp);
        } else {
            st.last_reconcile = Some(timestamp.clone());
            if let Some(source) = st.sources.first_mut() {
                source.last_reconcile = Some(timestamp);
            }
        }
    });

    if effective_total == 0 {
        tracing::debug!("reconcile: no drift detected");

        // This reconcile is the ground-truth snapshot: nothing drifts now, so
        // every outstanding drift row has healed. Clear them and reset the
        // in-memory count so `/status` and `/drift` both return to 0.
        if let Err(e) = store.resolve_all_drift() {
            tracing::warn!(error = %e, "failed to resolve outstanding drift on clean tick");
        }
        rt.block_on(async {
            let mut st = state.lock().await;
            st.drift_count = 0;
            if let Some(source) = st.sources.first_mut() {
                source.drift_count = 0;
            }
        });
    } else {
        tracing::info!(actions = effective_total, "reconcile: drift detected");

        // The plan's action set is the exact current drift set. Record each
        // diverging resource (UPSERT — no duplicate rows across ticks)...
        let mut current_drift: Vec<(String, String)> = Vec::new();
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
                current_drift.push((rtype, rid));
            }
        }
        // ...then resolve any still-unresolved rows NOT in the current set:
        // they healed since the last tick.
        if let Err(e) = store.resolve_drift_not_in(&current_drift) {
            tracing::warn!(error = %e, "failed to resolve healed drift rows");
        }

        // Execute onDrift scripts from resolved profile. Profile-level scripts
        // are skipped for per-module ticks — those fire only when a default
        // (whole-profile) reconcile detects drift.
        if module_filter.is_none() && !resolved.merged.scripts.on_drift.is_empty() {
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
            let working = crate::reconciler::script_default_workdir(&config_dir);
            for entry in &scripts.on_drift {
                match crate::reconciler::execute_script(
                    entry,
                    &config_dir,
                    &working,
                    &script_env,
                    default_timeout,
                    printer,
                    None,
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

        // Execute module-level onDrift scripts for each module that drifted.
        // Unlike profile onDrift, this fires on per-module ticks too: the plan is
        // already pruned to the filtered module above, so iterating it scopes
        // correctly in both the whole-profile and per-module cases.
        for module in &resolved_modules_ref {
            if module.on_drift_scripts.is_empty() || !module_has_drift(&plan, &module.name) {
                continue;
            }
            tracing::info!(
                module = %module.name,
                count = module.on_drift_scripts.len(),
                "running module onDrift script(s)"
            );
            let script_env = crate::reconciler::build_module_script_env(
                &config_dir,
                profile_name,
                crate::reconciler::ReconcileContext::Reconcile,
                &crate::reconciler::ScriptPhase::OnDrift,
                Some(&module.name),
                Some(&module.dir),
                &module.env,
            );
            let working = crate::reconciler::script_default_workdir(&config_dir);
            for entry in &module.on_drift_scripts {
                match crate::reconciler::execute_script(
                    entry,
                    &module.dir,
                    &working,
                    &script_env,
                    crate::reconciler::MODULE_SCRIPT_TIMEOUT,
                    printer,
                    None,
                ) {
                    Ok((desc, _, _)) => {
                        tracing::info!(module = %module.name, script = %desc, "module onDrift script completed");
                    }
                    Err(e) => {
                        tracing::error!(module = %module.name, error = %e, "module onDrift script failed");
                    }
                }
            }
        }

        // Set the in-memory count from the actual outstanding rows, not an
        // append-only accumulator, so `/status` tracks `/drift`. A read failure
        // leaves the prior count untouched rather than forcing a misleading 0.
        if let Some(outstanding) = super::drift::current_drift_count(&store) {
            rt.block_on(async {
                let mut st = state.lock().await;
                st.drift_count = outstanding;
                if let Some(source) = st.sources.first_mut() {
                    source.drift_count = outstanding;
                }
            });
        }

        // Check drift policy to decide whether to auto-apply or just notify.
        // Per-module ticks may override the global value via their patch entry.
        let drift_policy = drift_policy_override.clone().unwrap_or_else(|| {
            cfg.spec
                .daemon
                .as_ref()
                .and_then(|d| d.reconcile.as_ref())
                .map(|r| r.drift_policy.clone())
                .unwrap_or_default()
        });

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
                    None,
                    &crate::AbortFlag::new(),
                ) {
                    Ok(result) => {
                        let succeeded = result.succeeded();
                        let failed = result.failed();
                        tracing::info!(
                            succeeded = succeeded,
                            failed = failed,
                            "auto-apply complete"
                        );
                        // Self-heal the tracking table on a full (non-module)
                        // reconcile: drop rows whose package is gone (partial
                        // uninstall / out-of-band removal) so they can't leak.
                        if module_filter.is_none() {
                            match crate::reconciler::stale_tracked_packages(
                                &available_managers,
                                &cfgd_installed,
                            ) {
                                Ok(stale) => {
                                    for (mgr, id) in stale {
                                        let rid = format!("{mgr}/{id}");
                                        if let Err(e) =
                                            store.remove_managed_resource("package", &rid)
                                        {
                                            tracing::warn!(resource = %rid, error = %e, "failed to GC stale package tracking row");
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to compute stale package tracking rows")
                                }
                            }
                            // Prune packages whose custom/scripted manager block
                            // left the config: run the persisted uninstall script
                            // via the hook, then drop each row that was removed.
                            let known = registry.manager_names();
                            match store.orphaned_package_resources(&known) {
                                Ok(orphans) if !orphans.is_empty() => {
                                    for (mgr, pkg) in
                                        hooks.prune_orphaned_packages(&orphans, printer)
                                    {
                                        let rid = format!("{mgr}/{pkg}");
                                        if let Err(e) =
                                            store.remove_managed_resource("package", &rid)
                                        {
                                            tracing::warn!(resource = %rid, error = %e, "failed to GC orphaned package tracking row");
                                        }
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to compute orphaned package rows")
                                }
                            }
                        }
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

                        // `apply` resolves each applied resource's drift row, so
                        // the outstanding count now reflects the heal in this
                        // same tick: 0 on full success, the remainder on a
                        // partial failure (those rows stay recorded). A read
                        // failure leaves the prior count untouched.
                        if let Some(outstanding) = super::drift::current_drift_count(&store) {
                            rt.block_on(async {
                                let mut st = state.lock().await;
                                st.drift_count = outstanding;
                                if let Some(source) = st.sources.first_mut() {
                                    source.drift_count = outstanding;
                                }
                            });
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

    // Server check-in + pending-config consumption are profile-wide
    // operations; skip them for per-module ticks so a fast per-module cadence
    // doesn't hammer the gateway or race the default reconcile.
    if module_filter.is_some() {
        return;
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

/// Whether `plan` contains a non-Skip `Action::Module` targeting `module_name`.
///
/// Mirrors the profile-level "fire on detected drift" rule scoped to one
/// module's own actions: a `Skip` module action records no change, so it does
/// not count as drift.
pub(crate) fn module_has_drift(plan: &crate::reconciler::Plan, module_name: &str) -> bool {
    use crate::reconciler::{Action, ModuleActionKind};
    plan.phases.iter().flat_map(|p| &p.actions).any(|a| {
        matches!(
            a,
            Action::Module(ma)
                if ma.module_name == module_name
                    && !matches!(ma.kind, ModuleActionKind::Skip { .. })
        )
    })
}

pub(crate) fn action_resource_info(action: &crate::reconciler::Action) -> (String, String) {
    use crate::providers::{FileAction, PackageAction, SecretAction};
    use crate::reconciler::Action;

    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, .. } => ("file".to_string(), to_posix_string(target)),
            FileAction::Update { target, .. } => ("file".to_string(), to_posix_string(target)),
            FileAction::Delete { target, .. } => ("file".to_string(), to_posix_string(target)),
            FileAction::SetPermissions { target, .. } => {
                ("file".to_string(), to_posix_string(target))
            }
            FileAction::Skip { target, .. } => ("file".to_string(), to_posix_string(target)),
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
            SecretAction::Decrypt { target, .. } => ("secret".to_string(), to_posix_string(target)),
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
                EnvAction::WriteEnvFile { path, .. } => ("env".to_string(), to_posix_string(path)),
                EnvAction::InjectSourceLine { rc_path, .. } => {
                    ("env-rc".to_string(), to_posix_string(rc_path))
                }
                EnvAction::RefreshLiveSession { .. } => {
                    ("env-session".to_string(), "live-session".to_string())
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
        resources.insert(format!("files.{}", to_posix_string(&file.target)));
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
