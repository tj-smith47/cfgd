use super::*;
use crate::PathDisplayExt;
use crate::reconciler::{DecisionExclusions, withhold_from_plan};

// --- File Watcher ---

/// Whether a watch event's path is git bookkeeping rather than content.
///
/// A synced config dir is a git checkout watched recursively, so the daemon's
/// own periodic fetch rewrites `.git/FETCH_HEAD` under the watch — forwarding
/// those events makes every sync tick trigger an onChange reconcile of nothing.
/// cfgd never manages a file inside a `.git` directory (nor a `.git` gitlink
/// file), so anything under one is safe to drop at the watcher.
pub(crate) fn is_git_internal(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}

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
                            if is_git_internal(&path) {
                                continue;
                            }
                            match sender.try_send(path) {
                                Ok(()) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    tracing::debug!("file watcher channel full — event coalesced");
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "watch: file watcher event dropped");
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
            // A directory target watched NonRecursive reports only its
            // immediate children, so an edit nested one level deeper waited
            // for the interval tick while a sibling FILE target reconciled
            // immediately. Recurse into trees (`is_dir` follows a symlinked
            // target); the `.git` filter above keeps a checkout inside one
            // from self-triggering.
            let mode = if path.is_dir() {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            if let Err(e) = watcher.watch(path, mode) {
                tracing::warn!(path = %path.posix(), error = %e, "watch: cannot watch path");
            }
        } else if let Some(parent) = path.parent() {
            // Watch parent directory to detect file creation
            if parent.exists()
                && let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive)
            {
                tracing::warn!(path = %parent.posix(), error = %e, "watch: cannot watch path");
            }
        }
    }

    // Watch config directory for source changes
    if config_dir.exists()
        && let Err(e) = watcher.watch(config_dir, RecursiveMode::Recursive)
    {
        tracing::warn!(path = %config_dir.posix(), error = %e, "watch: cannot watch config dir");
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
            tracing::warn!(error = %e, "watch: cannot load config for file discovery");
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
            tracing::warn!(error = %e, "watch: cannot resolve profile for file discovery");
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
    /// The `--cache-dir` override; threaded to `compose_daemon_desired_state`
    /// and `resolve_daemon_modules` so a tick reads the SAME source/module
    /// cache every non-daemon verb honors, instead of silently falling back to
    /// the scope default. See `DaemonLoopContext::cache_dir_override`.
    pub cache_dir_override: Option<&'a Path>,
    pub printer: &'a crate::output::Printer,
    /// When set, restrict reconcile to actions targeting this module name.
    /// Used by per-module reconcile ticks fired from `ReconcilePatch` entries;
    /// the plan is filtered to retain only `Action::Module` entries whose
    /// `module_name` matches, and the two overrides below carry the per-module
    /// patch fields (`autoApply`, `driftPolicy`) so they actually drive
    /// behavior.
    pub module_filter: Option<&'a str>,
    /// Override for `cfg.spec.daemon.reconcile.auto_apply`, the source-decision
    /// gate. Unset falls back to the global config.
    pub auto_apply_override: Option<bool>,
    /// Override for `cfg.spec.daemon.reconcile.drift_policy`. Unset falls back
    /// to the global config. Set by a per-module tick (from its patch entry)
    /// and by a post-sync reconcile, which forces `Auto` because the source
    /// that changed asked for its refresh to be applied.
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

/// Run one reconcile tick and say, on the daemon's log, what it came to.
///
/// The announcement is the OUTCOME, not the start: a heartbeat per tick is
/// contentless — four of them in a row and no completion was what a reader of
/// this log actually got — while the outcome is the only line that distinguishes
/// a tick that converged from a tick that hung. [`reconcile_tick`] answers with
/// the sentence, or with `None` for a tick that never reached a verdict; every
/// one of those arms says why at `warn` or `error` on its way out, so start and
/// finish balance whichever way the tick ends.
pub(crate) fn handle_reconcile(
    config_path: &Path,
    profile_override: Option<&str>,
    ctx: ReconcileCtx<'_>,
) {
    if let Some(outcome) = reconcile_tick(config_path, profile_override, ctx) {
        tracing::info!("reconcile: complete — {outcome}");
    }
}

/// The tick itself. `Some(outcome)` is the sentence
/// [`handle_reconcile`] completes; `None` is a tick that ended before it
/// reached a verdict.
fn reconcile_tick(
    config_path: &Path,
    profile_override: Option<&str>,
    ctx: ReconcileCtx<'_>,
) -> Option<String> {
    let ReconcileCtx {
        state,
        notifier,
        notify_on_drift,
        hooks,
        state_dir_override,
        explicit_state_dir,
        cache_dir_override,
        printer,
        module_filter,
        auto_apply_override,
        drift_policy_override,
        scope,
        abort,
        cache,
    } = ctx;
    match module_filter {
        Some(name) => tracing::debug!(module = %name, "reconcile: running per-module check"),
        None => tracing::debug!("reconcile: running check"),
    }

    // Try to acquire the apply lock (non-blocking). If a CLI apply is in
    // progress, skip this reconciliation tick.
    let state_dir = match state_dir_override {
        Some(d) => d.to_path_buf(),
        None => match crate::state::default_state_dir() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "reconcile: cannot determine state directory");
                return None;
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
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, "reconcile: cannot acquire apply lock");
            return None;
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
                tracing::error!("reconcile: no profile configured — skipping");
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
        // manifest, failed signature) this tick is SKIPPED — never reconcile against
        // a substituted local-only desired state, because this is a pruning reconcile
        // and a dropped source-delivered package/module would be UNINSTALLED under
        // autoApply. Mirror the `resolve_profile` failure above: error + alert +
        // early-return, leaving the prior desired state (and last_reconcile) intact.
        // A benign never-synced cache-miss is warn+skip inside the resolver, not an
        // Err, so cache-miss still reconciles local-only.
        let composed = match super::compose_daemon_desired_state(
            &cfg,
            &local_resolved,
            printer,
            scope,
            cache_dir_override,
        ) {
                Ok(c) => c,
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
        hooks.extend_registry_custom_managers(&mut registry, &composed.resolved.merged.packages);

        Ok(super::tick_cache::DerivedConfig {
            cfg,
            profile_name,
            resolved: composed.resolved,
            source_module_roots: composed.source_module_roots,
            registry,
            source_advisories: composed.advisories,
        })
    });
    let Ok(derived) = derived else {
        return None;
    };
    // A source with no local cache, or one whose checkout came from an origin
    // the spec no longer names, is skipped by the composition and said out loud
    // by it. The condition persists until someone runs `cfgd sync`, so a tick
    // that REUSED the composition re-states what that composition said instead
    // of falling silent — an operator watching a warning stop reads it as fixed.
    for advisory in derived.advisories_to_restate() {
        advisory.restate(printer);
    }
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
            return None;
        }
    };
    let Some(store) = held_store.get() else {
        tracing::error!("reconcile: state store unavailable");
        return None;
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
        tracing::warn!(error = %e, "reconcile: failed to discard decisions of removed sources");
    }

    let available_managers = registry.available_package_managers();
    // The daemon is a full, unscoped reconcile, so it prunes: feed the real
    // cfgd-tracked set as `"<manager>/<identity>"` entries.
    let cfgd_installed: HashSet<String> = store
        .managed_package_ids()
        .unwrap_or_default()
        .into_iter()
        .map(|(mgr, pkg)| crate::state::package_resource_id(&mgr, &pkg))
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
            return None;
        }
    };

    // The policy review is profile-wide; skip it when scoped to a single
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
                return None;
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
            return None;
        }
    };
    let pending_exclusions =
        DecisionExclusions::from_withheld_with(&withheld, |p| hooks.expand_tilde(p));

    // The env arm withholds the surface as a unit, and apply rebuilds that
    // surface after the phases run from the declared set rather than from the
    // plan — so the pruning below is only half the guarantee without this.
    let reconciler = crate::reconciler::Reconciler::new(registry, store)
        .with_config_dir(&config_dir)
        .withholding_env_surface(pending_exclusions.withholds_env_surface())
        .withholding_rows(&pending_exclusions)
        .diffing_installed(&pkg_cx);

    // ONE file manager per tick: the manager that planned is the manager the
    // recorded-hash refresh below asks, because building a second one costs another
    // `cfgd.yaml` load and another secret-backend construction, every interval, for
    // the same answer.
    let (file_actions, file_manager) = match hooks.plan_files_with_manager(&config_dir, resolved) {
        Ok(planned) => planned,
        Err(e) => {
            tracing::error!(error = %e, "reconcile: file planning failed");
            return None;
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
            Some(&pkg_cx),
            printer,
            scope,
            cache_dir_override,
        );
        // The tick plans and (under auto-apply) applies, so its action descriptions
        // and recorded packages hash carry the version the read paths never ask for.
        // Survivor-gated: a package the machine already holds is elided from the
        // plan, so a converged tick queries nothing instead of pricing the whole
        // declared set on every interval. The compliance tick shares
        // `resolve_daemon_modules` and deliberately does NOT fill: nothing it
        // stores renders a version.
        reconciler.fill_planned_versions(&mut resolved_modules, &registry.manager_map());
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
            return None;
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
    // The prune also hands back the ids of every row it removed: the
    // AGGREGATE spellings — a bare `("module", name)`, a manager node — carry
    // no trace of the withheld resource, so the exclusions cannot re-derive
    // them from a recorded id alone, and the keep-set below folds these in
    // beside what `withholds_recorded_row` answers.
    let withheld_row_ids =
        withhold_from_plan(&mut plan, &pending_exclusions, registry).resource_ids;

    // Check drift policy to decide whether to auto-apply or just notify.
    // Per-module ticks may override the global value via their patch entry.
    // Resolved HERE rather than at the policy branch below, because the
    // unmanaged-file pass has to know the answer: it hashes every declared
    // target and source, and a tick that will only notify has nothing to
    // protect and no reason to pay for the walk.
    let drift_policy = drift_policy_override.clone().unwrap_or_else(|| {
        cfg.spec
            .daemon
            .as_ref()
            .and_then(|d| d.reconcile.as_ref())
            .map(|r| r.drift_policy.clone())
            .unwrap_or_default()
    });

    // A tick has nobody to ask, so it settles every unmanaged target the way
    // `--yes` does: keep a copy. Without this pass the daemon displaced a
    // user's own file with no copy kept, while `cfgd apply` over the identical
    // plan copied it aside — the same machine, two answers, decided by which
    // process got there first. Only an APPLYING tick sweeps: a report-only tick
    // displaces nothing, so a sidecar it took would be a copy of a file nobody
    // was about to overwrite.
    let reconciler = if matches!(drift_policy, config::DriftPolicy::Auto) {
        // The same root `resolve_daemon_modules` resolved the plan's own
        // modules from — a tick given `--cache-dir` symlinks its module files
        // there, and answering from the per-user default instead would read
        // every one of them as a stranger's file on this tick alone.
        let module_cache = super::tick_module_cache(&config_dir, scope, cache_dir_override);
        match crate::reconciler::sweep_unmanaged_file_targets(
            &mut plan,
            &config_dir,
            &module_cache,
            store,
            printer,
            &crate::effective::effective_file_strategies(
                &resolved.merged,
                resolved_modules_ref.as_slice(),
                &config_dir,
                registry.default_file_strategy,
            ),
            None,
            &mut |_, _| Ok(crate::reconciler::ResolvedConflict::Backup),
        ) {
            Ok(backups) => reconciler.backing_up(backups),
            Err(e) => {
                tracing::error!(error = %e, "reconcile: unmanaged-file pass failed");
                return None;
            }
        }
    } else {
        reconciler
    };
    // The plan's own promise, which already excludes a module the host
    // declined whole: that module probed nothing, so counting it would report
    // divergence no apply can settle and wake the policy branch every interval
    // for the life of the daemon. A refused file deploy IS counted here, as it
    // is everywhere else — it is a finding the reader must act on.
    let effective_total = plan.total_actions();

    let timestamp = crate::utc_now_iso8601();

    // A per-module tick moves only `module_last_reconcile`, so the
    // profile-wide "last reconcile" stamp keeps reflecting the default cadence.
    let header_modules = crate::output::HeaderModule::of_resolved(&resolved_modules_ref);
    let composed_sources = crate::reconciler::ComposedSource::from_declared(&cfg.spec.sources);
    let profile_inherits = resolved.inherits_chain();
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async {
        let mut st = state.lock().await;
        // The subscriptions are the CONFIG's, not the resolution's, so every
        // tick that read a config refreshes them: a SIGHUP that rewrites
        // `spec.sources` would otherwise leave the daemon's `Sources` row —
        // and the scheduled fire's header — naming the old subscriptions until
        // the next profile-wide tick, the startup seeding covering only the
        // never-ticked case. `profile` / `modules` below are RESOLUTION facts
        // and move only with the resolution that produced them; after a SIGHUP
        // the row names the new subscriptions beside the last profile-wide
        // resolution's modules until that resolution is redone, which is the
        // same staleness `daemon status` states for every other resolved fact.
        st.composed_sources = composed_sources.clone();
        if let Some(name) = module_filter {
            st.module_last_reconcile.insert(name.to_string(), timestamp);
        } else {
            st.last_reconcile = Some(timestamp);
            // The one resolution the loop performs, handed to the status
            // endpoint and to the scheduled backup fire so `daemon status` and
            // an unattended run header name what `cfgd status` and an apply
            // header name. A per-module tick resolved a SUBSET and says
            // nothing about the profile.
            //
            // The name comes from this resolution rather than being left at
            // what the pre-loop setup read: the fire compares a due unit's
            // profile against it to decide whether these sources and modules
            // describe that unit's configuration, so a name from one resolution
            // beside the layers of another is the one way that comparison can
            // answer wrongly.
            st.profile = Some(resolved.profile_name().to_string());
            st.modules = header_modules.clone();
            st.profile_inherits = profile_inherits.clone();
        }
    });

    // This tick just performed a live drift scan of the machine, whatever it
    // found — the recorded-state `status` header's staleness signal reads
    // from here, not from `drift_events` (which goes empty on a clean host
    // and so cannot date a clean scan).
    store.record_scan();

    // A file deployed by symlink is edited THROUGH the link, which is the source
    // changing and so is never drift — the tick above found none, and no action
    // will revisit the row. Correct the recorded content hash here, before the
    // drift branch, so a clean tick and a drifted one both do it; the write is
    // skipped when the hash already agrees, so a settled machine pays nothing per
    // interval. The file manager is the one this tick already planned through, and
    // is absent for a hook that owns none — the module half is refreshed either way.
    //
    // The count comes out with it. A pull that lands another machine's edit
    // through a symlink leaves nothing to plan, so the tick that carried the
    // sync closed on the same `nothing to do` as the four idle ticks above it
    // — this is the only fact that separates the two.
    let refreshed_hashes = match reconciler.refresh_link_deployed_hashes(
        file_manager.as_deref(),
        resolved,
        resolved_modules_ref.as_slice(),
    ) {
        Ok(refreshed) => refreshed,
        Err(e) => {
            tracing::warn!(error = %e, "reconcile: failed to refresh recorded file hashes");
            crate::reconciler::RefreshedHashes::default()
        }
    };

    // The scope a per-module tick may speak for: its own group's packages
    // (module-order dedup — a package an earlier resolved module also
    // declares is dedup'd onto that module's group, `dedup_module_packages`)
    // plus the env-var/alias names it declares. Looked up once for the whole
    // scoped set: the identity fold inside `module_scope` asks per package,
    // and a linear registry scan there is quadratic in a module declaring a
    // few hundred packages.
    let module_scope: crate::reconciler::ModuleScope = match module_filter {
        None => crate::reconciler::ModuleScope::default(),
        Some(name) => {
            let managers: std::collections::HashMap<&str, &dyn crate::providers::PackageManager> =
                registry
                    .package_managers()
                    .iter()
                    .map(|m| (m.name(), m.as_ref()))
                    .collect();
            crate::reconciler::module_scope(resolved_modules_ref.as_slice(), name, &managers)
        }
    };

    // A per-module tick probes ONE module, so beyond the re-find predicate it
    // may heal only rows attributable to that module by identity: every
    // `module` row under its name — the per-file `<name>/<target>` rows and the
    // `<name>:script` / `<name>:skip` spellings its own actions mint — plus the
    // per-package and per-shell rows its group carries. Everything else —
    // other modules', the machine-wide surfaces — stands for the next
    // profile-wide tick to judge.
    let outside_tick_scope = |rtype: &str, rid: &str| match module_filter {
        None => false,
        Some(name) => {
            !crate::reconciler::row_attributable_to_module(rtype, rid, name, &module_scope)
        }
    };
    // The rows this tick cannot vouch for either way, spelled as extra
    // members of the "current" set so the complement-resolve leaves them
    // standing. The plan the complement reads has already had the
    // pending-decision prune applied, so a withheld resource's rows are kept
    // by the exclusions themselves — the tick deliberately did not judge
    // them. A read failure keeps EVERY row standing: resolving against a set
    // of unknown membership is how a finding gets healed blind.
    let kept_rows = |planned: &[&crate::reconciler::Action]| match store.unresolved_drift() {
        Ok(rows) => Some(
            rows.into_iter()
                .filter(|e| {
                    outside_tick_scope(&e.resource_type, &e.resource_id)
                        || pending_exclusions
                            .withholds_recorded_row(&e.resource_type, &e.resource_id)
                        || tick_cannot_refind(
                            &e.resource_type,
                            &e.resource_id,
                            planned,
                            module_filter.is_some(),
                        )
                })
                .map(|e| (e.resource_type, e.resource_id))
                // The ids the prune itself removed from the plan: the
                // aggregate spellings the exclusions cannot answer for. Ids
                // no recorded row carries are harmless extra members of the
                // keep-set — resolve_drift_not_in only spares what exists.
                .chain(withheld_row_ids.iter().cloned())
                .collect::<Vec<_>>(),
        ),
        Err(e) => {
            tracing::warn!(error = %e, "reconcile: cannot read recorded drift — leaving every recorded row standing");
            None
        }
    };

    let outcome = if effective_total == 0 {
        tracing::debug!("reconcile: no drift detected");

        // This reconcile is the ground-truth snapshot for everything it
        // PROBED: a recorded row the tick re-found healed clears, while one
        // it cannot re-find under its own grammar or scope stands. The
        // in-memory count follows the store rather than assuming 0, so a
        // kept row still shows on `/status` and `/drift`.
        if let Some(keep) = kept_rows(&[])
            && let Err(e) = store.resolve_drift_not_in(&keep)
        {
            tracing::warn!(error = %e, "reconcile: failed to resolve outstanding drift on clean tick");
        }
        rt.block_on(async {
            let mut st = state.lock().await;
            st.drift_count = super::drift::current_drift_count(store).unwrap_or(0);
        });
        Some("nothing to do".to_string())
    } else {
        tracing::info!(
            "reconcile: drift detected in {}",
            crate::pluralize(effective_total, "resource")
        );

        // The plan's action set is the exact current drift set. Record each
        // diverging resource (UPSERT — no duplicate rows across ticks)...
        // One transaction over the whole record-and-resolve batch: each
        // upsert is two scans of an append-only table, and a per-row implicit
        // commit is its own WAL write every interval for the life of the
        // daemon. Per-row failures stay warnings — one refused row must not
        // roll back the rest of the snapshot.
        let mut planned: Vec<&crate::reconciler::Action> =
            Vec::with_capacity(plan.phases.iter().map(|p| p.action_count()).sum());
        if let Err(e) = store.in_transaction(|| {
            let mut current_drift: Vec<(String, String)> = Vec::new();
            for phase in &plan.phases {
                for action in phase.actions() {
                    // The ONE producer both sides read: what this tick records
                    // is exactly what an apply of the same action settles, and
                    // an action this host was never going to run yields no row
                    // at all — the header's total already excluded it.
                    for row in crate::reconciler::action_drift_rows(action, registry) {
                        if let Err(e) = store.record_drift(
                            &row.resource_type,
                            &row.resource_id,
                            row.expected.as_deref(),
                            row.actual.as_deref(),
                            config::LOCAL_LAYER,
                        ) {
                            tracing::warn!(error = %e, "reconcile: failed to record drift");
                        }
                        current_drift.push(row.key());
                    }
                    planned.push(action);
                }
            }
            // ...then resolve any still-unresolved rows NOT in the current
            // set — they healed since the last tick — where "current" also
            // carries every recorded row this tick's plan re-finds under
            // another producer's spelling or cannot judge at all.
            if let Some(keep) = kept_rows(&planned) {
                current_drift.extend(keep);
                if let Err(e) = store.resolve_drift_not_in(&current_drift) {
                    tracing::warn!(error = %e, "reconcile: failed to resolve healed drift rows");
                }
            }
            Ok(())
        }) {
            tracing::warn!(error = %e, "reconcile: failed to commit the drift snapshot");
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
                tracing::debug!(
                    count = profile_hooks.len(),
                    "reconcile: running onDrift scripts"
                );
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
                            tracing::debug!(script = %desc, "reconcile: onDrift script completed");
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "reconcile: onDrift script failed");
                        }
                    }
                }
            }

            for module in &drifted_modules {
                tracing::debug!(
                    module = %module.name,
                    count = module.on_drift_scripts.len(),
                    "reconcile: running module onDrift scripts"
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
                            tracing::debug!(module = %module.name, script = %desc, "reconcile: module onDrift script completed");
                        }
                        Err(e) => {
                            tracing::error!(module = %module.name, error = %e, "reconcile: module onDrift script failed");
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
            });
        }

        // The rows every arm below prints above its own body: a tick reports
        // the same run skeleton `cfgd apply` does, so the two surfaces cannot
        // describe one machine differently. Built once, before the policy
        // branch, because an applying tick and a notify-only tick differ in
        // what they do — never in what they are reconciling.
        let trigger = format!("drift ({effective_total} resources)");
        let run_ctx = || crate::reconciler::RunContext {
            title: crate::reconciler::RunTitle::Reconcile,
            config_path: Some(config_path),
            profile: Some(profile_name),
            sources: &composed_sources,
            modules: &header_modules,
            profile_inherits: &profile_inherits,
            trigger: Some(&trigger),
            subject: None,
            unit_source: None,
        };

        match drift_policy {
            config::DriftPolicy::Auto => {
                tracing::debug!(
                    actions = effective_total,
                    "reconcile: drift policy is Auto — applying actions"
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
                    Ok(None) => Some("nothing to do".to_string()),
                    Ok(Some(result)) => {
                        let succeeded = result.succeeded();
                        let failed = result.failed();
                        tracing::debug!(
                            succeeded = succeeded,
                            failed = failed,
                            "reconcile: auto-apply complete"
                        );
                        // The rows the apply just recorded carry no hash. The
                        // refresh above ran BEFORE the apply, so without this
                        // one the next tick backfills them and reports the
                        // backfill as deployed files having moved. Its count
                        // is bookkeeping about rows this tick wrote, not news.
                        if let Err(e) = reconciler.refresh_link_deployed_hashes(
                            file_manager.as_deref(),
                            resolved,
                            resolved_modules_ref.as_slice(),
                        ) {
                            tracing::warn!(error = %e, "reconcile: failed to seed recorded file hashes after apply");
                        }
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
                                        let rid = crate::state::package_resource_id(&mgr, &id);
                                        if let Err(e) =
                                            store.remove_managed_resource("package", &rid)
                                        {
                                            tracing::warn!(resource = %rid, error = %e, "reconcile: failed to GC stale package tracking row");
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "reconcile: failed to compute stale package tracking rows")
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
                                        let rid = crate::state::package_resource_id(&mgr, &pkg);
                                        if let Err(e) =
                                            store.remove_managed_resource("package", &rid)
                                        {
                                            tracing::warn!(resource = %rid, error = %e, "reconcile: failed to GC orphaned package tracking row");
                                        }
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(error = %e, "reconcile: failed to compute orphaned package rows")
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
                            });
                        }

                        // The tally the on-screen rollup above this line was
                        // built from, so the log and the rollup cannot disagree
                        // about how many actions succeeded. `outcome_counts` is
                        // silent about failures — the rollup gives them their
                        // own line — but a single-line log has no second line,
                        // so it names them here or hides them entirely.
                        let tally = result.tally();
                        let counts = crate::reconciler::outcome_counts(&tally);
                        Some(match tally.failed {
                            0 => counts,
                            failed => format!("{counts}, {failed} failed"),
                        })
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "reconcile: auto-apply failed");
                        if notify_on_drift {
                            notifier.notify(
                                "cfgd: auto-apply failed",
                                &format!("Auto-apply failed: {}. Run `cfgd apply` manually.", e),
                            );
                        }
                        None
                    }
                }
            }
            config::DriftPolicy::NotifyOnly | config::DriftPolicy::Prompt => {
                tracing::debug!(
                    "reconcile: drift policy is NotifyOnly — recording drift, not applying"
                );
                // A tick that detected drift and chose not to act still has to
                // show WHAT drifted, so it renders the preview tree — never an
                // execution tree — and closes on a verdict instead of a rollup.
                let run = crate::reconciler::ApplyRun::new(run_ctx(), &plan).preview_only();
                run.header(printer);
                run.preview(printer);
                // Every row the tree above printed is counted by some clause:
                // the drifted ones, and the rest as skipped. A sentence naming
                // fewer than the reader just saw is the drift.
                let skipped = plan.listed_action_count().saturating_sub(effective_total);
                let counted = if skipped == 0 {
                    format!("{} drifted", crate::pluralize(effective_total, "action"))
                } else {
                    format!(
                        "{} drifted, {} skipped",
                        crate::pluralize(effective_total, "action"),
                        skipped
                    )
                };
                printer
                    .status(crate::output::Role::Warn, "Drift detected")
                    .detail(format!("{counted}; policy is notify-only, nothing applied"));
                if notify_on_drift {
                    notifier.notify(
                        "cfgd: drift detected",
                        &format!(
                            "{} drifted from desired state. Run `cfgd apply` to reconcile.",
                            crate::pluralize(effective_total, "resource")
                        ),
                    );
                }
                Some(format!("{counted}, none applied"))
            }
        }
    };

    // The sentence states what the tick OBSERVED as well as what it did to
    // the machine. Folded here rather than in the clean arm, so every arm
    // of the branch above reports it on the same terms. Worded from the
    // reader's side: a pull moved bytes under a symlink, and nothing needed
    // doing because the link already delivers them. What cfgd refreshed is
    // its own record of those bytes, and a clause about bookkeeping printed
    // as work ("N deployed files refreshed") contradicted the "nothing to do"
    // beside it. Counted in FILES that MOVED, never rows and never a row's
    // coverage; a count the rows cannot prove is left unsaid.
    let outcome = match (refreshed_hashes.rows, refreshed_hashes.files) {
        (0, _) => outcome,
        (_, Some(moved)) => outcome.map(|sentence| {
            let link = if moved == 1 {
                "its link"
            } else {
                "their links"
            };
            format!(
                "{sentence}, {} changed upstream, already live through {link}",
                crate::pluralize(moved, "deployed file")
            )
        }),
        (_, None) => outcome.map(|sentence| {
            format!("{sentence}, deployed files changed upstream, already live through their links")
        }),
    };

    // A per-module tick names its module: the log carries ticks of both
    // cadences interleaved, and a bare completion sentence cannot say which of
    // them just converged.
    let outcome = match module_filter {
        Some(name) => outcome.map(|sentence| format!("module {name}: {sentence}")),
        None => outcome,
    };

    // Server check-in + pending-config consumption are profile-wide
    // operations; skip them for per-module ticks so a fast per-module cadence
    // doesn't hammer the gateway or race the default reconcile.
    if module_filter.is_some() {
        return outcome;
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
            tracing::debug!(keys = ?keys, "daemon: pending server config keys");
            tracing::info!(
                "daemon: consumed pending server config — next reconcile will pick up changes"
            );
            if let Err(e) = crate::state::clear_pending_server_config() {
                tracing::warn!(error = %e, "daemon: failed to clear pending server config");
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, "daemon: failed to load pending server config");
        }
    }

    outcome
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
/// A [`crate::reconciler::ModuleActionKind::FilesRefused`] is NOT excluded, so a
/// module whose only planned action is the refusal counts as drifted and fires
/// its `onDrift` hook every interval for as long as the refusal stands. That is
/// the intended reading of both halves: the module's declared files are not on
/// the machine and no tick will put them there until the source is encrypted (or
/// its strategy stops demanding it), which is exactly the standing divergence
/// the hook exists to announce — where a host-declined module is settled, not
/// diverged. Every other surface prices the refusal the same way: the header
/// counts it, both trees draw it, and the tick's closing sentence names it.
///
/// The caller passes the plan the tick will act on, which the reconcile loop
/// has already pruned of every resource awaiting a source decision. A module
/// whose only drifting resource is excluded therefore reports no drift and
/// fires no `onDrift` hook — deliberate: the hook exists to react to work the
/// daemon is about to do, and an undecided resource is work it will not do.
pub(crate) fn module_has_drift(plan: &crate::reconciler::Plan, module_name: &str) -> bool {
    use crate::reconciler::Action;
    plan.phases.iter().flat_map(|p| p.actions()).any(|a| {
        matches!(a, Action::Module(ma) if ma.module_name == module_name)
            && !crate::reconciler::module_skipped_whole(a)
    })
}

