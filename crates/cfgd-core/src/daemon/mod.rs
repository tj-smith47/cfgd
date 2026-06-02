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
use crate::output::{Printer, Role};
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

/// Per-user fallback socket name placed under the resolved runtime directory.
#[cfg(unix)]
const IPC_SOCKET_FILE: &str = "cfgd.sock";

/// Windows IPC endpoint. Named pipes are kernel objects in the
/// `\\.\pipe\` namespace — per-session, not file-system objects — so the
/// per-user-directory dance Unix needs does not apply here.
#[cfg(windows)]
const WINDOWS_PIPE_PATH: &str = r"\\.\pipe\cfgd";

/// Resolve the daemon IPC endpoint when no explicit override is supplied.
///
/// Honors `CFGD_DAEMON_IPC_PATH` first so test harnesses and operators can
/// isolate the socket. Otherwise:
/// - Unix: places `cfgd.sock` under [`crate::default_runtime_dir`], which is
///   `$XDG_RUNTIME_DIR/cfgd` on Linux when available (per-user tmpfs),
///   `$HOME/.cache/cfgd` as the Linux fallback, and
///   `$HOME/Library/Application Support/cfgd` on macOS. World-writable
///   `/tmp` is deliberately avoided — see the v0.4.0 hijack-vector audit.
///   A last-ditch fallback to `/tmp/cfgd.sock` only fires when home
///   resolution fails entirely (no `$HOME`, no override); the bind path
///   later refuses to listen if the parent dir is not owner-only.
/// - Windows: returns the named-pipe path verbatim.
///
/// Used by both the server-side bind (`run_daemon_with`) and the client-side
/// connect (`connect_daemon_ipc`) so the two stay in sync.
pub(crate) fn resolve_default_ipc_path() -> PathBuf {
    if let Some(override_path) = std::env::var_os("CFGD_DAEMON_IPC_PATH") {
        return PathBuf::from(override_path);
    }
    #[cfg(unix)]
    {
        crate::default_runtime_dir()
            .map(|dir| dir.join(IPC_SOCKET_FILE))
            .unwrap_or_else(|| PathBuf::from("/tmp/cfgd.sock"))
    }
    #[cfg(windows)]
    {
        PathBuf::from(WINDOWS_PIPE_PATH)
    }
}
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

    #[cfg(test)]
    pub(super) fn store_path_for_test(&self) -> Option<&Path> {
        self.store_path.as_deref()
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

        let url = url.clone();
        let body = build_webhook_payload(title, message, &crate::utc_now_iso8601());

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
/// Build the JSON payload posted by `Notifier::notify_webhook`. Split out so
/// the schema is testable without spawning a tokio thread or hitting the
/// network. The `timestamp_iso` is injected so tests get deterministic
/// output rather than `utc_now_iso8601()` at call time.
pub(super) fn build_webhook_payload(title: &str, message: &str, timestamp_iso: &str) -> String {
    serde_json::json!({
        "event": title,
        "message": message,
        "timestamp": timestamp_iso,
        "source": "cfgd",
    })
    .to_string()
}

// --- Submodule declarations ---

mod checkin;
mod daemon_config;
mod drift;
mod git;
mod health_ipc;
mod reconcile;
mod runner;
mod service;
mod sync;

#[cfg(test)]
mod tests;

use checkin::*;
use daemon_config::*;
// drift::* exposes record_file_drift{,_to} — the wildcard re-exports them
// for future tick handlers; today only direct `super::record_file_drift`
// call-sites in reconcile.rs use them, so the parent-scope wildcard
// appears unused under -D warnings.
#[allow(unused_imports)]
use drift::*;
use git::*;
use health_ipc::*;
use reconcile::*;
use runner::*;
// service::* contains cfg-gated launchd/systemd/windows wrappers — the parent
// wildcard appears unused on the platform that DOESN'T match its arm. Keep
// the import live across all platforms so the cross-platform call sites
// (install_service/uninstall_service/run_as_windows_service) compile uniformly.
#[allow(unused_imports)]
use service::*;
// sync::* exposes handle_sync / handle_version_check / handle_compliance_snapshot;
// the public re-exports point at them through `pub use`, but the wildcard at
// this scope keeps direct super::handle_* call sites in runner.rs compiling
// even when no other submodule path imports them.
#[allow(unused_imports)]
use sync::*;

// --- Public re-exports (preserve crate::daemon::<name> API) ---

pub use git::git_pull_sync;
pub use health_ipc::query_daemon_status;
pub use service::{install_service, run_as_windows_service, start_service, uninstall_service};

// --- Pre-loop setup (synchronous; pulled out so the SETUP arms are unit-testable) ---

/// Bundle of values built up from config + profile resolution before the
/// daemon loop spawns its watcher, health server, and timer pumps.
///
/// Constructed by [`build_pre_loop_setup`]. Tests can drive that function
/// against tempdir fixtures and assert on the populated fields without the
/// rest of `run_daemon`'s side-effect machinery (mpsc pumps, signal handlers,
/// Unix socket binds, network startup check-ins).
pub(super) struct PreLoopSetup {
    pub cfg: CfgdConfig,
    pub parsed: ParsedDaemonConfig,
    pub notifier: Arc<Notifier>,
    pub compliance_config: Option<config::ComplianceConfig>,
    pub compliance_interval: Option<Duration>,
    pub config_dir: PathBuf,
    pub sync_tasks: Vec<SyncTask>,
    pub initial_source_status: Vec<SourceStatus>,
    pub managed_paths: Vec<PathBuf>,
    pub reconcile_tasks: Vec<ReconcileTask>,
    pub shortest_reconcile: Duration,
    pub shortest_sync: Duration,
    pub server_checkin_url: Option<String>,
}

/// Build everything `run_daemon` needs before it starts spawning tasks.
///
/// This is purely synchronous: config load + profile resolution + pure
/// helpers from `daemon_config`, `checkin`, and `reconcile` submodules. No
/// sockets, no spawned tasks, no network. Production callers use this from
/// `run_daemon`; tests use it to exercise the SETUP arms directly.
pub(super) fn build_pre_loop_setup(
    config_path: &Path,
    profile_override: Option<&str>,
    hooks: &dyn DaemonHooks,
) -> Result<PreLoopSetup> {
    let cfg = config::load_config(config_path)?;
    let daemon_cfg = cfg.spec.daemon.clone().unwrap_or(config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    });
    let parsed = parse_daemon_config(&daemon_cfg);
    let notifier = Arc::new(Notifier::new(
        parsed.notify_method.clone(),
        parsed.webhook_url.clone(),
    ));

    let compliance_config = cfg.spec.compliance.clone();
    let compliance_interval = compliance_config
        .as_ref()
        .filter(|c| c.enabled)
        .and_then(|c| crate::parse_duration_str(&c.interval).ok());

    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let allow_unsigned = cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned);

    let source_cache_dir = crate::sources::SourceManager::default_cache_dir()
        .unwrap_or_else(|_| config_dir.join(".cfgd-sources"));
    let sync_tasks = build_sync_tasks(
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
    let initial_source_status = build_initial_source_status(&cfg.spec.sources);

    let managed_paths = discover_managed_paths(config_path, profile_override, hooks);

    let profiles_dir = config_dir.join("profiles");
    let profile_name = profile_override
        .or(cfg.spec.profile.as_deref())
        .unwrap_or("default");
    let resolved_profile = config::resolve_profile(profile_name, &profiles_dir).ok();
    let profile_chain: Vec<String> = resolved_profile
        .as_ref()
        .map(|r| r.layers.iter().map(|l| l.profile_name.clone()).collect())
        .unwrap_or_else(|| vec![profile_name.to_string()]);
    let chain_refs: Vec<&str> = profile_chain.iter().map(|s| s.as_str()).collect();
    let reconcile_tasks = build_reconcile_tasks(
        &daemon_cfg,
        resolved_profile.as_ref(),
        &chain_refs,
        parsed.reconcile_interval,
        parsed.auto_apply,
    );

    let shortest_reconcile = reconcile_tasks
        .iter()
        .map(|t| t.interval)
        .min()
        .unwrap_or(parsed.reconcile_interval);
    let shortest_sync = sync_tasks
        .iter()
        .map(|t| t.interval)
        .min()
        .unwrap_or(parsed.sync_interval);

    let server_checkin_url = find_server_url(&cfg);

    Ok(PreLoopSetup {
        cfg,
        parsed,
        notifier,
        compliance_config,
        compliance_interval,
        config_dir,
        sync_tasks,
        initial_source_status,
        managed_paths,
        reconcile_tasks,
        shortest_reconcile,
        shortest_sync,
        server_checkin_url,
    })
}

