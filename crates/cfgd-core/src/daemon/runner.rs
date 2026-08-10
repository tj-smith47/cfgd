// Daemon loop runner — extracted from `run_daemon` for testability.
//
// The select! loop and per-branch orchestration live here. `run_daemon` (in
// mod.rs) handles real-world wiring (config loading, file watchers, signal
// handlers) and then hands a `DaemonLoopContext` + `DaemonTriggers` to
// `run_daemon_loop`. Tests drive the loop directly via mpsc-based triggers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc, oneshot};

use super::backup::{
    BackupReloadSummary, BackupTimers, DegradedReason, resolve_backup_tasks, run_scheduled_backup,
};
use super::reconcile::{ReconcileCtx, handle_reconcile};
use super::sync::{handle_compliance_snapshot, handle_sync, handle_version_check};
use super::{
    DEBOUNCE_MS, DaemonHooks, DaemonState, Notifier, ReconcileTask, SourceStatus, SyncTask,
    parse_duration_or_default,
};
use crate::PathDisplayExt;
use crate::config::{self, CfgdConfig};
use crate::errors::{DaemonError, Result};
use crate::output::{Printer, Role};
use crate::state::StateStore;

/// Shared message for every per-tick error in the select loop; the `tick` field
/// distinguishes which handler failed.
const TICK_FAILED_MSG: &str = "daemon tick failed; loop continues";

/// How long the backup timer branch parks when no backup is scheduled. Nothing
/// depends on the wakeup — it exists only so the branch has a deadline to hold
/// while the other arms of the select drive the loop.
const BACKUP_IDLE_PARK: Duration = Duration::from_secs(3600);

pub(super) struct DaemonLoopContext {
    pub state: Arc<Mutex<DaemonState>>,
    pub hooks: Arc<dyn DaemonHooks>,
    pub notifier: Arc<Notifier>,
    pub config_path: PathBuf,
    pub profile_override: Option<String>,
    pub on_change_reconcile: bool,
    pub notify_on_drift: bool,
    pub compliance_config: Option<config::ComplianceConfig>,
    pub printer: Arc<Printer>,
    /// When set, `handle_reconcile` uses this directory instead of the
    /// platform default state dir. Tests pass a tempdir here so the loop
    /// never touches `~/.local/state/cfgd/`. The production loop ALWAYS sets
    /// it — `run_daemon_with` materializes the scope default so every
    /// downstream site agrees on one path — which is why it cannot double as
    /// "the operator overrode the state dir": that fact rides
    /// [`Self::explicit_state_dir`].
    pub state_dir_override: Option<PathBuf>,
    /// Whether the OPERATOR passed `--state-dir` (captured before the default
    /// is materialized into `state_dir_override`). Bringing your own state
    /// dir is what makes a foreign config authoritative over the store it
    /// sweeps and mints into, so the ownership gate reads THIS bit — reading
    /// `state_dir_override.is_some()` instead made every production tick an
    /// owner of whatever store the scope default resolved.
    pub explicit_state_dir: bool,
    /// Managed file targets the profile declares. A file-watch event records
    /// drift only when its path is one of these; config/source/`.git` paths
    /// trigger a reconcile but are not drift.
    pub managed_paths: Vec<PathBuf>,
    /// Deployment scope that selects FHS vs XDG directory roots.
    pub scope: crate::Scope,
    /// Raised when the daemon is asked to stop, BEFORE the shutdown trigger
    /// fires. In-flight blocking work that can take minutes — a backup's
    /// `preBackup` hook — polls it and gives up early, so a stop request is not
    /// held hostage by a hook's own timeout.
    pub abort: Arc<crate::AbortFlag>,
    /// The running cfgd binary's version (its `env!("CARGO_PKG_VERSION")`),
    /// passed in by the binary — the daemon's update check and skill-staleness
    /// probes compare against the *binary*, never cfgd-core's own version.
    pub cfgd_version: String,
}

pub(super) struct DaemonTriggers {
    pub file_rx: mpsc::Receiver<PathBuf>,
    pub reconcile_rx: mpsc::Receiver<()>,
    pub sync_rx: mpsc::Receiver<()>,
    pub version_check_rx: mpsc::Receiver<()>,
    pub compliance_rx: mpsc::Receiver<()>,
    pub sighup_rx: mpsc::Receiver<()>,
    pub shutdown_rx: oneshot::Receiver<()>,
}