/// The daemon twin of the CLI keep predicate (`full_check_cannot_refind`,
/// `crates/cfgd/src/cli/live_drift.rs`): whether a recorded drift row is one
/// THIS tick's plan cannot vouch for, so the complement-resolve must leave it
/// standing. The tick records one row per planned action in the daemon's own
/// grammar; a row another producer spelled differently is re-found only when
/// a planned action still covers the same fact, and a row about a surface the
/// tick never probes is never the tick's to heal.
///
/// What the tick genuinely re-finds, judged from the recorded row's side:
///
/// * `env-var` / `alias` — the CLI's per-item shell rows. The tick checks the
///   generated env FILE as a whole: a planned rewrite means the file is stale
///   and says nothing about which items still mismatch (keep), while no
///   planned rewrite means the file converged, which re-finds every declared
///   item healed (resolve) — but ONLY for a profile-wide tick. `narrow_to_module`
///   drops the whole env group unconditionally (it is machine-wide, owned by
///   no module), so a SCOPED tick's `planned` can never carry a `WriteEnvFile`
///   action whether or not the real file needs one; reading that absence as
///   "converged" would heal every env-var/alias row this module's chain owns
///   on every scoped tick, blind. `scoped` is what tells the two cases apart.
/// * `package` `<manager>:<identity>` — the CLI's per-package spelling, which
///   this tick now mints too, so identity governs: a package a planned batch
///   still carries is already in the current set, and one no batch carries is
///   installed. `provision:` / `refuse:` rows are the one shape the tick does
///   not mint per member — a cascade's node carries only its leader's id — so
///   they are re-found through the manager node that speaks for the manager.
/// * `module` — every spelling under a module's name: the per-file
///   `<module>/<target>` rows this tick mints itself through
///   `module_file_spec_resource_id`, and the `<module>:packages:…` /
///   `<module>:script` / `<module>:skip` ids its other kinds mint. All of them
///   are the tick's own grammar, so identity governs — a row the plan still
///   covers is in the current set before this predicate is consulted, and one
///   it does not cover is a file or a surface the plan found converged. The
///   exception is a module whose files the plan never probed
///   ([`crate::reconciler::module_files_unprobed`]): the host declined it whole,
///   or it refused the deploy before reading a target. Its rows are kept the way
///   the CLI keeps an unevaluated configurator's — a run that declined to look
///   proves nothing about what it did not read.
/// * `system` `<configurator>.<key>` — [`crate::reconciler::system_resource_key`]'s
///   spelling, which the tick's own `SetValue` mints too, so a row is re-found
///   by comparing the whole composed id rather than splitting it. A planned
///   `SystemAction::Skip` names a configurator this tick never probed (a
///   registered tool that left the host, a platform gate), and every row
///   under it is kept — the tick's twin of the CLI's `evaluated_system`
///   discipline.
/// * Every type the tick's own grammar mints resolves by row identity — a
///   standing daemon row is already in the current set before this predicate
///   is consulted.
/// * A type neither grammar knows belongs to a producer this tick knows
///   nothing about: keep.
///
/// A row a planned action covers stands even while this very tick's
/// auto-apply may heal it — fail-safe overstatement against wrongly healing a
/// finding the machine still shows. The ceiling is "until the plan stops
/// covering it": one interval under auto-apply, and under `NotifyOnly` until
/// the operator applies (for the per-item `env-var`/`alias` rows, until the
/// whole env file converges, since a planned rewrite says nothing per-item).
pub(super) fn tick_cannot_refind(
    resource_type: &str,
    resource_id: &str,
    planned: &[&crate::reconciler::Action],
    scoped: bool,
) -> bool {
    use crate::reconciler::{Action, EnvAction, SystemAction};
    match resource_type {
        "env-var" | "alias" => {
            scoped
                || planned
                    .iter()
                    .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })))
        }
        "package" => {
            if let Some(manager) = resource_id
                .strip_prefix("provision:")
                .or_else(|| resource_id.strip_prefix("refuse:"))
            {
                return planned.iter().any(|a| match a {
                    Action::Manager(ma) => {
                        ma.provisioned_managers().contains(&manager)
                            || ma.resource_id() == resource_id
                    }
                    _ => false,
                });
            }
            // Every other package row is the tick's own spelling: identity
            // governs, and a row the plan still covers never reaches here.
            false
        }
        // A module whose files this tick never probed — the host declined it
        // whole, or it refused the deploy outright.
        //
        // The keep is deliberately wider than the file rows it exists for: a
        // refused module's `<mod>:script` row is kept too, once `hooks_now`
        // (`reconciler/plan.rs`) elides the hooks because the module's package
        // work converged while the refusal still stands. Nothing in that window
        // vouches for the script's convergence, so healing it would be exactly
        // the blind heal this predicate prevents — over-report, never
        // under-report.
        "module" => {
            let owner = crate::reconciler::module_row_owner(resource_id);
            planned.iter().any(|a| {
                matches!(a, Action::Module(ma) if ma.module_name == owner)
                    && crate::reconciler::module_files_unprobed(a)
            })
        }
        // Judged whole against the composer's output rather than parsed: a KEY
        // may carry a colon (`windowsRegistry.HKCU:\Software\…`), so nothing
        // here may read one as a separator.
        "system" => planned.iter().any(|a| match a {
            Action::System(SystemAction::SetValue {
                configurator, key, ..
            }) => crate::reconciler::system_resource_key(configurator, key) == resource_id,
            // A registered-but-unavailable configurator plans Skip and probes
            // nothing, so a row under its namespace is vouched for neither
            // way — the tick's twin of the CLI's `evaluated_system` keep.
            Action::System(SystemAction::Skip { configurator, .. }) => resource_id
                .strip_prefix(configurator.as_str())
                .is_some_and(|rest| rest.starts_with('.')),
            _ => false,
        }),
        // script-literal-ok: resource TYPES, not manager names
        "file" | "secret" | "script" | "env" | "env-rc" | "env-session" | "manager" => false,
        _ => true,
    }
}

// --- Auto-apply decision handling ---

/// Record the rows a [`crate::reconciler::SourcePolicyReview`] asked for, and notify once per
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
