// Daemon — file watchers, reconciliation loop, sync, notifications, health endpoint, service management
//
// Locking convention (enforced by code review, not the compiler):
//   * `DaemonState` lives behind `Arc<tokio::sync::Mutex<_>>`.
//   * Every `.lock().await` MUST drop the guard before any `.await` on
//     network / filesystem / subprocess I/O. The pattern is: acquire,
//     clone out the fields needed, drop the guard, then do work.
//   * Holding the lock across an await would serialize the daemon onto
//     one in-flight request and invites deadlock when handlers call
//     each other. All 19 current `.lock().await` sites follow this rule;
//     new sites must too.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write as IoWrite};
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc};

use crate::config::{
    self, AutoApplyPolicyConfig, CfgdConfig, MergedProfile, NotifyMethod, OriginType, PolicyAction,
    ResolvedProfile,
};
use crate::errors::{DaemonError, Result};
use crate::output::{Printer, Verbosity};
use crate::providers::{FileAction, PackageAction, PackageManager, ProviderRegistry};
use crate::state::StateStore;

/// Trait for binary-specific operations the daemon needs.
/// The workstation binary (`cfgd`) implements this with concrete provider types.
pub trait DaemonHooks: Send + Sync {
    /// Build a ProviderRegistry with all available providers for this binary.
    fn build_registry(&self, config: &CfgdConfig) -> ProviderRegistry;

    /// Plan file actions by comparing desired vs actual state.
    fn plan_files(&self, config_dir: &Path, resolved: &ResolvedProfile) -> Result<Vec<FileAction>>;

    /// Plan package actions by comparing installed vs desired.
    fn plan_packages(
        &self,
        profile: &MergedProfile,
        managers: &[&dyn PackageManager],
    ) -> Result<Vec<PackageAction>>;

    /// Extend the registry with custom (user-defined) package managers from the profile.
    fn extend_registry_custom_managers(
        &self,
        registry: &mut ProviderRegistry,
        packages: &config::PackagesSpec,
    );

    /// Expand tilde (~) to home directory in a path.
    fn expand_tilde(&self, path: &Path) -> PathBuf;
}

const DEBOUNCE_MS: u64 = 500;
#[cfg(unix)]
const DEFAULT_IPC_PATH: &str = "/tmp/cfgd.sock";
#[cfg(windows)]
const DEFAULT_IPC_PATH: &str = r"\\.\pipe\cfgd";
const DEFAULT_RECONCILE_SECS: u64 = 300; // 5m
const DEFAULT_SYNC_SECS: u64 = 300; // 5m
#[cfg(unix)]
const LAUNCHD_LABEL: &str = "com.cfgd.daemon";
#[cfg(unix)]
const LAUNCHD_AGENTS_DIR: &str = "Library/LaunchAgents";
#[cfg(unix)]
const SYSTEMD_USER_DIR: &str = ".config/systemd/user";

// --- Sync Task ---

pub(super) struct SyncTask {
    source_name: String,
    repo_path: PathBuf,
    auto_pull: bool,
    auto_push: bool,
    auto_apply: bool,
    interval: Duration,
    last_synced: Option<Instant>,
    /// When true, verify commit signatures after pull (source requires it).
    require_signed_commits: bool,
    /// When true, skip signature verification (global allow-unsigned).
    allow_unsigned: bool,
}

// --- Reconcile Task (per-module or default) ---

pub(super) struct ReconcileTask {
    /// Module name, or `"__default__"` for non-patched resources.
    entity: String,
    interval: Duration,
    auto_apply: bool,
    drift_policy: config::DriftPolicy,
    last_reconciled: Option<Instant>,
}

// --- Per-source status ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub name: String,
    pub last_sync: Option<String>,
    pub last_reconcile: Option<String>,
    pub drift_count: u32,
    pub status: String,
}

