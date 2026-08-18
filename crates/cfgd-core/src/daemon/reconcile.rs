use super::*;
use crate::PathDisplayExt;
use crate::reconciler::{DecisionExclusions, action_resource_info, withhold_from_plan};

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
    /// Whether the operator explicitly passed `--state-dir` — distinct from
    /// `state_dir_override`, which the production loop always materializes
    /// from the scope default. Store ownership is judged on this bit, exactly
    /// as the CLI judges `cli.state_dir.is_some()`.
    pub explicit_state_dir: bool,
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
    /// Deployment scope that selects FHS vs XDG directory roots for module/source
    /// cache directories.
    pub scope: crate::Scope,
    /// Raised when the daemon is shutting down. Threaded into `reconciler::apply`
    /// so a `SIGTERM` arriving mid-auto-apply stops the pre/post scripts instead
    /// of waiting out `PROFILE_SCRIPT_TIMEOUT`.
    pub abort: &'a crate::AbortFlag,
    /// What this daemon has already derived from its config files, so a tick
    /// whose config has not moved re-parses nothing. See `tick_cache.rs`.
    pub cache: &'a super::tick_cache::TickCache,
}

/// Why a tick has nothing to reconcile.
///
/// Carries no reason on purpose: every arm that raises it has already said what
/// happened at the point it recognised it (a `tracing::error!`, and for a broken
/// source cache a notification too), and a derivation that fails must not be
/// cached, so the only thing the caller needs from it is "return".
struct DerivationSkipped;

impl From<crate::errors::CfgdError> for DerivationSkipped {
    fn from(_: crate::errors::CfgdError) -> Self {
        Self
    }
}

/// The daemon's binding of the run skeleton to `Reconciler::apply`.
///
/// A tick never prompts and never scopes itself with `--phase`/`--skip`, so the
/// executor carries only what `apply` cannot derive from the plan: the profile
/// being reconciled, the directory its scripts run in, the modules whose state
/// it records, and the shutdown flag a `SIGTERM` raises mid-apply.
struct TickExecutor<'a> {
    reconciler: &'a crate::reconciler::Reconciler<'a>,
    resolved: &'a crate::config::ResolvedProfile,
    config_dir: &'a Path,
    modules: &'a [crate::modules::ResolvedModule],
    abort: &'a crate::AbortFlag,
}