/// Run the daemon's main select loop.
///
/// `reconcile_interval_secs` and `sync_interval_secs` are shared with the
/// production pump tasks; the SIGHUP branch updates them so subsequent ticks
/// fire at the new cadence. In tests, the atomics are inspected to verify the
/// SIGHUP branch took the expected action.
///
/// `backup_tasks` needs no pump: reconcile and sync run on one fixed cadence
/// each, whereas every scheduled backup carries its own (and a cron's gaps are
/// uneven), so the loop parks on the soonest deadline in the set instead of
/// polling a shared interval and asking each unit whether it is due yet.
pub(super) async fn run_daemon_loop(
    ctx: DaemonLoopContext,
    mut triggers: DaemonTriggers,
    mut reconcile_tasks: Vec<ReconcileTask>,
    mut sync_tasks: Vec<SyncTask>,
    mut backup_timers: BackupTimers,
    reconcile_interval_secs: Arc<AtomicU64>,
    sync_interval_secs: Arc<AtomicU64>,
) -> Result<()> {
    let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
    let debounce = Duration::from_millis(DEBOUNCE_MS);

    loop {
        let backup_deadline = tokio::time::Instant::from_std(next_backup_deadline(&backup_timers));

        tokio::select! {
            Some(path) = triggers.file_rx.recv() => {
                if let Err(e) = handle_file_change_tick(&ctx, &mut last_change, debounce, path).await {
                    tracing::error!(error = %e, tick = "file_change", "{TICK_FAILED_MSG}");
                }
            }

            Some(()) = triggers.reconcile_rx.recv() => {
                if let Err(e) = handle_reconcile_tick(&ctx, &mut reconcile_tasks).await {
                    tracing::error!(error = %e, tick = "reconcile", "{TICK_FAILED_MSG}");
                }
            }

            Some(()) = triggers.sync_rx.recv() => {
                if let Err(e) = handle_sync_tick(&ctx, &mut sync_tasks).await {
                    tracing::error!(error = %e, tick = "sync", "{TICK_FAILED_MSG}");
                }
            }

            Some(()) = triggers.version_check_rx.recv() => {
                if let Err(e) = handle_version_check_tick(&ctx).await {
                    tracing::error!(error = %e, tick = "version_check", "{TICK_FAILED_MSG}");
                }
            }

            Some(()) = triggers.compliance_rx.recv() => {
                if let Err(e) = handle_compliance_tick(&ctx).await {
                    tracing::error!(error = %e, tick = "compliance", "{TICK_FAILED_MSG}");
                }
            }

            _ = tokio::time::sleep_until(backup_deadline) => {
                if let Err(e) = handle_backup_tick(&ctx, &mut backup_timers).await {
                    tracing::error!(error = %e, tick = "backup", "{TICK_FAILED_MSG}");
                }
            }

            Some(()) = triggers.sighup_rx.recv() => {
                apply_sighup_reload(
                    &ctx,
                    &reconcile_interval_secs,
                    &sync_interval_secs,
                    &mut backup_timers,
                );
            }

            _ = &mut triggers.shutdown_rx => {
                break;
            }
        }
    }

    Ok(())
}