// --- Shared Daemon State ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatusResponse {
    pub running: bool,
    pub pid: u32,
    pub uptime_secs: u64,
    pub last_reconcile: Option<String>,
    pub last_sync: Option<String>,
    pub drift_count: u32,
    pub sources: Vec<SourceStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_available: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub module_reconcile: Vec<ModuleReconcileStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleReconcileStatus {
    pub name: String,
    pub interval: String,
    pub auto_apply: bool,
    pub drift_policy: String,
    pub last_reconcile: Option<String>,
}

pub(super) struct DaemonState {
    started_at: Instant,
    last_reconcile: Option<String>,
    last_sync: Option<String>,
    drift_count: u32,
    sources: Vec<SourceStatus>,
    update_available: Option<String>,
    module_last_reconcile: HashMap<String, String>,
    // State DB path the `/drift` endpoint should read. `None` means "no store"
    // (used in tests so endpoint returns empty events without touching the
    // user's real `~/.local/share/cfgd/state.db`).
    store_path: Option<PathBuf>,
}

impl DaemonState {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            last_reconcile: None,
            last_sync: None,
            drift_count: 0,
            sources: vec![SourceStatus {
                name: "local".to_string(),
                last_sync: None,
                last_reconcile: None,
                drift_count: 0,
                status: "active".to_string(),
            }],
            update_available: None,
            module_last_reconcile: HashMap::new(),
            store_path: None,
        }
    }

    fn with_store_path(mut self, path: PathBuf) -> Self {
        self.store_path = Some(path);
        self
    }

    fn to_response(&self) -> DaemonStatusResponse {
        DaemonStatusResponse {
            running: true,
            pid: std::process::id(),
            uptime_secs: self.started_at.elapsed().as_secs(),
            last_reconcile: self.last_reconcile.clone(),
            last_sync: self.last_sync.clone(),
            drift_count: self.drift_count,
            sources: self.sources.clone(),
            update_available: self.update_available.clone(),
            module_reconcile: vec![],
        }
    }
}

// --- Notifier ---

pub(super) struct Notifier {
    method: NotifyMethod,
    webhook_url: Option<String>,
}

impl Notifier {
    fn new(method: NotifyMethod, webhook_url: Option<String>) -> Self {
        Self {
            method,
            webhook_url,
        }
    }

    fn notify(&self, title: &str, message: &str) {
        match self.method {
            NotifyMethod::Desktop => self.notify_desktop(title, message),
            NotifyMethod::Stdout => self.notify_stdout(title, message),
            NotifyMethod::Webhook => self.notify_webhook(title, message),
        }
    }

    fn notify_desktop(&self, title: &str, message: &str) {
        match notify_rust::Notification::new()
            .summary(title)
            .body(message)
            .appname("cfgd")
            .show()
        {
            Ok(_) => tracing::debug!(title = %title, "desktop notification sent"),
            Err(e) => {
                tracing::warn!(error = %e, "desktop notification failed, falling back to stdout");
                self.notify_stdout(title, message);
            }
        }
    }

    fn notify_stdout(&self, title: &str, message: &str) {
        tracing::info!(title = %title, message = %message, "notification");
    }

    fn notify_webhook(&self, title: &str, message: &str) {
        let Some(ref url) = self.webhook_url else {
            tracing::warn!("webhook notification requested but no webhook-url configured");
            return;
        };

        let payload = serde_json::json!({
            "event": title,
            "message": message,
            "timestamp": crate::utc_now_iso8601(),
            "source": "cfgd",
        });

        let url = url.clone();
        let body = payload.to_string();

        // Run webhook POST via spawn_blocking (uses tokio's bounded threadpool)
        tokio::task::spawn_blocking(move || {
            match crate::http::http_agent(crate::http::HTTP_WEBHOOK_TIMEOUT)
                .post(&url)
                .set("Content-Type", "application/json")
                .send_string(&body)
            {
                Ok(_) => tracing::debug!(url = %url, "webhook notification sent"),
                Err(e) => tracing::warn!(error = %e, "webhook notification failed"),
            }
        });
    }
}
// --- Submodule declarations ---