// --- Main Daemon Entry Point ---

pub async fn run_daemon(
    config_path: PathBuf,
    profile_override: Option<String>,
    printer: Arc<Printer>,
    hooks: Arc<dyn DaemonHooks>,
) -> Result<()> {
    run_daemon_with(
        config_path,
        profile_override,
        printer,
        hooks,
        DaemonRunOverrides::default(),
    )
    .await
}

/// Test-shaped knobs for [`run_daemon_with`]. Production callers go through
/// [`run_daemon`] which uses `DaemonRunOverrides::default()` and matches the
/// pre-refactor behaviour byte-for-byte. Tests set the fields they need to
/// bypass real-world side effects:
///
/// * `ipc_path` — point the health socket / already-running check at a
///   tempdir so concurrent tests don't fight over the per-user runtime
///   socket resolved by [`resolve_default_ipc_path`].
/// * `state_dir_override` — redirect both the `DaemonState` store path and
///   the per-tick `handle_reconcile` / `handle_compliance_snapshot` state
///   dir to a tempdir so the real `~/.local/share/cfgd/` is never touched.
/// * `skip_health_server` — don't spawn the HTTP/IPC health server. Useful
///   when a test doesn't need `/healthz` or `/drift` and wants to avoid the
///   socket bind entirely.
/// * `skip_startup_checkin` — even if the parsed config has a Server origin,
///   suppress the startup `try_server_checkin` call. Keeps tests offline.
/// * `external_triggers` — when supplied, the function bypasses all
///   real-world trigger sources (file_watcher, interval pumps, SIGHUP /
///   SIGINT / SIGTERM handlers) and drives the loop entirely from the
///   provided receivers. The test owns the matching senders and pushes
///   events to drive specific arms in `run_daemon_loop`.
#[derive(Default)]
pub(super) struct DaemonRunOverrides {
    pub ipc_path: Option<PathBuf>,
    pub state_dir_override: Option<PathBuf>,
    pub skip_health_server: bool,
    pub skip_startup_checkin: bool,
    pub(in crate::daemon) external_triggers: Option<DaemonTriggers>,
}

