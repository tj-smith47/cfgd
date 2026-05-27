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
    /// never touches `~/.local/share/cfgd/`.
    pub state_dir_override: Option<PathBuf>,
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
pub(super) async fn run_daemon_loop(
    ctx: DaemonLoopContext,
    mut triggers: DaemonTriggers,
    mut reconcile_tasks: Vec<ReconcileTask>,
    mut sync_tasks: Vec<SyncTask>,
    reconcile_interval_secs: Arc<AtomicU64>,
    sync_interval_secs: Arc<AtomicU64>,
) -> Result<()> {
    let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
    let debounce = Duration::from_millis(DEBOUNCE_MS);

    loop {
        tokio::select! {
            Some(path) = triggers.file_rx.recv() => {
                if let Err(e) = handle_file_change_tick(&ctx, &mut last_change, debounce, path).await {
                    tracing::error!(error = %e, tick = "file_change", "daemon tick failed; loop continues");
                }
            }

            Some(()) = triggers.reconcile_rx.recv() => {
                if let Err(e) = handle_reconcile_tick(&ctx, &mut reconcile_tasks).await {
                    tracing::error!(error = %e, tick = "reconcile", "daemon tick failed; loop continues");
                }
            }

            Some(()) = triggers.sync_rx.recv() => {
                if let Err(e) = handle_sync_tick(&ctx, &mut sync_tasks).await {
                    tracing::error!(error = %e, tick = "sync", "daemon tick failed; loop continues");
                }
            }

            Some(()) = triggers.version_check_rx.recv() => {
                if let Err(e) = handle_version_check_tick(&ctx).await {
                    tracing::error!(error = %e, tick = "version_check", "daemon tick failed; loop continues");
                }
            }

            Some(()) = triggers.compliance_rx.recv() => {
                if let Err(e) = handle_compliance_tick(&ctx).await {
                    tracing::error!(error = %e, tick = "compliance", "daemon tick failed; loop continues");
                }
            }

            Some(()) = triggers.sighup_rx.recv() => {
                apply_sighup_reload(
                    &ctx.config_path,
                    &reconcile_interval_secs,
                    &sync_interval_secs,
                    &ctx.printer,
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

    let drift_recorded = super::drift::record_file_drift(&path);
    if drift_recorded {
        {
            let mut st = ctx.state.lock().await;
            st.drift_count += 1;
            if let Some(source) = st.sources.first_mut() {
                source.drift_count += 1;
            }
        }

        if ctx.notify_on_drift {
            ctx.notifier.notify(
                "cfgd: drift detected",
                &format!("File changed: {}", path.posix()),
            );
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
        let printer = Arc::clone(&ctx.printer);
        tokio::task::spawn_blocking(move || {
            handle_reconcile(
                &cp,
                po.as_deref(),
                ReconcileCtx {
                    state: &st,
                    notifier: &nt,
                    notify_on_drift: notify_drift,
                    hooks: &*hk,
                    state_dir_override: state_dir.as_deref(),
                    printer: &printer,
                    module_filter: None,
                    auto_apply_override: None,
                    drift_policy_override: None,
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
            let printer = Arc::clone(&ctx.printer);
            tokio::task::spawn_blocking(move || {
                handle_reconcile(
                    &cp,
                    po.as_deref(),
                    ReconcileCtx {
                        state: &st,
                        notifier: &nt,
                        notify_on_drift: notify_drift,
                        hooks: &*hk,
                        state_dir_override: state_dir.as_deref(),
                        printer: &printer,
                        module_filter: None,
                        auto_apply_override: None,
                        drift_policy_override: None,
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
            let printer = Arc::clone(&ctx.printer);
            let module_name = entity_name.clone();
            tokio::task::spawn_blocking(move || {
                handle_reconcile(
                    &cp,
                    po.as_deref(),
                    ReconcileCtx {
                        state: &st,
                        notifier: &nt,
                        notify_on_drift: notify_drift,
                        hooks: &*hk,
                        state_dir_override: state_dir.as_deref(),
                        printer: &printer,
                        module_filter: Some(&module_name),
                        auto_apply_override: Some(task_auto_apply),
                        drift_policy_override: Some(task_drift_policy),
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

pub(super) async fn handle_version_check_tick(ctx: &DaemonLoopContext) -> Result<()> {
    tracing::trace!("version check tick");
    handle_version_check(&ctx.state, &ctx.notifier).await;
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
        tokio::task::spawn_blocking(move || {
            handle_compliance_snapshot(&cp, po.as_deref(), &*hk, &cc2, sd.as_deref());
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
/// intervals. All other daemon-config fields (profile, sources list,
/// `drift_policy`, `notify_on_drift`, `on_change_reconcile`, compliance config,
/// packages, files) require a daemon **restart** to take effect, because they
/// are baked into [`DaemonLoopContext`] / per-source watchers at startup and
/// changing them in-flight would require tearing down + rebuilding the file
/// watcher set, the notifier, and the source-status state machine — work that
/// is not implemented and would be racy with in-flight reconciles.
///
/// This scope is intentional; a user editing those fields and sending SIGHUP
/// must restart the daemon. The startup banner and the reload-completion line
/// both surface this explicitly so it isn't a silent surprise.
///
/// Split out from the select! branch so the parsing + atomic-update logic is
/// directly testable without spawning signal handlers.
pub(super) fn apply_sighup_reload(
    config_path: &Path,
    reconcile_secs: &AtomicU64,
    sync_secs: &AtomicU64,
    printer: &Printer,
) {
    printer.status_simple(
        Role::Info,
        "Reloading configuration (SIGHUP) — timer intervals only; other fields require restart",
    );
    match config::load_config(config_path) {
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
            if changed.is_empty() {
                printer.status_simple(
                    Role::Info,
                    "Config validated; no timer changes detected (other field changes require restart)",
                );
            } else {
                printer.status_simple(
                    Role::Ok,
                    format!(
                        "Timer intervals reloaded: {} (other field changes require restart)",
                        changed.join(", ")
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