mod checkin;
mod daemon_config;
mod drift;
mod git;
mod health_ipc;
mod reconcile;
mod service;
mod sync;

#[cfg(test)]
mod tests;

use checkin::*;
use daemon_config::*;
use drift::*;
use git::*;
use health_ipc::*;
use reconcile::*;
#[allow(unused_imports)]
use service::*;
use sync::*;

// --- Public re-exports (preserve crate::daemon::<name> API) ---

pub use git::git_pull_sync;
pub use health_ipc::query_daemon_status;
pub use service::{install_service, run_as_windows_service, uninstall_service};

// --- Main Daemon Entry Point ---

pub async fn run_daemon(
    config_path: PathBuf,
    profile_override: Option<String>,
    printer: Arc<Printer>,
    hooks: Arc<dyn DaemonHooks>,
) -> Result<()> {
    printer.header("Daemon");
    printer.info("Starting cfgd daemon...");

    // Load config to get daemon settings
    let cfg = config::load_config(&config_path)?;
    let daemon_cfg = cfg.spec.daemon.clone().unwrap_or(config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
    });

    // Parse daemon config into resolved values with defaults
    let parsed = parse_daemon_config(&daemon_cfg);
    let reconcile_interval = parsed.reconcile_interval;
    let sync_interval = parsed.sync_interval;
    let auto_pull = parsed.auto_pull;
    let auto_push = parsed.auto_push;
    let on_change_reconcile = parsed.on_change_reconcile;
    let notify_on_drift = parsed.notify_on_drift;

    let notifier = Arc::new(Notifier::new(
        parsed.notify_method.clone(),
        parsed.webhook_url.clone(),
    ));
    let daemon_state = match crate::state::default_state_dir() {
        Ok(dir) => DaemonState::new().with_store_path(dir.join("state.db")),
        Err(e) => {
            tracing::warn!(error = %e, "cannot resolve default state dir; /drift endpoint disabled");
            DaemonState::new()
        }
    };
    let state = Arc::new(Mutex::new(daemon_state));

    // Parse compliance snapshot config
    let compliance_config = cfg.spec.compliance.clone();
    let compliance_interval = compliance_config
        .as_ref()
        .filter(|c| c.enabled)
        .and_then(|c| crate::parse_duration_str(&c.interval).ok());

    // Build sync tasks for local config and each configured source
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let allow_unsigned = cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned);

    let source_cache_dir = crate::sources::SourceManager::default_cache_dir()
        .unwrap_or_else(|_| config_dir.join(".cfgd-sources"));
    let mut sync_tasks = build_sync_tasks(
        &config_dir,
        &parsed,
        &cfg.spec.sources,
        allow_unsigned,
        &source_cache_dir,
        |source_dir| {
            crate::sources::detect_source_manifest(source_dir)
                .ok()
                .flatten()
                .map(|m| m.spec.policy.constraints.require_signed_commits)
        },
    );

    // Initialize per-source status entries
    {
        let mut st = state.lock().await;
        for source in &cfg.spec.sources {
            st.sources.push(SourceStatus {
                name: source.name.clone(),
                last_sync: None,
                last_reconcile: None,
                drift_count: 0,
                status: "active".to_string(),
            });
        }
    }

    // Discover managed file paths for watching
    let managed_paths = discover_managed_paths(&config_path, profile_override.as_deref(), &*hooks);

    // Set up file watcher channel
    let (file_tx, mut file_rx) = mpsc::channel::<PathBuf>(256);
    let _watcher = setup_file_watcher(file_tx, &managed_paths, &config_dir)?;

    // Check for already-running daemon via IPC connectivity
    #[cfg(unix)]
    {
        let socket_path = PathBuf::from(DEFAULT_IPC_PATH);
        if socket_path.exists() {
            if StdUnixStream::connect(&socket_path).is_ok() {
                return Err(DaemonError::AlreadyRunning {
                    pid: std::process::id(),
                }
                .into());
            }
            // Stale socket from crashed daemon — remove it
            let _ = std::fs::remove_file(&socket_path);
        }
    }
    #[cfg(windows)]
    {
        if connect_daemon_ipc().is_some() {
            return Err(DaemonError::AlreadyRunning {
                pid: std::process::id(),
            }
            .into());
        }
    }

    // Start health server
    let health_state = Arc::clone(&state);
    let ipc_path = DEFAULT_IPC_PATH.to_string();
    let health_handle = tokio::spawn(async move {
        if let Err(e) = run_health_server(&ipc_path, health_state).await {
            tracing::error!(error = %e, "health server error");
        }
    });

    let mut intervals = vec![format!("reconcile={}s", reconcile_interval.as_secs())];
    if auto_pull || auto_push {
        intervals.push(format!(
            "sync={}s (pull={}, push={})",
            sync_interval.as_secs(),
            auto_pull,
            auto_push
        ));
    }
    if let Some(interval) = compliance_interval {
        intervals.push(format!("compliance={}s", interval.as_secs()));
    }
    printer.success(&format!("Health: {}", DEFAULT_IPC_PATH));
    printer.success(&format!("Intervals: {}", intervals.join(", ")));
    printer.info("Daemon running — press Ctrl+C to stop");
    printer.newline();

    // Initial server check-in at startup
    if find_server_url(&cfg).is_some() {
        let startup_cfg = cfg.clone();
        let startup_config_path = config_path.clone();
        let startup_profile_override = profile_override.clone();
        tokio::task::spawn_blocking(move || {
            let config_dir = startup_config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            let profiles_dir = config_dir.join("profiles");
            let profile_name = match startup_profile_override
                .as_deref()
                .or(startup_cfg.spec.profile.as_deref())
            {
                Some(p) => p,
                None => {
                    tracing::error!("no profile configured — skipping reconciliation");
                    return;
                }
            };
            match config::resolve_profile(profile_name, &profiles_dir) {
                Ok(resolved) => {
                    let changed = try_server_checkin(&startup_cfg, &resolved);
                    if changed {
                        tracing::info!("server reports config changed at startup");
                    }
                    // Consume any pending server config at startup so the first
                    // reconcile tick picks up the changes.
                    match crate::state::load_pending_server_config() {
                        Ok(Some(_pending)) => {
                            tracing::info!(
                                "startup: found pending server config — first reconcile will apply it"
                            );
                            if let Err(e) = crate::state::clear_pending_server_config() {
                                tracing::warn!(error = %e, "startup: failed to clear pending server config");
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "startup: failed to load pending server config");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "startup check-in: failed to resolve profile");
                }
            }
        })
        .await
        .map_err(|e| DaemonError::WatchError {
            message: format!("startup check-in task failed: {}", e),
        })?;
    }

    // Build per-module reconcile tasks from patches
    let profiles_dir = config_dir.join("profiles");
    let profile_name = profile_override
        .as_deref()
        .or(cfg.spec.profile.as_deref())
        .unwrap_or("default");
    let resolved_profile = config::resolve_profile(profile_name, &profiles_dir).ok();
    let profile_chain: Vec<String> = resolved_profile
        .as_ref()
        .map(|r| r.layers.iter().map(|l| l.profile_name.clone()).collect())
        .unwrap_or_else(|| vec![profile_name.to_string()]);
    let chain_refs: Vec<&str> = profile_chain.iter().map(|s| s.as_str()).collect();

    let mut reconcile_tasks = build_reconcile_tasks(
        &daemon_cfg,
        resolved_profile.as_ref(),
        &chain_refs,
        reconcile_interval,
        parsed.auto_apply,
    );

    // Debounce tracking for file events
    let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
    let debounce = Duration::from_millis(DEBOUNCE_MS);

    // Set up timers — use shortest interval across all reconcile and sync tasks
    let shortest_reconcile = reconcile_tasks
        .iter()
        .map(|t| t.interval)
        .min()
        .unwrap_or(reconcile_interval);
    let shortest_sync = sync_tasks
        .iter()
        .map(|t| t.interval)
        .min()
        .unwrap_or(sync_interval);
    let mut reconcile_timer = tokio::time::interval(shortest_reconcile);
    let mut sync_timer = tokio::time::interval(shortest_sync);
    let mut version_check_timer = tokio::time::interval(crate::upgrade::version_check_interval());

    // Compliance snapshot timer — only created when compliance is enabled
    let mut compliance_timer = compliance_interval.map(tokio::time::interval);

    // Unix: set up SIGHUP handler for config reload.
    // On Windows, SIGHUP doesn't exist — recv_sighup() pends forever.
    #[cfg(unix)]
    let mut sighup_signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|e| DaemonError::WatchError {
            message: format!("failed to register SIGHUP handler: {}", e),
        })?;
    #[cfg(not(unix))]
    let mut sighup_signal = ();

    // Unix: set up SIGTERM handler for graceful shutdown.
    // On Windows, shutdown is handled via the Windows Service control manager.
    #[cfg(unix)]
    let mut sigterm_signal =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| {
            DaemonError::WatchError {
                message: format!("failed to register SIGTERM handler: {}", e),
            }
        })?;
    #[cfg(not(unix))]
    let mut sigterm_signal = ();

    // Skip the first immediate tick
    reconcile_timer.tick().await;
    sync_timer.tick().await;
    version_check_timer.tick().await;
    if let Some(ref mut timer) = compliance_timer {
        timer.tick().await;
    }

    loop {
        tokio::select! {
            Some(path) = file_rx.recv() => {
                // Debounce: skip if we saw this path recently
                let now = Instant::now();
                if let Some(last) = last_change.get(&path)
                    && now.duration_since(*last) < debounce
                {
                    continue;
                }
                last_change.insert(path.clone(), now);

                tracing::info!(path = %path.display(), "file changed");

                // Record drift. Mutate state counters under the mutex, then DROP
                // the guard before calling `notifier.notify()` — Desktop notify
                // does a blocking dbus round-trip that otherwise blocks every
                // other daemon task waiting on the state mutex.
                let drift_recorded = record_file_drift(&path);
                if drift_recorded {
                    {
                        let mut st = state.lock().await;
                        st.drift_count += 1;
                        if let Some(source) = st.sources.first_mut() {
                            source.drift_count += 1;
                        }
                    }

                    if notify_on_drift {
                        notifier.notify(
                            "cfgd: drift detected",
                            &format!("File changed: {}", path.display()),
                        );
                    }
                }

                // Optionally reconcile on change
                if on_change_reconcile {
                    let cp = config_path.clone();
                    let po = profile_override.clone();
                    let st = Arc::clone(&state);
                    let nt = Arc::clone(&notifier);
                    let notify_drift = notify_on_drift;
                    let hk = Arc::clone(&hooks);
                    tokio::task::spawn_blocking(move || {
                        handle_reconcile(&cp, po.as_deref(), &st, &nt, notify_drift, &*hk, None);
                    }).await.map_err(|e| DaemonError::WatchError {
                        message: format!("reconcile task failed: {}", e),
                    })?;
                }
            }

            _ = reconcile_timer.tick() => {
                tracing::trace!("reconcile tick");
                let now = Instant::now();

                // Check each reconcile task — only run if its interval has elapsed
                let mut ran_default = false;
                for task in &mut reconcile_tasks {
                    if let Some(last) = task.last_reconciled
                        && now.duration_since(last) < task.interval
                    {
                        continue;
                    }
                    task.last_reconciled = Some(now);

                    if task.entity == "__default__" {
                        ran_default = true;
                        let cp = config_path.clone();
                        let po = profile_override.clone();
                        let st = Arc::clone(&state);
                        let nt = Arc::clone(&notifier);
                        let notify_drift = notify_on_drift;
                        let hk = Arc::clone(&hooks);
                        tokio::task::spawn_blocking(move || {
                            handle_reconcile(&cp, po.as_deref(), &st, &nt, notify_drift, &*hk, None);
                        }).await.map_err(|e| DaemonError::WatchError {
                            message: format!("reconcile task failed: {}", e),
                        })?;
                    } else {
                        // Per-module reconcile — currently records the timestamp;
                        // scoped module reconciliation uses the same handle_reconcile
                        // with --module filtering (future: handle_module_reconcile).
                        let entity_name = task.entity.clone();
                        tracing::info!(
                            module = %entity_name,
                            interval = %task.interval.as_secs(),
                            auto_apply = task.auto_apply,
                            drift_policy = ?task.drift_policy,
                            "per-module reconcile tick"
                        );
                        let rt = tokio::runtime::Handle::current();
                        let st = Arc::clone(&state);
                        let ts = crate::utc_now_iso8601();
                        rt.block_on(async {
                            let mut st = st.lock().await;
                            st.module_last_reconcile
                                .insert(entity_name, ts);
                        });
                    }
                }

                // If the default task didn't run this tick but a module task did,
                // that's expected — module tasks can have shorter intervals.
                if !ran_default {
                    tracing::trace!("default reconcile task not due this tick");
                }
            }

            _ = sync_timer.tick() => {
                tracing::trace!("sync tick");
                let now = Instant::now();
                for task in &mut sync_tasks {
                    // Skip if this source was synced recently (per-source interval)
                    if let Some(last) = task.last_synced
                        && now.duration_since(last) < task.interval
                    {
                        continue;
                    }
                    task.last_synced = Some(now);

                    let st = Arc::clone(&state);
                    let repo = task.repo_path.clone();
                    let pull = task.auto_pull;
                    let push = task.auto_push;
                    let auto_apply = task.auto_apply;
                    let source_name = task.source_name.clone();
                    let require_signed = task.require_signed_commits;
                    let allow_uns = task.allow_unsigned;
                    tokio::task::spawn_blocking(move || {
                        let changed = handle_sync(&repo, pull, push, &source_name, &st, require_signed, allow_uns);
                        if changed && !auto_apply {
                            tracing::info!(
                                source = %source_name,
                                "changes detected but auto-apply is disabled — run 'cfgd sync' interactively"
                            );
                        }
                    }).await.map_err(|e| DaemonError::WatchError {
                        message: format!("sync task failed: {}", e),
                    })?;
                }
            }

            _ = version_check_timer.tick() => {
                tracing::trace!("version check tick");
                let st = Arc::clone(&state);
                let nt = Arc::clone(&notifier);
                tokio::task::spawn_blocking(move || {
                    handle_version_check(&st, &nt);
                }).await.map_err(|e| DaemonError::WatchError {
                    message: format!("version check task failed: {}", e),
                })?;
            }

            _ = async {
                match compliance_timer.as_mut() {
                    Some(timer) => timer.tick().await,
                    None => std::future::pending().await,
                }
            } => {
                tracing::trace!("compliance snapshot tick");
                if let Some(ref cc) = compliance_config {
                    let cp = config_path.clone();
                    let po = profile_override.clone();
                    let hk = Arc::clone(&hooks);
                    let cc2 = cc.clone();
                    tokio::task::spawn_blocking(move || {
                        handle_compliance_snapshot(&cp, po.as_deref(), &*hk, &cc2);
                    }).await.map_err(|e| DaemonError::WatchError {
                        message: format!("compliance snapshot task failed: {}", e),
                    })?;
                }
            }

            // Unix: reload config on SIGHUP (kill -HUP <pid>).
            // On Windows, this branch never fires (recv_sighup pends forever).
            _ = recv_sighup(&mut sighup_signal) => {
                printer.info("Reloading configuration (SIGHUP)...");
                match config::load_config(&config_path) {
                    Ok(new_cfg) => {
                        // Update reconcile/sync timer intervals from new config.
                        // Note: full config reload (modules, packages, etc.) requires
                        // a daemon restart. SIGHUP only hot-reloads timer intervals.
                        let mut changed = Vec::new();
                        if let Some(ref rc) = new_cfg.spec.daemon.as_ref().and_then(|d| d.reconcile.clone()) {
                            let new_interval = parse_duration_or_default(&rc.interval);
                            reconcile_timer = tokio::time::interval(new_interval);
                            reconcile_timer.tick().await; // skip first immediate tick
                            changed.push(format!("reconcile={:?}", new_interval));
                        }
                        if let Some(ref sc) = new_cfg.spec.daemon.as_ref().and_then(|d| d.sync.clone()) {
                            let new_interval = parse_duration_or_default(&sc.interval);
                            sync_timer = tokio::time::interval(new_interval);
                            sync_timer.tick().await;
                            changed.push(format!("sync={:?}", new_interval));
                        }
                        if changed.is_empty() {
                            printer.info("Config validated; no timer changes detected");
                        } else {
                            printer.success(&format!("Timer intervals reloaded: {}", changed.join(", ")));
                        }
                    }
                    Err(e) => {
                        printer.warning(&format!("Config reload failed: {}", e));
                    }
                }
            }

            _ = recv_sigterm(&mut sigterm_signal) => {
                printer.info("Received SIGTERM, shutting down daemon...");
                break;
            }

            _ = tokio::signal::ctrl_c() => {
                printer.newline();
                printer.info("Shutting down daemon...");
                break;
            }
        }
    }

    // Shutdown health server
    health_handle.abort();
    let _ = health_handle.await;
    // Unix: remove socket file. Windows: named pipes are kernel objects, no cleanup needed.
    #[cfg(unix)]
    {
        let socket_path = PathBuf::from(DEFAULT_IPC_PATH);
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    printer.success("Daemon stopped");
    Ok(())
}
// --- Helpers ---