/// Process a single file-change event: debounce, record drift, optionally
/// trigger an immediate reconcile.
pub(super) async fn handle_file_change_tick(
    ctx: &DaemonLoopContext,
    last_change: &mut HashMap<PathBuf, Instant>,
    debounce: Duration,
    path: PathBuf,
) -> Result<()> {
    let now = Instant::now();
    if let Some(last) = last_change.get(&path)
        && now.duration_since(*last) < debounce
    {
        return Ok(());
    }
    last_change.insert(path.clone(), now);

    tracing::info!(path = %path.posix(), "file changed");

    // A change to a config/source/`.git` path is a desired-state UPDATE that
    // triggers a reconcile below — it is NOT drift. Only a change to a managed
    // TARGET diverging from desired state counts.
    let is_managed = super::drift::path_is_managed_target(&path, &ctx.managed_paths);
    if is_managed {
        let store = match ctx.state_dir_override.as_deref() {
            Some(dir) => StateStore::open_in_dir(dir),
            None => StateStore::open_default(),
        };
        match store {
            Ok(store) => {
                if super::drift::record_file_drift_to(&store, &path) {
                    if let Some(count) = super::drift::current_drift_count(&store) {
                        let mut st = ctx.state.lock().await;
                        st.drift_count = count;
                        if let Some(source) = st.sources.first_mut() {
                            source.drift_count = count;
                        }
                    }

                    if ctx.notify_on_drift {
                        ctx.notifier.notify(
                            "cfgd: drift detected",
                            &format!("File changed: {}", path.posix()),
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot open state store for drift recording");
            }
        }
    }

    if ctx.on_change_reconcile {
        let cp = ctx.config_path.clone();
        let po = ctx.profile_override.clone();
        let st = Arc::clone(&ctx.state);
        let nt = Arc::clone(&ctx.notifier);
        let notify_drift = ctx.notify_on_drift;
        let hk = Arc::clone(&ctx.hooks);
        let state_dir = ctx.state_dir_override.clone();
        let explicit = ctx.explicit_state_dir;
        let printer = Arc::clone(&ctx.printer);
        let scope = ctx.scope;
        let abort = Arc::clone(&ctx.abort);
        crate::spawn_blocking_with_test_home(move || {
            handle_reconcile(
                &cp,
                po.as_deref(),
                ReconcileCtx {
                    state: &st,
                    notifier: &nt,
                    notify_on_drift: notify_drift,
                    hooks: &*hk,
                    state_dir_override: state_dir.as_deref(),
                    explicit_state_dir: explicit,
                    printer: &printer,
                    module_filter: None,
                    auto_apply_override: None,
                    drift_policy_override: None,
                    scope,
                    abort: &abort,
                },
            );
        })
        .await
        .map_err(|e| DaemonError::WatchError {
            message: format!("reconcile task failed: {}", e),
        })?;
    }

    Ok(())
}

pub(super) async fn handle_reconcile_tick(
    ctx: &DaemonLoopContext,
    reconcile_tasks: &mut [ReconcileTask],
) -> Result<()> {
    tracing::trace!("reconcile tick");
    let now = Instant::now();

    let mut ran_default = false;
    for task in reconcile_tasks.iter_mut() {
        if let Some(last) = task.last_reconciled
            && now.duration_since(last) < task.interval
        {
            continue;
        }
        task.last_reconciled = Some(now);

        if task.entity == "__default__" {
            ran_default = true;
            let cp = ctx.config_path.clone();
            let po = ctx.profile_override.clone();
            let st = Arc::clone(&ctx.state);
            let nt = Arc::clone(&ctx.notifier);
            let notify_drift = ctx.notify_on_drift;
            let hk = Arc::clone(&ctx.hooks);
            let state_dir = ctx.state_dir_override.clone();
            let explicit = ctx.explicit_state_dir;
            let printer = Arc::clone(&ctx.printer);
            let scope = ctx.scope;
            let abort = Arc::clone(&ctx.abort);
            crate::spawn_blocking_with_test_home(move || {
                handle_reconcile(
                    &cp,
                    po.as_deref(),
                    ReconcileCtx {
                        state: &st,
                        notifier: &nt,
                        notify_on_drift: notify_drift,
                        hooks: &*hk,
                        state_dir_override: state_dir.as_deref(),
                        explicit_state_dir: explicit,
                        printer: &printer,
                        module_filter: None,
                        auto_apply_override: None,
                        drift_policy_override: None,
                        scope,
                        abort: &abort,
                    },
                );
            })
            .await
            .map_err(|e| DaemonError::WatchError {
                message: format!("reconcile task failed: {}", e),
            })?;
        } else {
            let entity_name = task.entity.clone();
            let task_auto_apply = task.auto_apply;
            let task_drift_policy = task.drift_policy.clone();
            tracing::info!(
                module = %entity_name,
                interval = %task.interval.as_secs(),
                auto_apply = task_auto_apply,
                drift_policy = ?task_drift_policy,
                "per-module reconcile tick"
            );
            let cp = ctx.config_path.clone();
            let po = ctx.profile_override.clone();
            let st = Arc::clone(&ctx.state);
            let nt = Arc::clone(&ctx.notifier);
            let notify_drift = ctx.notify_on_drift;
            let hk = Arc::clone(&ctx.hooks);
            let state_dir = ctx.state_dir_override.clone();
            let explicit = ctx.explicit_state_dir;
            let printer = Arc::clone(&ctx.printer);
            let module_name = entity_name.clone();
            let scope = ctx.scope;
            let abort = Arc::clone(&ctx.abort);
            crate::spawn_blocking_with_test_home(move || {
                handle_reconcile(
                    &cp,
                    po.as_deref(),
                    ReconcileCtx {
                        state: &st,
                        notifier: &nt,
                        notify_on_drift: notify_drift,
                        hooks: &*hk,
                        state_dir_override: state_dir.as_deref(),
                        explicit_state_dir: explicit,
                        printer: &printer,
                        module_filter: Some(&module_name),
                        auto_apply_override: Some(task_auto_apply),
                        drift_policy_override: Some(task_drift_policy),
                        scope,
                        abort: &abort,
                    },
                );
            })
            .await
            .map_err(|e| DaemonError::WatchError {
                message: format!("per-module reconcile task failed: {}", e),
            })?;
        }
    }

    if !ran_default {
        tracing::trace!("default reconcile task not due this tick");
    }
    Ok(())
}

pub(super) async fn handle_sync_tick(
    ctx: &DaemonLoopContext,
    sync_tasks: &mut [SyncTask],
) -> Result<()> {
    tracing::trace!("sync tick");
    let now = Instant::now();
    for task in sync_tasks.iter_mut() {
        if let Some(last) = task.last_synced
            && now.duration_since(last) < task.interval
        {
            continue;
        }
        task.last_synced = Some(now);

        let changed = handle_sync(
            &task.repo_path,
            task.auto_pull,
            task.auto_push,
            &task.source_name,
            &ctx.state,
            task.require_signed_commits,
            task.allow_unsigned,
        )
        .await;
        if changed && !task.auto_apply {
            tracing::info!(
                source = %task.source_name,
                "changes detected but auto-apply is disabled — run 'cfgd sync' interactively"
            );
        }
    }
    Ok(())
}

/// The soonest deadline the backup branch must wake for — a fire or a
/// re-resolve — or a far park when there is neither.
pub(super) fn next_backup_deadline(backup_timers: &BackupTimers) -> Instant {
    backup_timers
        .next_deadline()
        .unwrap_or_else(|| Instant::now() + BACKUP_IDLE_PARK)
}

/// Re-resolve the timer set in place.
///
/// Returns the reload summary when the set was actually swapped, and `None`
/// when the resolution was degraded or failed and the RUNNING set was kept
/// (with a retry armed). Callers decide whether to report to the console:
/// SIGHUP does, because a human asked; the automatic retry stays in `tracing`
/// so a persistently broken source does not narrate itself every five minutes.
fn refresh_backup_timers(
    ctx: &DaemonLoopContext,
    cfg: &CfgdConfig,
    timers: &mut BackupTimers,
    now: Instant,
) -> Option<BackupReloadSummary> {
    match resolve_backup_tasks(
        cfg,
        &ctx.config_path,
        ctx.profile_override.as_deref(),
        &ctx.printer,
        ctx.scope,
        ctx.state_dir_override.as_deref(),
        now,
    ) {
        Ok(resolved) => {
            let degraded = resolved.degraded;
            let summary = timers.apply_resolved(resolved, now);
            if degraded.is_some() {
                tracing::warn!(
                    adopted = summary.is_some(),
                    "backup timers: source composition unavailable — retrying"
                );
            }
            summary
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "backup timers: profile resolution failed — keeping the running timer set, retrying"
            );
            timers.arm_retry(now, DegradedReason::ProfileUnresolved);
            None
        }
    }
}

/// Run every scheduled backup whose deadline has passed, and re-resolve the set
/// first when a degraded resolution armed a retry.
///
/// **Overlap**: the daemon's select loop processes one tick at a time and this
/// handler awaits each run, so a unit's next fire is not even evaluated while
/// its own run is in flight. That is the loop's half of the guard; the engine
/// holds the other half (a per-unit lock inside `run_backup`), which is what
/// also excludes a hand-run or an apply happening at the same moment. What
/// neither decides on its own is what happens to the fires that elapsed during
/// a long run, so `BackupTask::advance` drops them (logging how many) rather
/// than queueing a burst of catch-up runs against a source that has not changed
/// meanwhile.
pub(super) async fn handle_backup_tick(
    ctx: &DaemonLoopContext,
    backup_timers: &mut BackupTimers,
) -> Result<()> {
    let now = Instant::now();
    if backup_timers.retry_due(now) {
        match config::load_config(&ctx.config_path) {
            Ok(cfg) => {
                if refresh_backup_timers(ctx, &cfg, backup_timers, now).is_some() {
                    let (role, note) = backup_timers.reload_line_qualifier();
                    let count = backup_timers.len();
                    let message = if count == 0 {
                        format!("Backup schedule resolved: no units configured{note}")
                    } else {
                        format!("Backup schedules restored: {count} scheduled{note}")
                    };
                    ctx.printer.status_simple(role, message);
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "backup timers: config reload failed — keeping the running timer set, retrying"
                );
                backup_timers.arm_retry(now, DegradedReason::ConfigUnreadable);
            }
        }
    }

    let due = backup_timers.take_due(now);
    if due.is_empty() {
        return Ok(());
    }

    let config_dir = ctx
        .config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    // Startup materialized this from scope precisely so every downstream site
    // agrees on one path; `None` here means that derivation already failed, and
    // re-deriving it would just fail the same way.
    let Some(state_dir) = ctx.state_dir_override.clone() else {
        tracing::error!("backup: no state directory resolved at startup — runs skipped");
        return Ok(());
    };

    for (profile_name, spec) in due {
        tracing::info!(backup = %spec.name, "scheduled backup tick");
        let printer = Arc::clone(&ctx.printer);
        let abort = Arc::clone(&ctx.abort);
        let config_dir = config_dir.clone();
        let state_dir = state_dir.clone();
        crate::spawn_blocking_with_test_home(move || {
            run_scheduled_backup(
                &spec,
                &config_dir,
                &profile_name,
                &state_dir,
                &printer,
                &abort,
            );
        })
        .await
        .map_err(|e| DaemonError::WatchError {
            message: format!("backup task failed: {}", e),
        })?;
    }
    Ok(())
}

pub(super) async fn handle_version_check_tick(ctx: &DaemonLoopContext) -> Result<()> {
    tracing::trace!("version check tick");
    // Load the live config so the check honors `spec.update.policy`. A load
    // failure degrades to the default policy (Prompt → Notify in the daemon's
    // non-interactive context) rather than skipping the check entirely.
    let update_cfg = config::load_config(&ctx.config_path)
        .ok()
        .and_then(|c| c.spec.update)
        .unwrap_or_default();
    handle_version_check(&update_cfg, &ctx.state, &ctx.notifier, &ctx.cfgd_version).await;
    Ok(())
}

pub(super) async fn handle_compliance_tick(ctx: &DaemonLoopContext) -> Result<()> {
    tracing::trace!("compliance snapshot tick");
    if let Some(ref cc) = ctx.compliance_config {
        let cp = ctx.config_path.clone();
        let po = ctx.profile_override.clone();
        let hk = Arc::clone(&ctx.hooks);
        let cc2 = cc.clone();
        let sd = ctx.state_dir_override.clone();
        let scope = ctx.scope;
        crate::spawn_blocking_with_test_home(move || {
            handle_compliance_snapshot(&cp, po.as_deref(), &*hk, &cc2, sd.as_deref(), scope);
        })
        .await
        .map_err(|e| DaemonError::WatchError {
            message: format!("compliance snapshot task failed: {}", e),
        })?;
    }
    Ok(())
}

/// Apply a SIGHUP-driven config reload.
///
/// **Scope (intentional)**: SIGHUP refreshes ONLY the reconcile and sync timer
/// intervals and the scheduled-backup timer set. All other daemon-config fields
/// (profile, sources list, `drift_policy`, `notify_on_drift`,
/// `on_change_reconcile`, compliance config, packages, files) require a daemon
/// **restart** to take effect, because they are baked into
/// [`DaemonLoopContext`] / per-source watchers at startup and changing them
/// in-flight would require tearing down + rebuilding the file watcher set, the
/// notifier, and the source-status state machine — work that is not implemented
/// and would be racy with in-flight reconciles.
///
/// `spec.backups[]` is inside the scope because a backup timer owns no
/// long-lived machinery: rebuilding the set is a pure swap of deadlines, and
/// unchanged units keep the deadline they had, so a reload never restarts the
/// clock on a backup the user did not touch.
///
/// The swap is all-or-nothing: a reload that cannot fully resolve the config
/// (a profile typo, a source cache mid-rewrite) keeps the RUNNING timer set and
/// arms a retry, so one SIGHUP over a transient failure can never retire a
/// working schedule until a restart.
///
/// This scope is intentional; a user editing the out-of-scope fields and
/// sending SIGHUP must restart the daemon. The startup banner and the
/// reload-completion line both surface this explicitly so it isn't a silent
/// surprise.
///
/// Split out from the select! branch so the parsing + atomic-update logic is
/// directly testable without spawning signal handlers.
pub(super) fn apply_sighup_reload(
    ctx: &DaemonLoopContext,
    reconcile_secs: &AtomicU64,
    sync_secs: &AtomicU64,
    backup_timers: &mut BackupTimers,
) {
    let printer = &ctx.printer;
    printer.status_simple(
        Role::Info,
        "Reloading configuration (SIGHUP) — timer intervals and backup schedules only; other fields require restart",
    );
    match config::load_config(&ctx.config_path) {
        Ok(new_cfg) => {
            let (new_reconcile, new_sync) = compute_sighup_intervals(&new_cfg);
            let mut changed = Vec::new();
            if let Some(d) = new_reconcile {
                reconcile_secs.store(d.as_secs(), Ordering::Relaxed);
                changed.push(format!("reconcile={:?}", d));
            }
            if let Some(d) = new_sync {
                sync_secs.store(d.as_secs(), Ordering::Relaxed);
                changed.push(format!("sync={:?}", d));
            }

            let backups = refresh_backup_timers(ctx, &new_cfg, backup_timers, Instant::now());
            // A refusal only matters when there was a running set to protect.
            // With none, "kept 0 schedules" would be an alarming way to say
            // that a machine which declares no backups still declares none.
            let refused = backups.is_none() && backup_timers.len() > 0;
            let reloaded = backups.as_ref().is_some_and(|b| !b.is_empty());

            if !refused && !reloaded && changed.is_empty() {
                printer.status_simple(
                    Role::Info,
                    "Config validated; no timer changes detected (other field changes require restart)",
                );
                return;
            }
            if refused {
                // Said out loud because the alternative — reporting "0 removed"
                // — reads as "your edit had no effect" when what actually
                // happened is that the daemon refused to act on half the inputs.
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "Backup schedules NOT reloaded: config did not fully resolve — keeping the {} running schedule(s), retrying automatically",
                        backup_timers.len()
                    ),
                );
            }
            if !changed.is_empty() {
                printer.status_simple(
                    Role::Ok,
                    format!(
                        "Timer intervals reloaded: {} (other field changes require restart)",
                        changed.join(", ")
                    ),
                );
            }
            if let Some(b) = backups.filter(|b| !b.is_empty()) {
                let (role, note) = backup_timers.reload_line_qualifier();
                printer.status_simple(
                    role,
                    format!(
                        "Backup schedules reloaded: {} added, {} removed, {} rescheduled{note}",
                        b.added, b.removed, b.rescheduled
                    ),
                );
            }
        }
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "Config reload failed: {}",
                    crate::output::collapse_to_subject_line(&e),
                ),
            );
        }
    }
}

/// Compute the (reconcile, sync) intervals from a freshly-loaded config.
/// Returns `None` for any field that the config does not specify, so the
/// caller can leave existing intervals in place.
pub(super) fn compute_sighup_intervals(cfg: &CfgdConfig) -> (Option<Duration>, Option<Duration>) {
    let reconcile = cfg
        .spec
        .daemon
        .as_ref()
        .and_then(|d| d.reconcile.as_ref())
        .map(|rc| parse_duration_or_default(&rc.interval));
    let sync = cfg
        .spec
        .daemon
        .as_ref()
        .and_then(|d| d.sync.as_ref())
        .map(|sc| parse_duration_or_default(&sc.interval));
    (reconcile, sync)
}

/// Build the initial `SourceStatus` rows for each configured source. Extracted
/// for testability; consumed by `run_daemon` to seed `DaemonState.sources`.
pub(super) fn build_initial_source_status(sources: &[config::SourceSpec]) -> Vec<SourceStatus> {
    sources
        .iter()
        .map(|source| SourceStatus {
            name: source.name.clone(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 0,
            status: "active".to_string(),
        })
        .collect()
}