impl crate::reconciler::RunExecutor for TickExecutor<'_> {
    fn apply(
        &mut self,
        plan: &crate::reconciler::Plan,
        printer: &crate::output::Printer,
    ) -> crate::errors::Result<crate::reconciler::ApplyResult> {
        self.reconciler.apply(
            plan,
            self.resolved,
            self.config_dir,
            printer,
            None,
            self.modules,
            crate::reconciler::ReconcileContext::Reconcile,
            false,
            None,
            self.abort,
        )
    }
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
        explicit_state_dir,
        printer,
        module_filter,
        auto_apply_override,
        drift_policy_override,
        scope,
        abort,
        cache,
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
    // apply mutex lives in the state dir; the CLI's apply_lock_dir() must
    // resolve to the same place (override-else-default_state_dir) or concurrent
    // CLI+daemon applies won't mutually-exclude.
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

    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    // Everything derived from the config FILES, reused for as long as none of
    // the files the last derivation read has moved. Nothing in here observes the
    // machine — the plan below still does that on every tick — so a hit cannot
    // hide drift; it only skips re-parsing a config that is byte-for-byte the
    // one this daemon already parsed. See `tick_cache.rs`.
    let derived = cache.config_derivation(config_path, profile_override, || {
        let cfg = config::load_config(config_path).inspect_err(|e| {
            tracing::error!(error = %e, "reconcile: config load failed");
        })?;

        let profiles_dir = config_dir.join("profiles");
        let profile_name = match profile_override.or(cfg.spec.profile.as_deref()) {
            Some(p) => p.to_string(),
            None => {
                tracing::error!("no profile configured — skipping reconciliation");
                return Err(DerivationSkipped);
            }
        };

        let local_resolved = config::resolve_profile(&profile_name, &profiles_dir).inspect_err(
            |e| {
                tracing::error!(error = %e, "reconcile: profile resolution failed");
            },
        )?;

        // Compose with sources CACHE-ONLY so reconcile sees the same source-composed
        // desired state every other command does, without touching the network in the
        // tight tick (the sync task owns fetch cadence). `resolved` is the effective
        // (local ⊕ sources) profile used for package/file/module planning;
        // `local_resolved` is the local-config input it composes over.
        //
        // FAIL-CLOSED: on a real compose error (malformed/constraint-violating cached
        // manifest, failed signature) we must SKIP this tick — never reconcile against
        // a substituted local-only desired state, because this is a pruning reconcile
        // and a dropped source-delivered package/module would be UNINSTALLED under
        // autoApply. Mirror the `resolve_profile` failure above: error + alert +
        // early-return, leaving the prior desired state (and last_reconcile) intact.
        // A benign never-synced cache-miss is warn+skip inside the resolver, not an
        // Err, so cache-miss still reconciles local-only.
        let (resolved, source_module_roots) =
            match super::compose_daemon_desired_state(&cfg, &local_resolved, printer, scope) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "reconcile: source composition failed — SKIPPING tick to avoid pruning source-delivered state against a degraded desired set"
                    );
                    notifier.notify(
                        "cfgd: reconcile skipped — source composition failed",
                        &format!(
                            "A configured source's cached config is broken ({e}). Reconcile was skipped to avoid uninstalling source-delivered packages. Run `cfgd sync` then `cfgd status` to inspect."
                        ),
                    );
                    return Err(DerivationSkipped);
                }
            };

        let mut registry = hooks.build_registry(&cfg);
        hooks.extend_registry_custom_managers(&mut registry, &resolved.merged.packages);

        Ok(super::tick_cache::DerivedConfig {
            cfg,
            profile_name,
            resolved,
            source_module_roots,
            registry,
        })
    });
    let Ok(derived) = derived else {
        return;
    };
    let cfg = &*derived.cfg;
    let profile_name = derived.profile_name.as_str();
    let resolved = &*derived.resolved;
    let source_module_roots = &*derived.source_module_roots;
    let registry = &*derived.registry;

    // Opened on the first tick that wants one and held for the daemon's life: a
    // connection describes the database rather than a snapshot of it, so nothing
    // about it can go stale between ticks.
    let held_store = match cache.store(|| match state_dir_override {
        Some(d) => StateStore::open_in_dir(d),
        // Reachable only when startup's scope-default materialization failed
        // (`run_daemon_with` fills `state_dir_override` on every healthy
        // tick); re-derive from the SAME scope rather than the user default,
        // or a system-scope daemon would fall back to the per-user store.
        None => StateStore::open_default_for(scope),
    }) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: state store error");
            return;
        }
    };
    let Some(store) = held_store.get() else {
        tracing::error!("reconcile: state store unavailable");
        return;
    };

    // Process auto-apply decisions for source items
    let auto_apply =
        auto_apply_override.unwrap_or_else(|| crate::reconciler::configured_auto_apply(cfg));

    // Discard the decisions of a source the subscriber has dropped: source
    // gone, items gone. Outside every OTHER gate below, because the rows a
    // removed source leaves are exactly the rows nobody can answer —
    // `cfgd decide` acts against a source that no longer exists — and dropping
    // the LAST source, or turning auto-apply off, must not be what strands
    // them. The one gate it does take is store ownership, shared with
    // `cfgd apply`: a daemon started on a FOREIGN config against the DEFAULT
    // store would otherwise delete another config's rows. Ownership is judged
    // on the resolved config path itself, so an installed service unit baking
    // `--config <default path>` still sweeps its own machine's rows.
    // Ownership is judged on the OPERATOR's `--state-dir`, never on the
    // materialized `state_dir_override` — the production loop always fills
    // that in from the scope default, so `.is_some()` on it would make every
    // deployed daemon an owner of whatever store the default resolved and a
    // `cfgd daemon --config /foreign.yaml` would sweep and mint the default
    // store's rows.
    let owns_the_store =
        crate::reconciler::owns_decision_store(config_path, explicit_state_dir, scope);
    let subscribed: Vec<String> = cfg.spec.sources.iter().map(|s| s.name.clone()).collect();
    if owns_the_store && let Err(e) = store.discard_decisions_not_in(&subscribed) {
        tracing::warn!(error = %e, "failed to discard decisions of removed sources");
    }

    let available_managers = registry.available_package_managers();
    // The daemon is a full, unscoped reconcile, so it prunes: feed the real
    // cfgd-tracked set as `"<manager>/<identity>"` entries.
    let cfgd_installed: HashSet<String> = store
        .managed_package_ids()
        .unwrap_or_default()
        .into_iter()
        .map(|(mgr, pkg)| format!("{mgr}/{pkg}"))
        .collect();
    // The enumerations are LENT by the daemon rather than owned by the tick:
    // asking every manager what it has installed is the tick's most expensive
    // observation, and it is already double-bounded by the resolution
    // generation and a 30s ceiling, so a tick faster than that reads the
    // previous tick's answer and a slower one re-asks exactly as before.
    let pkg_cx = crate::providers::PackageContext::with_shared_enumerations(printer, store, {
        cache.enumerations()
    });
    // Planned BEFORE the policy review because the classification consumes the
    // planner's own installed-state observation — one enumeration, threaded,
    // never a second shell-out. A planning failure therefore skips the tick
    // before anything is minted: a review taken without the observation would
    // ask about items the machine may already run.
    let (pkg_actions, actual_packages) = match hooks.plan_packages_observed(
        &resolved.merged,
        &available_managers,
        &cfgd_installed,
        &pkg_cx,
    ) {
        Ok(out) => out,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: package planning failed");
            return;
        }
    };

    // The policy review is profile-wide; skip it when we're scoped to a single
    // module so a per-module tick doesn't accidentally accept/reject items from
    // sources unrelated to the patched module. The classification itself is the
    // shared one `cfgd plan` and `cfgd apply` read, so a `Reject`-tier item the
    // daemon declines cannot be installed by a manual apply; only the WRITES
    // below (the rows, the hashes) belong to the daemon.
    let review = if module_filter.is_none() {
        match crate::reconciler::review_source_policies(
            store,
            cfg,
            resolved,
            auto_apply,
            &actual_packages,
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "reconcile: cannot review source auto-apply policy");
                notifier.notify(
                    "cfgd: reconcile skipped — source decisions unreadable",
                    &format!(
                        "cfgd could not read the source decision state ({e}), so this reconcile was skipped rather than applying items that may be awaiting your review. Run `cfgd status` to inspect."
                    ),
                );
                return;
            }
        }
    } else {
        crate::reconciler::SourcePolicyReview::default()
    };
    // The WRITE half takes the same gate as the sweep, mirroring `cfgd
    // apply`: a foreign config does not record rows and hashes into someone
    // else's store. The items are withheld from this tick either way — the
    // unrecorded mints ride `with_unrecorded` below.
    if owns_the_store {
        mint_reviewed_decisions(store, &review, notifier);
    }

    // The rows are read whatever the mode: minting a decision is an auto-apply
    // behaviour, but honouring one that already exists is not. A tick that read
    // them only under auto-apply would report drift for a resource `cfgd plan`
    // hides, on the same machine and the same rows.
    let decision_scope = crate::reconciler::DecisionScope::new(
        subscribed,
        &crate::reconciler::local_profile(resolved),
    );
    // `with_unrecorded` closes the write-failure window: a mint the store
    // rejected has no row for `read` to return, and the unattended tick is the
    // one path that would install it with nobody watching. For every row that
    // DID record, it is a no-op — `read` already returned it.
    let withheld = match crate::reconciler::WithheldDecisions::read(store, &decision_scope) {
        Ok(w) => w
            .with_policy_declined(review.declined)
            .with_unrecorded(&review.to_mint, &decision_scope)
            .with_undecidable(review.undecidable)
            .with_auto_accepted(&review.auto_accepted),
        Err(e) => {
            tracing::error!(error = %e, "reconcile: cannot read source decisions");
            notifier.notify(
                "cfgd: reconcile skipped — source decisions unreadable",
                &format!(
                    "cfgd could not read the source decision state ({e}), so this reconcile was skipped rather than applying items that may be awaiting your review. Run `cfgd status` to inspect."
                ),
            );
            return;
        }
    };
    let pending_exclusions =
        DecisionExclusions::from_withheld_with(&withheld, |p| hooks.expand_tilde(p));

    // The env arm withholds the surface as a unit, and apply rebuilds that
    // surface after the phases run from the declared set rather than from the
    // plan — so the pruning below is only half the guarantee without this.
    let reconciler = crate::reconciler::Reconciler::new(registry, store)
        .withholding_env_surface(pending_exclusions.withholds_env_surface());

    let file_actions = match hooks.plan_files(&config_dir, resolved) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: file planning failed");
            return;
        }
    };

    // Resolve modules from profile + lockfile + source-delivered roots, reusing
    // the last resolution while its own inputs and its config derivation stand
    // and its TTL has not run out (see `tick_cache.rs` for why this slot takes a
    // ceiling the config slot does not).
    let resolved_modules_ref = cache.modules(&derived, || {
        let mut resolved_modules = super::resolve_daemon_modules(
            registry,
            resolved,
            &config_dir,
            source_module_roots,
            printer,
            scope,
        );
        // The tick plans and (under auto-apply) applies, so its action descriptions
        // and recorded packages hash carry the version the read paths never ask for.
        // The compliance tick shares `resolve_daemon_modules` and deliberately does
        // NOT fill: nothing it stores renders a version.
        crate::modules::fill_module_available_versions(
            &mut resolved_modules,
            &registry.manager_map(),
        );
        resolved_modules
    });
    let mut plan = match reconciler.plan(
        resolved,
        file_actions,
        pkg_actions,
        (*resolved_modules_ref).clone(),
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
        narrow_to_module(&mut plan, name);
    }

    // A resource whose source change is still awaiting a decision is not the
    // daemon's to touch. Prune it out of the plan itself rather than only
    // discounting it: the tick's action count, the drift rows it records and
    // the actions an auto-apply executes then describe one set, and the header
    // cannot name a number the run disagrees with.
    withhold_from_plan(&mut plan, &pending_exclusions);
    let effective_total = plan.total_actions();

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
            for action in phase.actions() {
                let (rtype, rid) = action_resource_info(action);
                if let Err(e) = store.record_drift(
                    &rtype,
                    &rid,
                    None,
                    Some("drift detected"),
                    config::LOCAL_LAYER,
                ) {
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

        // The onDrift hooks of the profile and of every drifted module, as one
        // `Drift Hooks` tree above the reconcile header.
        //
        // Profile-level scripts are skipped for per-module ticks — those fire
        // only when a default (whole-profile) reconcile detects drift. Module
        // scripts fire on per-module ticks too: the plan is already pruned to
        // the filtered module above, so `module_has_drift` scopes correctly in
        // both cases.
        let profile_hooks: &[crate::config::ScriptEntry] = if module_filter.is_none() {
            &resolved.merged.scripts.on_drift
        } else {
            &[]
        };
        let drifted_modules: Vec<&crate::modules::ResolvedModule> = resolved_modules_ref
            .iter()
            .filter(|module| {
                !module.on_drift_scripts.is_empty() && module_has_drift(&plan, &module.name)
            })
            .collect();
        // The column is derived from config before the first script runs: the
        // pseudo-phase streams, so there is no close to buffer against, and
        // `execute_script` composes exactly this subject.
        let hook_labels: Vec<String> = profile_hooks
            .iter()
            .chain(
                drifted_modules
                    .iter()
                    .flat_map(|module| module.on_drift_scripts.iter()),
            )
            .map(|entry| {
                crate::reconciler::hook_script_subject(
                    crate::reconciler::ScriptPhase::OnDrift.display_name(),
                    entry.run_str(),
                )
                .to_string()
            })
            .collect();

        if !hook_labels.is_empty() {
            let hook_width =
                crate::reconciler::align_width_of(hook_labels.iter().map(String::as_str));
            // Opened before the loops, not assembled after them: each script
            // emits its own status as it finishes, and it has to land under its
            // owner group at that moment.
            let hooks_phase =
                crate::reconciler::pseudo_phase(printer, crate::reconciler::HOOKS_PHASE_LABEL);
            let drift_script_path_dirs = crate::reconciler::all_recorded_path_dirs(store);

            if !profile_hooks.is_empty() {
                tracing::info!(count = profile_hooks.len(), "running onDrift scripts");
                let owner = crate::reconciler::Owner::profile(profile_name);
                let _group = hooks_phase.owner(&owner, hook_width);
                let script_env =
                    crate::reconciler::build_script_env(&crate::reconciler::ScriptEnvContext {
                        config_dir: &config_dir,
                        profile_name,
                        context: crate::reconciler::ReconcileContext::Reconcile,
                        phase: &crate::reconciler::ScriptPhase::OnDrift,
                        module_name: None,
                        module_dir: None,
                        path_dirs: &drift_script_path_dirs,
                    });
                let working = crate::reconciler::script_default_workdir(&config_dir);
                for entry in profile_hooks {
                    match crate::reconciler::execute_script(
                        entry,
                        &config_dir,
                        &working,
                        &script_env,
                        crate::PROFILE_SCRIPT_TIMEOUT,
                        printer,
                        None,
                        None,
                        crate::reconciler::ScriptReport {
                            subject: crate::reconciler::ScriptSubject::Hook(
                                crate::reconciler::ScriptPhase::OnDrift.display_name(),
                            ),
                            non_fatal: true,
                            ..Default::default()
                        },
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

            for module in &drifted_modules {
                tracing::info!(
                    module = %module.name,
                    count = module.on_drift_scripts.len(),
                    "running module onDrift scripts"
                );
                let owner = crate::reconciler::Owner::module(&module.name);
                let _group = hooks_phase.owner(&owner, hook_width);
                let script_env = crate::reconciler::build_module_script_env(
                    &crate::reconciler::ScriptEnvContext {
                        config_dir: &config_dir,
                        profile_name,
                        context: crate::reconciler::ReconcileContext::Reconcile,
                        phase: &crate::reconciler::ScriptPhase::OnDrift,
                        module_name: Some(&module.name),
                        module_dir: Some(&module.dir),
                        path_dirs: &drift_script_path_dirs,
                    },
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
                        None,
                        crate::reconciler::ScriptReport {
                            subject: crate::reconciler::ScriptSubject::Hook(
                                crate::reconciler::ScriptPhase::OnDrift.display_name(),
                            ),
                            non_fatal: true,
                            ..Default::default()
                        },
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
        }

        // Set the in-memory count from the actual outstanding rows, not an
        // append-only accumulator, so `/status` tracks `/drift`. A read failure
        // leaves the prior count untouched rather than forcing a misleading 0.
        if let Some(outstanding) = super::drift::current_drift_count(store) {
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

        // The rows every arm below prints above its own body: a tick reports
        // the same run skeleton `cfgd apply` does, so the two surfaces cannot
        // describe one machine differently. Built once, before the policy
        // branch, because an applying tick and a notify-only tick differ in
        // what they do — never in what they are reconciling.
        let trigger = format!("drift ({effective_total} resources)");
        let module_names: Vec<String> = resolved_modules_ref
            .iter()
            .map(|module| module.name.clone())
            .collect();
        let run_ctx = || crate::reconciler::RunContext {
            title: crate::reconciler::RunTitle::Reconcile,
            config_path: Some(config_path),
            profile: Some(profile_name),
            modules: &module_names,
            trigger: Some(&trigger),
        };

        match drift_policy {
            config::DriftPolicy::Auto => {
                tracing::info!(
                    actions = effective_total,
                    "drift policy is Auto — applying actions"
                );
                let run = crate::reconciler::ApplyRun::new(run_ctx(), &plan);
                let mut exec = TickExecutor {
                    reconciler: &reconciler,
                    resolved,
                    config_dir: &config_dir,
                    modules: &resolved_modules_ref,
                    abort,
                };
                match run
                    .execute(printer, crate::reconciler::Confirm::Skip, &mut exec)
                    .map(|disposition| match disposition {
                        crate::reconciler::RunDisposition::Applied { result, .. } => Some(result),
                        // A run carrying a plan, executing (not `preview_only`)
                        // and never prompting has no other disposition.
                        crate::reconciler::RunDisposition::NothingToDo
                        | crate::reconciler::RunDisposition::Previewed
                        | crate::reconciler::RunDisposition::Declined
                        | crate::reconciler::RunDisposition::BackupsApplied { .. } => None,
                    }) {
                    Ok(None) => {}
                    Ok(Some(result)) => {
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
                                &pkg_cx,
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
                                        hooks.prune_orphaned_packages(&orphans, &pkg_cx)
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
                                    "{} succeeded, {} failed. Run `cfgd status` for details.",
                                    crate::pluralize(succeeded, "action"),
                                    failed
                                ),
                            );
                        } else if notify_on_drift {
                            notifier.notify(
                                "cfgd: auto-apply succeeded",
                                &format!(
                                    "{} applied successfully.",
                                    crate::pluralize(succeeded, "action")
                                ),
                            );
                        }

                        // `apply` resolves each applied resource's drift row, so
                        // the outstanding count now reflects the heal in this
                        // same tick: 0 on full success, the remainder on a
                        // partial failure (those rows stay recorded). A read
                        // failure leaves the prior count untouched.
                        if let Some(outstanding) = super::drift::current_drift_count(store) {
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
                // A tick that detected drift and chose not to act still has to
                // show WHAT drifted, so it renders the preview tree — never an
                // execution tree — and closes on a verdict instead of a rollup.
                let run = crate::reconciler::ApplyRun::new(run_ctx(), &plan).preview_only();
                run.header(printer);
                run.preview(printer);
                printer.status_simple(
                    crate::output::Role::Warn,
                    format!(
                        "Drift detected — {}; policy is notify-only, nothing applied",
                        crate::pluralize(effective_total, "action")
                    ),
                );
                if notify_on_drift {
                    notifier.notify(
                        "cfgd: drift detected",
                        &format!(
                            "{} drifted from desired state. Run `cfgd apply` to reconcile.",
                            crate::pluralize(effective_total, "resource")
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
    let changed = try_server_checkin(cfg, resolved);
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

/// Narrow a planned tick down to one module's own work.
///
/// Keeps that module's groups, and the manager nodes whose consumers are among
/// the actions that survived — nothing else. A module's packages install
/// through managers no module group contains, so dropping `cfgd:managers`
/// wholesale would install them against an index this tick never refreshed:
/// the only refresh cfgd runs outside the phase covers a manager it bootstraps
/// mid-run, which a manager already present never reaches. Narrowing by
/// consumer rather than keeping the group whole is the same rule the planner
/// mints by, so a tick for a module with no packages still plans nothing.
pub(super) fn narrow_to_module(plan: &mut crate::reconciler::Plan, module: &str) {
    for phase in &mut plan.phases {
        phase.retain_groups(|owner| {
            (owner.kind == crate::reconciler::OwnerKind::Module && owner.name == module)
                || owner.is_managers()
        });
    }
    // Every group owned by anything else, and every module group for a
    // different module, is dropped by the retain above — drop the emptied
    // phases too so drift recording and `reconciler.apply` only ever see the
    // filtered module's own work.
    plan.phases.retain(|p| !p.is_empty());
    crate::reconciler::prune_to_surviving_consumers(plan);
}

/// Whether `plan` contains a non-Skip `Action::Module` targeting `module_name`.
///
/// Mirrors the profile-level "fire on detected drift" rule scoped to one
/// module's own actions: a `Skip` module action records no change, so it does
/// not count as drift.
///
/// The caller passes the plan the tick will act on, which the reconcile loop
/// has already pruned of every resource awaiting a source decision. A module
/// whose only drifting resource is excluded therefore reports no drift and
/// fires no `onDrift` hook — deliberate: the hook exists to react to work the
/// daemon is about to do, and an undecided resource is work it will not do.
pub(crate) fn module_has_drift(plan: &crate::reconciler::Plan, module_name: &str) -> bool {
    use crate::reconciler::{Action, ModuleActionKind};
    plan.phases.iter().flat_map(|p| p.actions()).any(|a| {
        matches!(
            a,
            Action::Module(ma)
                if ma.module_name == module_name
                    && !matches!(ma.kind, ModuleActionKind::Skip { .. })
        )
    })
}

// --- Auto-apply decision handling ---

/// Record the rows a [`SourcePolicyReview`] asked for, and notify once per
/// source.
///
/// The daemon's wrapper over the shared [`crate::reconciler::mint_decisions`]:
/// `cfgd apply` mints the same rows so an item is never installed before it is
/// asked about, but only the reconcile loop is unattended enough to need
/// telling the operator out of band.
pub(crate) fn mint_reviewed_decisions(
    store: &StateStore,
    review: &crate::reconciler::SourcePolicyReview,
    notifier: &Notifier,
) {
    // One notification per source rather than per item.
    for (source_name, count) in crate::reconciler::mint_decisions(store, review) {
        notifier.notify(
            "cfgd: pending decisions",
            &format!(
                "Source \"{}\" has {} new {} item{} pending your review.\n\
                 Run `cfgd status` to see details, `cfgd decide accept --source {}` to accept all.",
                source_name,
                count,
                if count == 1 {
                    "recommended"
                } else {
                    "recommended/optional"
                },
                if count == 1 { "" } else { "s" },
                source_name,
            ),
        );
    }
}