/// Module-local wrapper around [`crate::parse_duration_str`] that returns the
/// daemon's `DEFAULT_RECONCILE_SECS` (5 minutes) fallback when parsing fails.
///
/// Intentional duplication with `cfgd-operator::leader::parse_duration_secs`:
/// the two callers want different fallbacks (daemon reconcile loop default vs.
/// leader-election lease-window default), so a single shared helper with a
/// parameterised default would just push the default decision back to every
/// call site without saving any code. Kept local and documented per
/// dedup-audit S1 (decision: keep + document).
pub(crate) fn parse_duration_or_default(s: &str) -> Duration {
    crate::parse_duration_str(s).unwrap_or(Duration::from_secs(DEFAULT_RECONCILE_SECS))
}

/// Receive a SIGHUP signal on Unix. On non-Unix platforms, pends forever.
#[cfg(unix)]
async fn recv_sighup(signal: &mut tokio::signal::unix::Signal) {
    signal.recv().await;
}

/// Receive a SIGHUP signal on Unix. On non-Unix platforms, pends forever.
#[cfg(not(unix))]
async fn recv_sighup(_signal: &mut ()) {
    std::future::pending::<()>().await;
}

/// Receive a SIGTERM signal on Unix. On non-Unix platforms, pends forever.
#[cfg(unix)]
async fn recv_sigterm(signal: &mut tokio::signal::unix::Signal) {
    signal.recv().await;
}

/// Receive a SIGTERM signal on Unix. On non-Unix platforms, pends forever.
#[cfg(not(unix))]
async fn recv_sigterm(_signal: &mut ()) {
    std::future::pending::<()>().await;
}