/// Bundle of trigger receivers + the task handles that feed them. Production
/// callers build this from spawned pumps + signal handlers; tests build it
/// from externally-owned senders with `pump` / `shutdown_task` fields left
/// `None`. Lives in `run_daemon_with` only — not exposed.
struct TriggerSetup {
    triggers: DaemonTriggers,
    reconcile_pump: Option<tokio::task::JoinHandle<()>>,
    sync_pump: Option<tokio::task::JoinHandle<()>>,
    version_check_pump: Option<tokio::task::JoinHandle<()>>,
    compliance_pump: Option<tokio::task::JoinHandle<()>>,
    sighup_pump: Option<tokio::task::JoinHandle<()>>,
    shutdown_task: Option<tokio::task::JoinHandle<()>>,
}

pub(super) async fn run_daemon_with(
    config_path: PathBuf,
    profile_override: Option<String>,
    printer: Arc<Printer>,
    hooks: Arc<dyn DaemonHooks>,
    overrides: DaemonRunOverrides,
) -> Result<()> {
    printer.heading("Daemon");
    printer.status_simple(Role::Info, "Starting cfgd daemon...");

    let setup = build_pre_loop_setup(&config_path, profile_override.as_deref(), &*hooks)?;

    let (daemon_state, state_dir_warning) =
        init_daemon_state_with_warning(overrides.state_dir_override.as_deref());
    if let Some(msg) = state_dir_warning {
        printer.status_simple(Role::Warn, msg);
    }
    let state = Arc::new(Mutex::new(daemon_state));

    // Initialize per-source status entries
    {
        let mut st = state.lock().await;
        st.sources.extend(setup.initial_source_status.clone());
    }

    // External-triggers mode supplies its own file_rx; production wires up a
    // notify-based watcher and pushes via file_tx → file_rx.
    let using_external_triggers = overrides.external_triggers.is_some();
    let (file_rx_for_triggers, _watcher_handle): (
        Option<mpsc::Receiver<PathBuf>>,
        Option<notify::RecommendedWatcher>,
    ) = if using_external_triggers {
        (None, None)
    } else {
        let (file_tx, file_rx) = mpsc::channel::<PathBuf>(256);
        let watcher = setup_file_watcher(file_tx, &setup.managed_paths, &setup.config_dir)?;
        (Some(file_rx), Some(watcher))
    };

    let ipc_path = overrides
        .ipc_path
        .clone()
        .unwrap_or_else(resolve_default_ipc_path);
    check_already_running(&ipc_path)?;

    // Start health server (skippable in tests that don't need /healthz).
    let health_handle = if overrides.skip_health_server {
        None
    } else {
        let health_state = Arc::clone(&state);
        let health_ipc_path = ipc_path.to_string_lossy().to_string();
        Some(tokio::spawn(async move {
            if let Err(e) = run_health_server(&health_ipc_path, health_state).await {
                tracing::error!(error = %e, "health server error");
            }
        }))
    };

    let intervals = format_interval_lines(&setup.parsed, setup.compliance_interval);
    print_startup_banner(&printer, &intervals, &ipc_path.to_string_lossy());

    // Initial server check-in at startup (skippable for offline tests).
    if setup.server_checkin_url.is_some() && !overrides.skip_startup_checkin {
        let startup_cfg = setup.cfg.clone();
        let startup_config_path = config_path.clone();
        let startup_profile_override = profile_override.clone();
        tokio::task::spawn_blocking(move || {
            run_startup_checkin_blocking(
                &startup_config_path,
                startup_profile_override.as_deref(),
                &startup_cfg,
            );
        })
        .await
        .map_err(|e| DaemonError::WatchError {
            message: format!("startup check-in task failed: {}", e),
        })?;
    }

    // Shared atomics: SIGHUP updates these so pump tasks pick up the new
    // cadence on the next tick. (See `runner::apply_sighup_reload`.)
    let reconcile_secs = Arc::new(std::sync::atomic::AtomicU64::new(
        setup.shortest_reconcile.as_secs(),
    ));
    let sync_secs = Arc::new(std::sync::atomic::AtomicU64::new(
        setup.shortest_sync.as_secs(),
    ));

    // Build the triggers + spawn the production pumps/signal handlers, OR
    // adopt the externally-supplied triggers verbatim. The cleanup path at
    // the bottom only aborts what was actually spawned.
    let TriggerSetup {
        triggers,
        reconcile_pump,
        sync_pump,
        version_check_pump,
        compliance_pump,
        sighup_pump,
        shutdown_task,
    } = if let Some(t) = overrides.external_triggers {
        TriggerSetup {
            triggers: t,
            reconcile_pump: None,
            sync_pump: None,
            version_check_pump: None,
            compliance_pump: None,
            sighup_pump: None,
            shutdown_task: None,
        }
    } else {
        let (reconcile_tx, reconcile_rx) = mpsc::channel::<()>(8);
        let (sync_tx, sync_rx) = mpsc::channel::<()>(8);
        let (version_check_tx, version_check_rx) = mpsc::channel::<()>(8);
        let (compliance_tx, compliance_rx) = mpsc::channel::<()>(8);
        let (sighup_tx, sighup_rx) = mpsc::channel::<()>(8);

        let reconcile_pump = spawn_interval_pump(Arc::clone(&reconcile_secs), reconcile_tx);
        let sync_pump = spawn_interval_pump(Arc::clone(&sync_secs), sync_tx);

        let version_check_secs = Arc::new(std::sync::atomic::AtomicU64::new(
            crate::upgrade::version_check_interval().as_secs(),
        ));
        let version_check_pump = spawn_interval_pump(version_check_secs, version_check_tx);

        let compliance_pump = setup.compliance_interval.map(|d| {
            let secs = Arc::new(std::sync::atomic::AtomicU64::new(d.as_secs()));
            spawn_interval_pump(secs, compliance_tx)
        });

        #[cfg(unix)]
        let sighup_pump = Some(spawn_sighup_pump(sighup_tx)?);
        #[cfg(not(unix))]
        let sighup_pump: Option<tokio::task::JoinHandle<()>> = {
            let _ = sighup_tx; // suppress unused warning on Windows
            None
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown_printer = Arc::clone(&printer);
        let shutdown_task = tokio::spawn(async move {
            wait_for_shutdown(shutdown_printer).await;
            let _ = shutdown_tx.send(());
        });

        let file_rx = file_rx_for_triggers.ok_or_else(|| DaemonError::WatchError {
            message: "internal: production path did not initialise file watcher".to_string(),
        })?;
        TriggerSetup {
            triggers: DaemonTriggers {
                file_rx,
                reconcile_rx,
                sync_rx,
                version_check_rx,
                compliance_rx,
                sighup_rx,
                shutdown_rx,
            },
            reconcile_pump: Some(reconcile_pump),
            sync_pump: Some(sync_pump),
            version_check_pump: Some(version_check_pump),
            compliance_pump,
            sighup_pump,
            shutdown_task: Some(shutdown_task),
        }
    };

    let ctx = DaemonLoopContext {
        state: Arc::clone(&state),
        hooks: Arc::clone(&hooks),
        notifier: Arc::clone(&setup.notifier),
        config_path: config_path.clone(),
        profile_override: profile_override.clone(),
        on_change_reconcile: setup.parsed.on_change_reconcile,
        notify_on_drift: setup.parsed.notify_on_drift,
        compliance_config: setup.compliance_config.clone(),
        printer: Arc::clone(&printer),
        state_dir_override: overrides.state_dir_override.clone(),
    };

    let loop_result = run_daemon_loop(
        ctx,
        triggers,
        setup.reconcile_tasks,
        setup.sync_tasks,
        reconcile_secs,
        sync_secs,
    )
    .await;

    // Shut down whatever the trigger-builder block actually spawned.
    if let Some(h) = reconcile_pump {
        h.abort();
    }
    if let Some(h) = sync_pump {
        h.abort();
    }
    if let Some(h) = version_check_pump {
        h.abort();
    }
    if let Some(h) = compliance_pump {
        h.abort();
    }
    if let Some(h) = sighup_pump {
        h.abort();
    }
    if let Some(h) = shutdown_task {
        h.abort();
    }

    // Shutdown health server (only present when not skipped).
    if let Some(h) = health_handle {
        h.abort();
        // Drain the cancellation; the JoinError is always Cancelled here
        // (we just sent abort), nothing actionable to surface.
        let _ = h.await;
    }
    cleanup_ipc_socket(&ipc_path);

    printer.status_simple(Role::Ok, "Daemon stopped");
    loop_result
}

/// Initialize a fresh `DaemonState`, attaching the state-DB path when one can
/// be resolved. When the platform default state dir is unavailable, the
/// returned state has no store path (the `/drift` IPC endpoint will return
/// empty events rather than crash). The `override_dir` parameter exists for
/// tests: passing `Some(dir)` skips the platform lookup entirely.
///
/// Test-only convenience that drops the warning string —
/// `init_daemon_state_with_warning` is the one used by `run_daemon_with`.
#[cfg(test)]
pub(super) fn init_daemon_state(override_dir: Option<&Path>) -> DaemonState {
    init_daemon_state_with_warning(override_dir).0
}

/// Like [`init_daemon_state`] but also returns a printer-facing warning
/// message when the platform default state dir resolution fails — callers
/// can surface it in the startup banner so operators aren't dependent on
/// catching the `tracing::warn!` line.
pub(super) fn init_daemon_state_with_warning(
    override_dir: Option<&Path>,
) -> (DaemonState, Option<String>) {
    let dir_result = override_dir
        .map(|d| Ok(d.to_path_buf()))
        .unwrap_or_else(crate::state::default_state_dir);
    match dir_result {
        Ok(dir) => (
            DaemonState::new().with_store_path(dir.join(crate::state::STATE_DB_FILENAME)),
            None,
        ),
        Err(e) => {
            tracing::warn!(error = %e, "cannot resolve default state dir; /drift endpoint disabled");
            let banner = format!("Drift endpoint disabled: cannot resolve default state dir ({e})");
            (DaemonState::new(), Some(banner))
        }
    }
}

/// Verify no other cfgd daemon is reachable via the IPC endpoint. Returns
/// `Err(AlreadyRunning)` if a connect succeeds; clears a stale socket file
/// (Unix) otherwise. On Windows, falls back to the shared
/// `connect_daemon_ipc()` probe and ignores `_ipc_path` — named pipes are
/// kernel objects with no on-disk cleanup.
pub(super) fn check_already_running(_ipc_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        if _ipc_path.exists() {
            if StdUnixStream::connect(_ipc_path).is_ok() {
                return Err(DaemonError::AlreadyRunning {
                    pid: std::process::id(),
                }
                .into());
            }
            // Stale socket from crashed daemon — remove it
            let _ = std::fs::remove_file(_ipc_path);
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
    Ok(())
}

/// Build the "Intervals: ..." line components for the startup banner. Returns
/// a vector of `key=value` segments the printer joins with `, `. Sync and
/// compliance lines are conditional; reconcile is always present.
pub(super) fn format_interval_lines(
    parsed: &ParsedDaemonConfig,
    compliance_interval: Option<Duration>,
) -> Vec<String> {
    let mut intervals = vec![format!(
        "reconcile={}s",
        parsed.reconcile_interval.as_secs()
    )];
    if parsed.auto_pull || parsed.auto_push {
        intervals.push(format!(
            "sync={}s (pull={}, push={})",
            parsed.sync_interval.as_secs(),
            parsed.auto_pull,
            parsed.auto_push
        ));
    }
    if let Some(interval) = compliance_interval {
        intervals.push(format!("compliance={}s", interval.as_secs()));
    }
    intervals
}

/// Emit the three-line startup banner: health endpoint, interval summary,
/// run hint. Pure-output; testable via `Printer::for_test_at(Verbosity::Normal)`
/// (Quiet suppresses Ok/Info statuses).
pub(super) fn print_startup_banner(printer: &Printer, intervals: &[String], ipc_path: &str) {
    printer.status_simple(Role::Ok, format!("Health: {}", ipc_path));
    printer.status_simple(Role::Ok, format!("Intervals: {}", intervals.join(", ")));
    printer.status_simple(Role::Info, "Daemon running — press Ctrl+C to stop");
}

/// Synchronous body of the startup server check-in. Resolves the profile,
/// posts the check-in payload, and clears any pending server config so the
/// first reconcile tick picks it up. Extracted from the `spawn_blocking`
/// closure so tests can drive the no-profile and resolve-failure arms without
/// scheduling onto a tokio runtime.
pub(super) fn run_startup_checkin_blocking(
    config_path: &Path,
    profile_override: Option<&str>,
    cfg: &CfgdConfig,
) {
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
    match config::resolve_profile(profile_name, &profiles_dir) {
        Ok(resolved) => {
            let changed = try_server_checkin(cfg, &resolved);
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
}

/// Remove the daemon's IPC socket file at shutdown. No-op on Windows (named
/// pipes are kernel objects with no on-disk artifact).
#[allow(unused_variables)]
pub(super) fn cleanup_ipc_socket(ipc_path: &Path) {
    #[cfg(unix)]
    {
        if ipc_path.exists() {
            let _ = std::fs::remove_file(ipc_path);
        }
    }
}

// --- Pump / shutdown task helpers ---

/// Spawn a task that pumps fixed-cadence ticks into `tx`. The interval is read
/// from `interval_secs` before every sleep, so SIGHUP-driven updates take
/// effect on the next iteration. Aborting the returned handle stops the pump.
fn spawn_interval_pump(
    interval_secs: Arc<std::sync::atomic::AtomicU64>,
    tx: mpsc::Sender<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let secs = interval_secs
                .load(std::sync::atomic::Ordering::Relaxed)
                .max(1);
            tokio::time::sleep(Duration::from_secs(secs)).await;
            if tx.send(()).await.is_err() {
                break;
            }
        }
    })
}

/// Spawn a task that pushes `()` to `tx` on every SIGHUP. Unix only.
#[cfg(unix)]
fn spawn_sighup_pump(tx: mpsc::Sender<()>) -> Result<tokio::task::JoinHandle<()>> {
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|e| DaemonError::WatchError {
            message: format!("failed to register SIGHUP handler: {}", e),
        })?;
    Ok(tokio::spawn(async move {
        while signal.recv().await.is_some() {
            if tx.send(()).await.is_err() {
                break;
            }
        }
    }))
}

/// Wait for SIGTERM (Unix) or Ctrl+C (any platform) and print the matching
/// shutdown message. Returns when either fires.
async fn wait_for_shutdown(printer: Arc<Printer>) {
    #[cfg(unix)]
    {
        let sigterm = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut s) => {
                    s.recv().await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to register SIGTERM handler");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            _ = sigterm => {
                printer.status_simple(Role::Info, "Received SIGTERM, shutting down daemon...");
            }
            _ = tokio::signal::ctrl_c() => {
                printer.status_simple(Role::Info, "Shutting down daemon...");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        printer.status_simple(Role::Info, "Shutting down daemon...");
    }
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
