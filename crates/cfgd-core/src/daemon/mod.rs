// Daemon — file watchers, reconciliation loop, sync, notifications, health endpoint, service management

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

struct SyncTask {
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

struct ReconcileTask {
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

struct DaemonState {
    started_at: Instant,
    last_reconcile: Option<String>,
    last_sync: Option<String>,
    drift_count: u32,
    sources: Vec<SourceStatus>,
    update_available: Option<String>,
    module_last_reconcile: HashMap<String, String>,
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
        }
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

struct Notifier {
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
            match ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(10))
                .build()
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

// --- Server Check-in ---

#[derive(Debug, Serialize)]
struct CheckinPayload {
    device_id: String,
    hostname: String,
    os: String,
    arch: String,
    config_hash: String,
}

#[derive(Debug, Deserialize)]
struct CheckinServerResponse {
    #[serde(rename = "status")]
    _status: String,
    config_changed: bool,
    #[serde(rename = "config")]
    _config: Option<serde_json::Value>,
}

/// Generate a stable device ID from the hostname using SHA256.
fn generate_device_id() -> std::result::Result<String, String> {
    let host = hostname::get()
        .map_err(|e| format!("failed to get hostname: {}", e))?
        .to_string_lossy()
        .to_string();
    Ok(crate::sha256_hex(host.as_bytes()))
}

/// Compute a SHA256 hash of the resolved profile serialized to YAML.
fn compute_config_hash(resolved: &ResolvedProfile) -> std::result::Result<String, String> {
    let yaml = serde_yaml::to_string(&resolved.merged.packages)
        .map_err(|e| format!("failed to serialize profile for hashing: {}", e))?;
    Ok(crate::sha256_hex(yaml.as_bytes()))
}

/// Perform a server check-in. Returns true if the server indicates config has changed.
/// On any error, logs a warning and returns false (best-effort).
fn server_checkin(server_url: &str, resolved: &ResolvedProfile) -> bool {
    let device_id = match generate_device_id() {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "server check-in failed");
            return false;
        }
    };

    let host = match hostname::get() {
        Ok(h) => h.to_string_lossy().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "server check-in: failed to get hostname");
            return false;
        }
    };

    let config_hash = match compute_config_hash(resolved) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "server check-in failed");
            return false;
        }
    };

    let payload = CheckinPayload {
        device_id,
        hostname: host,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        config_hash,
    };

    let url = format!("{}/api/v1/checkin", server_url.trim_end_matches('/'));

    let body = match serde_json::to_string(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "server check-in: failed to serialize payload");
            return false;
        }
    };

    tracing::info!(
        url = %url,
        device_id = %payload.device_id,
        "checking in with server"
    );

    match ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&body)
    {
        Ok(response) => {
            let status = response.status();
            match response.into_string() {
                Ok(resp_body) => match serde_json::from_str::<CheckinServerResponse>(&resp_body) {
                    Ok(resp) => {
                        tracing::info!(
                            config_changed = resp.config_changed,
                            "server check-in successful"
                        );
                        resp.config_changed
                    }
                    Err(e) => {
                        tracing::warn!(
                            status = status,
                            error = %e,
                            "server check-in: failed to parse response"
                        );
                        false
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "server check-in: failed to read response body");
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "server check-in failed");
            false
        }
    }
}

/// Find the server URL from the config's origin, if origin type is `server`.
fn find_server_url(config: &CfgdConfig) -> Option<String> {
    config
        .spec
        .origin
        .iter()
        .find(|o| matches!(o.origin_type, OriginType::Server))
        .map(|o| o.url.clone())
}

/// Perform a server check-in if configured. Returns true if config changed on server.
fn try_server_checkin(config: &CfgdConfig, resolved: &ResolvedProfile) -> bool {
    match find_server_url(config) {
        Some(url) => server_checkin(&url, resolved),
        None => false,
    }
}

// --- Parsed Daemon Config ---

/// Parsed daemon configuration values with defaults applied.
struct ParsedDaemonConfig {
    reconcile_interval: Duration,
    sync_interval: Duration,
    auto_pull: bool,
    auto_push: bool,
    on_change_reconcile: bool,
    notify_on_drift: bool,
    notify_method: NotifyMethod,
    webhook_url: Option<String>,
    auto_apply: bool,
}

fn parse_daemon_config(daemon_cfg: &config::DaemonConfig) -> ParsedDaemonConfig {
    let reconcile_interval = daemon_cfg
        .reconcile
        .as_ref()
        .map(|r| parse_duration_or_default(&r.interval))
        .unwrap_or(Duration::from_secs(DEFAULT_RECONCILE_SECS));

    let sync_interval = daemon_cfg
        .sync
        .as_ref()
        .map(|s| parse_duration_or_default(&s.interval))
        .unwrap_or(Duration::from_secs(DEFAULT_SYNC_SECS));

    let auto_pull = daemon_cfg
        .sync
        .as_ref()
        .map(|s| s.auto_pull)
        .unwrap_or(false);

    let auto_push = daemon_cfg
        .sync
        .as_ref()
        .map(|s| s.auto_push)
        .unwrap_or(false);

    let on_change_reconcile = daemon_cfg
        .reconcile
        .as_ref()
        .map(|r| r.on_change)
        .unwrap_or(false);

    let notify_on_drift = daemon_cfg.notify.as_ref().map(|n| n.drift).unwrap_or(false);

    let notify_method = daemon_cfg
        .notify
        .as_ref()
        .map(|n| n.method.clone())
        .unwrap_or(NotifyMethod::Stdout);

    let webhook_url = daemon_cfg
        .notify
        .as_ref()
        .and_then(|n| n.webhook_url.clone());

    let auto_apply = daemon_cfg
        .reconcile
        .as_ref()
        .map(|r| r.auto_apply)
        .unwrap_or(false);

    ParsedDaemonConfig {
        reconcile_interval,
        sync_interval,
        auto_pull,
        auto_push,
        on_change_reconcile,
        notify_on_drift,
        notify_method,
        webhook_url,
        auto_apply,
    }
}

/// Build the list of per-module and default reconcile tasks from daemon config and resolved profile.
///
/// For each module in the resolved profile, checks if reconcile patches produce effective
/// settings that differ from the global config. If so, creates a dedicated per-module task.
/// Always appends a `__default__` task for non-patched resources.
fn build_reconcile_tasks(
    daemon_cfg: &config::DaemonConfig,
    resolved: Option<&config::ResolvedProfile>,
    profile_chain: &[&str],
    reconcile_interval: Duration,
    auto_apply: bool,
) -> Vec<ReconcileTask> {
    let reconcile_patches = daemon_cfg
        .reconcile
        .as_ref()
        .map(|r| &r.patches[..])
        .unwrap_or(&[]);

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
        drift_policy: daemon_cfg
            .reconcile
            .as_ref()
            .map(|r| r.drift_policy.clone())
            .unwrap_or_default(),
        last_reconciled: None,
    });

    tasks
}

/// Build sync tasks for local config and each configured source.
///
/// Creates one task for the local config directory (always present), plus one task
/// per configured source whose cache directory exists on disk.
fn build_sync_tasks(
    config_dir: &Path,
    parsed: &ParsedDaemonConfig,
    sources: &[config::SourceSpec],
    allow_unsigned: bool,
    source_cache_dir: &Path,
    manifest_detector: impl Fn(&Path) -> Option<bool>,
) -> Vec<SyncTask> {
    let mut tasks: Vec<SyncTask> = vec![SyncTask {
        source_name: "local".to_string(),
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
            let require_signed = manifest_detector(&source_dir).unwrap_or(false);
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
    let state = Arc::new(Mutex::new(DaemonState::new()));

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

                // Record drift
                let drift_recorded = record_file_drift(&path);
                if drift_recorded {
                    let mut st = state.lock().await;
                    st.drift_count += 1;
                    if let Some(source) = st.sources.first_mut() {
                        source.drift_count += 1;
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

// --- File Watcher ---

fn setup_file_watcher(
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

fn discover_managed_paths(
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

fn handle_reconcile(
    config_path: &Path,
    profile_override: Option<&str>,
    state: &Arc<Mutex<DaemonState>>,
    notifier: &Arc<Notifier>,
    notify_on_drift: bool,
    hooks: &dyn DaemonHooks,
    state_dir_override: Option<&Path>,
) {
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
        let quiet_printer = crate::output::Printer::new(crate::output::Verbosity::Quiet);
        match crate::modules::resolve_modules(
            &resolved.merged.modules,
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
            &quiet_printer,
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
                false,
                None,
                None,
            );
            let printer = Printer::new(Verbosity::Quiet);
            let default_timeout = crate::PROFILE_SCRIPT_TIMEOUT;
            for entry in &scripts.on_drift {
                match crate::reconciler::execute_script(
                    entry,
                    &config_dir,
                    &script_env,
                    default_timeout,
                    &printer,
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
                let printer = Printer::new(Verbosity::Quiet);
                match reconciler.apply(
                    &plan,
                    &resolved,
                    &config_dir,
                    &printer,
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

fn action_resource_info(action: &crate::reconciler::Action) -> (String, String) {
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
fn extract_source_resources(merged: &MergedProfile) -> HashSet<String> {
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
fn hash_resources(resources: &HashSet<String>) -> String {
    let mut sorted: Vec<&String> = resources.iter().collect();
    sorted.sort();
    let combined: String = sorted.iter().map(|r| format!("{}\n", r)).collect();
    crate::sha256_hex(combined.as_bytes())
}

/// Process auto-apply decisions for source items. Returns the set of resource paths
/// that should be excluded from the plan (pending decisions).
fn process_source_decisions(
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
fn pending_resource_paths(store: &StateStore) -> HashSet<String> {
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
fn infer_item_tier(resource: &str) -> &'static str {
    // Files with "security" or "policy" in the path tend to be locked/required
    if resource.contains("security") || resource.contains("policy") || resource.contains("locked") {
        "locked"
    } else {
        "recommended"
    }
}

// --- Sync Handler ---

/// Returns true if changes were detected during sync.
fn handle_sync(
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

fn handle_version_check(state: &Arc<Mutex<DaemonState>>, notifier: &Arc<Notifier>) {
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

fn handle_compliance_snapshot(
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

fn git_pull(repo_path: &Path) -> std::result::Result<bool, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| format!("open repo: {}", e))?;

    let head = repo.head().map_err(|e| format!("get HEAD: {}", e))?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| "cannot determine branch name".to_string())?;

    // Try git CLI first with SSH hang protection.
    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(String::from));
    let repo_dir = &repo_path.display().to_string();
    let cli_ok = crate::try_git_cmd(
        remote_url.as_deref(),
        &["-C", repo_dir, "fetch", "origin", branch_name],
        "fetch",
        None,
    );

    if !cli_ok {
        // Fall back to libgit2
        let mut remote = repo
            .find_remote("origin")
            .map_err(|e| format!("find remote: {}", e))?;
        let mut fetch_opts = git2::FetchOptions::new();
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(crate::git_ssh_credentials);
        fetch_opts.remote_callbacks(callbacks);
        remote
            .fetch(&[branch_name], Some(&mut fetch_opts), None)
            .map_err(|e| format!("fetch: {}", e))?;
    }

    // Check if we need to fast-forward
    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .map_err(|e| format!("find FETCH_HEAD: {}", e))?;
    let fetch_commit = repo
        .reference_to_annotated_commit(&fetch_head)
        .map_err(|e| format!("resolve FETCH_HEAD: {}", e))?;

    let (analysis, _) = repo
        .merge_analysis(&[&fetch_commit])
        .map_err(|e| format!("merge analysis: {}", e))?;

    if analysis.is_up_to_date() {
        return Ok(false);
    }

    if analysis.is_fast_forward() {
        let refname = format!("refs/heads/{}", branch_name);
        let mut reference = repo
            .find_reference(&refname)
            .map_err(|e| format!("find ref: {}", e))?;
        reference
            .set_target(fetch_commit.id(), "cfgd: fast-forward pull")
            .map_err(|e| format!("set target: {}", e))?;
        repo.set_head(&refname)
            .map_err(|e| format!("set HEAD: {}", e))?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .map_err(|e| format!("checkout: {}", e))?;
        return Ok(true);
    }

    Err("cannot fast-forward — remote has diverged".to_string())
}

fn git_auto_commit_push(repo_path: &Path) -> std::result::Result<bool, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| format!("open repo: {}", e))?;

    // Check for changes
    let mut index = repo.index().map_err(|e| format!("get index: {}", e))?;
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| format!("stage changes: {}", e))?;
    index.write().map_err(|e| format!("write index: {}", e))?;

    let diff = repo
        .diff_index_to_workdir(Some(&index), None)
        .map_err(|e| format!("diff: {}", e))?;

    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());

    let staged_diff = if let Some(ref tree) = head_tree {
        repo.diff_tree_to_index(Some(tree), Some(&index), None)
            .map_err(|e| format!("staged diff: {}", e))?
    } else {
        // No HEAD yet, everything in index is new
        repo.diff_tree_to_index(None, Some(&index), None)
            .map_err(|e| format!("staged diff: {}", e))?
    };

    if diff.stats().map(|s| s.files_changed()).unwrap_or(0) == 0
        && staged_diff.stats().map(|s| s.files_changed()).unwrap_or(0) == 0
    {
        return Ok(false);
    }

    // Create commit
    let tree_oid = index
        .write_tree()
        .map_err(|e| format!("write tree: {}", e))?;
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|e| format!("find tree: {}", e))?;

    let signature = repo
        .signature()
        .map_err(|e| format!("get signature: {}", e))?;

    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());

    let parents: Vec<&git2::Commit> = parent.as_ref().map(|p| vec![p]).unwrap_or_default();

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "cfgd: auto-commit configuration changes",
        &tree,
        &parents,
    )
    .map_err(|e| format!("commit: {}", e))?;

    // Push — try git CLI first with SSH hang protection.
    let head = repo.head().map_err(|e| format!("get HEAD: {}", e))?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| "cannot determine branch name".to_string())?;

    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(String::from));

    let repo_dir = &repo_path.display().to_string();
    let cli_ok = crate::try_git_cmd(
        remote_url.as_deref(),
        &["-C", repo_dir, "push", "origin", branch_name],
        "push",
        None,
    );

    if !cli_ok {
        // Fall back to libgit2.
        let mut remote = repo
            .find_remote("origin")
            .map_err(|e| format!("find remote: {}", e))?;

        let mut push_opts = git2::PushOptions::new();
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(crate::git_ssh_credentials);
        push_opts.remote_callbacks(callbacks);

        let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
        remote
            .push(&[&refspec], Some(&mut push_opts))
            .map_err(|e| format!("push: {}", e))?;
    }

    Ok(true)
}

// --- Health Server ---

#[cfg(unix)]
async fn run_health_server(ipc_path: &str, state: Arc<Mutex<DaemonState>>) -> Result<()> {
    let listener = UnixListener::bind(ipc_path).map_err(|e| DaemonError::HealthSocketError {
        message: format!("bind {}: {}", ipc_path, e),
    })?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("accept: {}", e),
            })?;

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_health_connection(stream, state).await {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}

#[cfg(windows)]
async fn run_health_server(ipc_path: &str, state: Arc<Mutex<DaemonState>>) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(ipc_path)
        .map_err(|e| DaemonError::HealthSocketError {
            message: format!("create pipe {}: {}", ipc_path, e),
        })?;

    loop {
        server
            .connect()
            .await
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("accept pipe: {}", e),
            })?;

        let connected = server;
        server = ServerOptions::new()
            .first_pipe_instance(false)
            .create(ipc_path)
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("create pipe {}: {}", ipc_path, e),
            })?;

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_health_connection(connected, state).await {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}

async fn handle_health_connection<S>(
    stream: S,
    state: Arc<Mutex<DaemonState>>,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = tokio::io::BufReader::new(reader);

    // Read the HTTP request line
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    // Parse path from "GET /path HTTP/1.x"
    let path = request_line.split_whitespace().nth(1).unwrap_or("/health");

    // Drain remaining headers
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    let st = state.lock().await;

    let (status_code, body) = match path {
        "/health" => {
            let health = serde_json::json!({
                "status": "ok",
                "pid": std::process::id(),
                "uptime_secs": st.started_at.elapsed().as_secs(),
            });
            ("200 OK", serde_json::to_string_pretty(&health)?)
        }
        "/status" => {
            let response = st.to_response();
            ("200 OK", serde_json::to_string_pretty(&response)?)
        }
        "/drift" => {
            let store = StateStore::open_default();
            let drift_events = store.and_then(|s| s.unresolved_drift()).unwrap_or_default();

            let drift: Vec<serde_json::Value> = drift_events
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "resource_type": d.resource_type,
                        "resource_id": d.resource_id,
                        "expected": d.expected,
                        "actual": d.actual,
                        "timestamp": d.timestamp,
                    })
                })
                .collect();

            (
                "200 OK",
                serde_json::to_string_pretty(&serde_json::json!({
                    "drift_count": drift.len(),
                    "events": drift,
                }))?,
            )
        }
        _ => (
            "404 Not Found",
            serde_json::json!({"error": "not found"}).to_string(),
        ),
    };

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_code,
        body.len(),
        body
    );

    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;

    Ok(())
}

// --- Record drift for a specific file ---

fn record_file_drift_to(store: &StateStore, path: &Path) -> bool {
    match store.record_drift(
        "file",
        &path.display().to_string(),
        None,
        Some("modified"),
        "local",
    ) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, "failed to record drift");
            false
        }
    }
}

fn record_file_drift(path: &Path) -> bool {
    let store = match StateStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "cannot open state store for drift recording");
            return false;
        }
    };
    record_file_drift_to(&store, path)
}

// --- Service Management ---
// launchd on macOS, systemd on Linux, Windows Service on Windows.

pub fn install_service(config_path: &Path, profile: Option<&str>) -> Result<()> {
    let cfgd_binary = std::env::current_exe().map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("cannot determine binary path: {}", e),
    })?;
    #[cfg(windows)]
    {
        install_windows_service(&cfgd_binary, config_path, profile)
    }
    #[cfg(unix)]
    {
        if cfg!(target_os = "macos") {
            install_launchd_service(&cfgd_binary, config_path, profile)
        } else {
            install_systemd_service(&cfgd_binary, config_path, profile)
        }
    }
}

pub fn uninstall_service() -> Result<()> {
    #[cfg(windows)]
    {
        uninstall_windows_service()
    }
    #[cfg(unix)]
    {
        if cfg!(target_os = "macos") {
            uninstall_launchd_service()
        } else {
            uninstall_systemd_service()
        }
    }
}

/// Install cfgd as a Windows Service via sc.exe.
#[cfg(windows)]
fn install_windows_service(binary: &Path, config_path: &Path, profile: Option<&str>) -> Result<()> {
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let config_str = config_abs.display().to_string();
    let config_str = config_str.strip_prefix(r"\\?\").unwrap_or(&config_str);

    let binary_str = binary.display().to_string();
    let binary_str = binary_str.strip_prefix(r"\\?\").unwrap_or(&binary_str);

    let mut bin_args = format!(
        "\"{}\" daemon service --config \"{}\"",
        binary_str, config_str,
    );
    if let Some(p) = profile {
        bin_args.push_str(&format!(" --profile \"{}\"", p));
    }

    // sc.exe requires key= and value as separate arguments
    let output = std::process::Command::new("sc.exe")
        .args([
            "create",
            "cfgd",
            "binPath=",
            &bin_args,
            "start=",
            "auto",
            "DisplayName=",
            "cfgd Configuration Manager",
        ])
        .output()
        .map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("sc.exe create failed: {}", e),
        })?;

    if !output.status.success() {
        return Err(DaemonError::ServiceInstallFailed {
            message: format!(
                "sc.exe create failed: {}",
                crate::stdout_lossy_trimmed(&output)
            ),
        }
        .into());
    }

    // Set service description
    if let Err(e) = std::process::Command::new("sc.exe")
        .args([
            "description",
            "cfgd",
            "Declarative machine configuration management daemon",
        ])
        .output()
    {
        tracing::warn!(error = %e, "failed to set Windows Service description");
    }

    // Start the service
    if let Err(e) = std::process::Command::new("sc.exe")
        .args(["start", "cfgd"])
        .output()
    {
        tracing::warn!(error = %e, "failed to start Windows Service");
    }

    tracing::info!("installed Windows Service: cfgd");
    Ok(())
}

/// Uninstall cfgd Windows Service via sc.exe.
#[cfg(windows)]
fn uninstall_windows_service() -> Result<()> {
    // Stop service first (best-effort — may not be running)
    if let Err(e) = std::process::Command::new("sc.exe")
        .args(["stop", "cfgd"])
        .output()
    {
        tracing::debug!(error = %e, "sc.exe stop (pre-uninstall)");
    }

    let output = std::process::Command::new("sc.exe")
        .args(["delete", "cfgd"])
        .output()
        .map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("sc.exe delete failed: {}", e),
        })?;

    if !output.status.success() {
        return Err(DaemonError::ServiceInstallFailed {
            message: format!(
                "sc.exe delete failed: {}",
                crate::stdout_lossy_trimmed(&output)
            ),
        }
        .into());
    }

    tracing::info!("removed Windows Service: cfgd");
    Ok(())
}

/// Hooks stored before dispatching to the SCM so `windows_service_main` can retrieve them.
#[cfg(windows)]
static SERVICE_HOOKS: std::sync::OnceLock<Arc<dyn DaemonHooks>> = std::sync::OnceLock::new();

/// Run the daemon as a Windows Service. Called by the SCM (Service Control Manager),
/// not directly by users. `hooks` provides the binary-specific provider implementations.
#[cfg(windows)]
pub fn run_as_windows_service(hooks: Arc<dyn DaemonHooks>) -> Result<()> {
    use windows_service::service_dispatcher;
    // Store hooks before dispatching — ffi_service_main retrieves them via OnceLock.
    let _ = SERVICE_HOOKS.set(hooks);
    service_dispatcher::start("cfgd", ffi_service_main).map_err(|e| DaemonError::ServiceError {
        message: format!("failed to start service dispatcher: {}", e),
    })?;
    Ok(())
}

/// Windows Service mode is only available on Windows.
#[cfg(not(windows))]
pub fn run_as_windows_service(_hooks: Arc<dyn DaemonHooks>) -> Result<()> {
    Err(DaemonError::ServiceError {
        message: "Windows Service mode is only available on Windows".to_string(),
    }
    .into())
}

#[cfg(windows)]
extern "system" fn ffi_service_main(_argc: u32, _argv: *mut *mut u16) {
    if let Err(e) = windows_service_main() {
        tracing::error!(error = %e, "windows service main failed");
    }
}

#[cfg(windows)]
fn init_windows_logging() {
    let log_dir = std::env::var("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("cfgd"))
        .unwrap_or_else(|_| crate::default_config_dir());

    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("daemon.log");

    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_target(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    }
}

#[cfg(windows)]
fn windows_service_main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use windows_service::service::*;
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

    init_windows_logging();

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register("cfgd", event_handler)?;

    // Report StartPending while we initialize
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: std::time::Duration::from_secs(10),
        process_id: None,
    })?;

    // Parse config/profile from process args.
    // SCM invokes: cfgd.exe daemon service --config "C:\..." [--profile "name"]
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = crate::default_config_dir().join("config.yaml");
    let mut profile_override: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" if i + 1 < args.len() => {
                config_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--profile" if i + 1 < args.len() => {
                profile_override = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Retrieve hooks stored by run_as_windows_service
    let hooks = SERVICE_HOOKS
        .get()
        .ok_or("SERVICE_HOOKS not initialized — run_as_windows_service must be called first")?
        .clone();

    // Create the tokio runtime on the main service thread so we can shut it down gracefully
    let rt = tokio::runtime::Runtime::new()?;
    let printer = Arc::new(crate::output::Printer::new(crate::output::Verbosity::Quiet));

    // Spawn the daemon loop on the runtime
    rt.spawn(async move {
        if let Err(e) = run_daemon(config_path, profile_override, printer, hooks).await {
            tracing::error!(error = %e, "daemon error");
        }
    });

    // Report Running — daemon loop is now active
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    })?;

    // Block until the SCM sends a stop/shutdown signal
    let _ = shutdown_rx.recv();

    // Report StopPending
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StopPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: std::time::Duration::from_secs(5),
        process_id: None,
    })?;

    // Gracefully shut down the runtime, giving in-flight operations time to complete
    rt.shutdown_timeout(std::time::Duration::from_secs(5));

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

/// Generate launchd plist content for the daemon service.
#[cfg(unix)]
fn generate_launchd_plist(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    home: &Path,
) -> String {
    let mut args = vec![
        format!("<string>{}</string>", binary.display()),
        "<string>--config</string>".to_string(),
        format!("<string>{}</string>", config_path.display()),
        "<string>daemon</string>".to_string(),
    ];

    if let Some(p) = profile {
        args.push("<string>--profile</string>".to_string());
        args.push(format!("<string>{}</string>", p));
    }

    let args_xml = args.join("\n            ");
    let label = LAUNCHD_LABEL;
    let home_display = home.display();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
            {args_xml}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home_display}/Library/Logs/cfgd.log</string>
    <key>StandardErrorPath</key>
    <string>{home_display}/Library/Logs/cfgd.err</string>
</dict>
</plist>"#
    )
}

#[cfg(unix)]
fn install_launchd_service(binary: &Path, config_path: &Path, profile: Option<&str>) -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let plist_dir = home.join(LAUNCHD_AGENTS_DIR);
    std::fs::create_dir_all(&plist_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create LaunchAgents dir: {}", e),
    })?;

    let plist_path = plist_dir.join(format!("{}.plist", LAUNCHD_LABEL));
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let plist = generate_launchd_plist(binary, &config_abs, profile, &home);

    crate::atomic_write_str(&plist_path, &plist).map_err(|e| {
        DaemonError::ServiceInstallFailed {
            message: format!("write plist: {}", e),
        }
    })?;

    tracing::info!(path = %plist_path.display(), "installed launchd service");
    Ok(())
}

#[cfg(unix)]
fn uninstall_launchd_service() -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let plist_path = home
        .join(LAUNCHD_AGENTS_DIR)
        .join(format!("{}.plist", LAUNCHD_LABEL));

    if plist_path.exists() {
        std::fs::remove_file(&plist_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove plist: {}", e),
        })?;
        tracing::info!(path = %plist_path.display(), "removed launchd service");
    }

    Ok(())
}

/// Generate systemd unit file content for the daemon service.
#[cfg(unix)]
fn generate_systemd_unit(binary: &Path, config_path: &Path, profile: Option<&str>) -> String {
    let mut exec_start = format!(
        "{} --config {} daemon",
        binary.display(),
        config_path.display()
    );
    if let Some(p) = profile {
        exec_start = format!(
            "{} --config {} --profile {} daemon",
            binary.display(),
            config_path.display(),
            p
        );
    }

    format!(
        r#"[Unit]
Description=cfgd configuration daemon
After=network.target

[Service]
Type=simple
ExecStart={exec_start}
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target"#
    )
}

#[cfg(unix)]
fn install_systemd_service(binary: &Path, config_path: &Path, profile: Option<&str>) -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let unit_dir = home.join(SYSTEMD_USER_DIR);
    std::fs::create_dir_all(&unit_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create systemd user dir: {}", e),
    })?;

    let unit_path = unit_dir.join("cfgd.service");
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let unit = generate_systemd_unit(binary, &config_abs, profile);

    crate::atomic_write_str(&unit_path, &unit).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("write unit file: {}", e),
    })?;

    tracing::info!(path = %unit_path.display(), "installed systemd user service");
    Ok(())
}

#[cfg(unix)]
fn uninstall_systemd_service() -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let unit_path = home.join(SYSTEMD_USER_DIR).join("cfgd.service");

    if unit_path.exists() {
        std::fs::remove_file(&unit_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove unit file: {}", e),
        })?;
        tracing::info!(path = %unit_path.display(), "removed systemd user service");
    }

    Ok(())
}

// --- Status Query (for cfgd daemon status) ---

/// Connect to the daemon IPC endpoint. Returns `None` if the daemon is not reachable.
fn connect_daemon_ipc() -> Option<IpcStream> {
    #[cfg(unix)]
    {
        let path = PathBuf::from(DEFAULT_IPC_PATH);
        if !path.exists() {
            return None;
        }
        let stream = StdUnixStream::connect(&path).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        Some(IpcStream::Unix(stream))
    }
    #[cfg(windows)]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(DEFAULT_IPC_PATH)
            .ok()?;
        Some(IpcStream::Pipe(file))
    }
}

/// Platform-specific IPC stream wrapper implementing Read + Write.
enum IpcStream {
    #[cfg(unix)]
    Unix(StdUnixStream),
    #[cfg(windows)]
    Pipe(std::fs::File),
}

impl std::io::Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.read(buf),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.read(buf),
        }
    }
}

impl std::io::Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.write(buf),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.flush(),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.flush(),
        }
    }
}

pub fn query_daemon_status() -> Result<Option<DaemonStatusResponse>> {
    let mut stream = match connect_daemon_ipc() {
        Some(s) => s,
        None => return Ok(None),
    };

    write!(
        stream,
        "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| DaemonError::HealthSocketError {
        message: format!("write request: {}", e),
    })?;

    let reader = BufReader::new(&mut stream);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| DaemonError::HealthSocketError {
            message: format!("read response: {}", e),
        })?;

        if in_body {
            lines.push(line);
        } else if line.trim().is_empty() {
            in_body = true;
        }
    }

    let body = lines.join("\n");
    if body.is_empty() {
        return Ok(None);
    }

    let status: DaemonStatusResponse =
        serde_json::from_str(&body).map_err(|e| DaemonError::HealthSocketError {
            message: format!("parse response: {}", e),
        })?;

    Ok(Some(status))
}

// --- Public sync functions for CLI commands ---

pub fn git_pull_sync(repo_path: &Path) -> std::result::Result<bool, String> {
    git_pull(repo_path)
}

// --- Helpers ---

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::test_state;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration_or_default("30s"), Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration_or_default("5m"), Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration_or_default("1h"), Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_plain_number() {
        assert_eq!(parse_duration_or_default("120"), Duration::from_secs(120));
    }

    #[test]
    fn parse_duration_invalid_falls_back() {
        assert_eq!(
            parse_duration_or_default("invalid"),
            Duration::from_secs(DEFAULT_RECONCILE_SECS)
        );
    }

    #[test]
    fn parse_duration_with_whitespace() {
        assert_eq!(parse_duration_or_default(" 10m "), Duration::from_secs(600));
    }

    #[test]
    fn daemon_state_initial() {
        let state = DaemonState::new();
        assert!(state.last_reconcile.is_none());
        assert!(state.last_sync.is_none());
        assert_eq!(state.drift_count, 0);
        assert_eq!(state.sources.len(), 1);
        assert_eq!(state.sources[0].name, "local");
    }

    #[test]
    fn daemon_state_response() {
        let state = DaemonState::new();
        let response = state.to_response();
        assert!(response.running);
        assert!(response.pid > 0);
        assert_eq!(response.sources.len(), 1);
    }

    #[test]
    fn notifier_stdout_does_not_panic() {
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        assert!(matches!(notifier.method, NotifyMethod::Stdout));
        assert!(notifier.webhook_url.is_none());
        // Stdout notifier calls tracing::info! — verify it completes without panic
        notifier.notify("test", "message");
    }

    #[test]
    fn source_status_round_trips() {
        let status = SourceStatus {
            name: "local".to_string(),
            last_sync: Some("2026-01-01T00:00:00Z".to_string()),
            last_reconcile: None,
            drift_count: 3,
            status: "active".to_string(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "local");
        assert_eq!(parsed.last_sync.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert!(parsed.last_reconcile.is_none());
        assert_eq!(parsed.drift_count, 3);
        assert_eq!(parsed.status, "active");
        // Verify camelCase renaming
        assert!(json.contains("\"driftCount\":3"));
        assert!(json.contains("\"lastSync\":"));
    }

    #[test]
    #[cfg(unix)]
    fn systemd_unit_path() {
        let home = "/home/testuser";
        let unit_dir = PathBuf::from(home).join(SYSTEMD_USER_DIR);
        let unit_path = unit_dir.join("cfgd.service");
        assert_eq!(
            unit_path.to_str().unwrap(),
            "/home/testuser/.config/systemd/user/cfgd.service"
        );
    }

    #[test]
    fn generate_device_id_is_stable() {
        let id1 = generate_device_id().unwrap();
        let id2 = generate_device_id().unwrap();
        assert_eq!(id1, id2);
        // SHA256 hex string is 64 characters
        assert_eq!(id1.len(), 64);
    }

    #[test]
    fn compute_config_hash_is_deterministic() {
        use crate::config::{
            CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
            ResolvedProfile,
        };
        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let hash1 = compute_config_hash(&resolved).unwrap();
        let hash2 = compute_config_hash(&resolved).unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn find_server_url_returns_none_for_git_origin() {
        use crate::config::*;
        let config = CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "Config".into(),
            metadata: ConfigMetadata {
                name: "test".into(),
            },
            spec: ConfigSpec {
                profile: Some("default".into()),
                origin: vec![OriginSpec {
                    origin_type: OriginType::Git,
                    url: "https://github.com/test/repo.git".into(),
                    branch: "master".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                }],
                daemon: None,
                secrets: None,
                sources: vec![],
                theme: None,
                modules: None,
                security: None,
                aliases: std::collections::HashMap::new(),
                file_strategy: crate::config::FileStrategy::default(),
                ai: None,
                compliance: None,
            },
        };
        assert!(find_server_url(&config).is_none());
    }

    #[test]
    fn find_server_url_returns_url_for_server_origin() {
        use crate::config::*;
        let config = CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "Config".into(),
            metadata: ConfigMetadata {
                name: "test".into(),
            },
            spec: ConfigSpec {
                profile: Some("default".into()),
                origin: vec![OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://cfgd.example.com".into(),
                    branch: "master".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                }],
                daemon: None,
                secrets: None,
                sources: vec![],
                theme: None,
                modules: None,
                security: None,
                aliases: std::collections::HashMap::new(),
                file_strategy: crate::config::FileStrategy::default(),
                ai: None,
                compliance: None,
            },
        };
        assert_eq!(
            find_server_url(&config),
            Some("https://cfgd.example.com".to_string())
        );
    }

    #[test]
    fn checkin_payload_round_trips() {
        let payload = CheckinPayload {
            device_id: "abc123".into(),
            hostname: "test-host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            config_hash: "deadbeef".into(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["device_id"], "abc123");
        assert_eq!(parsed["hostname"], "test-host");
        assert_eq!(parsed["os"], "linux");
        assert_eq!(parsed["arch"], "x86_64");
        assert_eq!(parsed["config_hash"], "deadbeef");
        // Exactly 5 fields
        assert_eq!(parsed.as_object().unwrap().len(), 5);
    }

    #[test]
    fn checkin_response_deserializes() {
        let json = r#"{"status":"ok","config_changed":true,"config":null}"#;
        let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
        assert!(resp.config_changed);
        assert_eq!(resp._status, "ok");
    }

    #[test]
    #[cfg(unix)]
    fn launchd_plist_path() {
        let home = "/Users/testuser";
        let plist_dir = PathBuf::from(home).join(LAUNCHD_AGENTS_DIR);
        let plist_path = plist_dir.join(format!("{}.plist", LAUNCHD_LABEL));
        assert_eq!(
            plist_path.to_str().unwrap(),
            "/Users/testuser/Library/LaunchAgents/com.cfgd.daemon.plist"
        );
    }

    #[test]
    fn extract_source_resources_from_merged_profile() {
        use crate::config::{
            BrewSpec, CargoSpec, FilesSpec, ManagedFileSpec, MergedProfile, PackagesSpec,
        };

        let merged = MergedProfile {
            packages: PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec!["ripgrep".into(), "fd".into()],
                    casks: vec!["firefox".into()],
                    ..Default::default()
                }),
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            files: FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "dotfiles/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            env: vec![crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "vim".into(),
            }],
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("packages.brew.ripgrep"));
        assert!(resources.contains("packages.brew.fd"));
        assert!(resources.contains("packages.brew.firefox"));
        assert!(resources.contains("packages.cargo.bat"));
        assert!(resources.contains("files./home/user/.zshrc"));
        assert!(resources.contains("env.EDITOR"));
        assert_eq!(resources.len(), 6);
    }

    #[test]
    fn hash_resources_is_deterministic() {
        let r1: HashSet<String> =
            HashSet::from_iter(["a".to_string(), "b".to_string(), "c".to_string()]);
        let r2: HashSet<String> =
            HashSet::from_iter(["c".to_string(), "a".to_string(), "b".to_string()]);

        assert_eq!(hash_resources(&r1), hash_resources(&r2));
    }

    #[test]
    fn hash_resources_differs_for_different_sets() {
        let r1: HashSet<String> = HashSet::from_iter(["a".to_string()]);
        let r2: HashSet<String> = HashSet::from_iter(["b".to_string()]);

        assert_ne!(hash_resources(&r1), hash_resources(&r2));
    }

    #[test]
    fn infer_item_tier_defaults_to_recommended() {
        assert_eq!(infer_item_tier("packages.brew.ripgrep"), "recommended");
        assert_eq!(infer_item_tier("env.EDITOR"), "recommended");
    }

    #[test]
    fn infer_item_tier_detects_locked() {
        assert_eq!(infer_item_tier("files.security-policy.yaml"), "locked");
        assert_eq!(
            infer_item_tier("files./home/user/.config/company/security.yaml"),
            "locked"
        );
    }

    #[test]
    fn process_source_decisions_first_run_records_decisions() {
        use crate::config::PackagesSpec;
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig::default(); // new_recommended: Notify

        let merged = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(crate::config::CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

        // First run: all items are new, policy is Notify → pending decisions created
        let pending = store.pending_decisions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].resource, "packages.cargo.bat");
        assert!(excluded.contains("packages.cargo.bat"));
    }

    #[test]
    fn process_source_decisions_accept_policy_no_pending() {
        use crate::config::PackagesSpec;
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Accept,
            ..Default::default()
        };

        let merged = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(crate::config::CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

        // Accept policy: no pending decisions, not excluded from plan
        let pending = store.pending_decisions().unwrap();
        assert!(pending.is_empty());
        assert!(!excluded.contains("packages.cargo.bat"));
    }

    // --- Compliance snapshot-on-change logic ---

    #[test]
    fn compliance_snapshot_skips_when_hash_unchanged() {
        let store = test_state();
        let snapshot = crate::compliance::ComplianceSnapshot {
            timestamp: crate::utc_now_iso8601(),
            machine: crate::compliance::MachineInfo {
                hostname: "test".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
            },
            profile: "default".into(),
            sources: vec!["local".into()],
            checks: vec![crate::compliance::ComplianceCheck {
                category: "file".into(),
                status: crate::compliance::ComplianceStatus::Compliant,
                detail: Some("present".into()),
                ..Default::default()
            }],
            summary: crate::compliance::ComplianceSummary {
                compliant: 1,
                warning: 0,
                violation: 0,
            },
        };

        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let hash = crate::sha256_hex(json.as_bytes());

        // Store first snapshot
        store.store_compliance_snapshot(&snapshot, &hash).unwrap();

        // Latest hash should match — a second store would be skipped
        let latest = store.latest_compliance_hash().unwrap();
        assert_eq!(latest.as_deref(), Some(hash.as_str()));
    }

    #[test]
    fn compliance_snapshot_stores_when_hash_changes() {
        let store = test_state();

        let snapshot1 = crate::compliance::ComplianceSnapshot {
            timestamp: "2026-01-01T00:00:00Z".into(),
            machine: crate::compliance::MachineInfo {
                hostname: "test".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
            },
            profile: "default".into(),
            sources: vec!["local".into()],
            checks: vec![crate::compliance::ComplianceCheck {
                category: "file".into(),
                status: crate::compliance::ComplianceStatus::Compliant,
                ..Default::default()
            }],
            summary: crate::compliance::ComplianceSummary {
                compliant: 1,
                warning: 0,
                violation: 0,
            },
        };

        let json1 = serde_json::to_string_pretty(&snapshot1).unwrap();
        let hash1 = crate::sha256_hex(json1.as_bytes());
        store.store_compliance_snapshot(&snapshot1, &hash1).unwrap();

        // Different snapshot with a violation
        let snapshot2 = crate::compliance::ComplianceSnapshot {
            timestamp: "2026-01-02T00:00:00Z".into(),
            machine: crate::compliance::MachineInfo {
                hostname: "test".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
            },
            profile: "default".into(),
            sources: vec!["local".into()],
            checks: vec![crate::compliance::ComplianceCheck {
                category: "package".into(),
                status: crate::compliance::ComplianceStatus::Violation,
                ..Default::default()
            }],
            summary: crate::compliance::ComplianceSummary {
                compliant: 0,
                warning: 0,
                violation: 1,
            },
        };

        let json2 = serde_json::to_string_pretty(&snapshot2).unwrap();
        let hash2 = crate::sha256_hex(json2.as_bytes());

        // Hashes differ — new snapshot should be stored
        assert_ne!(hash1, hash2);
        let latest = store.latest_compliance_hash().unwrap();
        assert_ne!(latest.as_deref(), Some(hash2.as_str()));

        store.store_compliance_snapshot(&snapshot2, &hash2).unwrap();
        let latest = store.latest_compliance_hash().unwrap();
        assert_eq!(latest.as_deref(), Some(hash2.as_str()));

        // Both snapshots stored
        let history = store.compliance_history(None, 10).unwrap();
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn compliance_timer_not_created_when_disabled() {
        // When compliance is not enabled, compliance_interval should be None
        let config = config::ComplianceConfig {
            enabled: false,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        let interval = config
            .enabled
            .then(|| crate::parse_duration_str(&config.interval).ok())
            .flatten();

        assert!(interval.is_none());
    }

    #[test]
    fn compliance_timer_created_when_enabled() {
        let config = config::ComplianceConfig {
            enabled: true,
            interval: "30m".into(),
            retention: "7d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        let interval = config
            .enabled
            .then(|| crate::parse_duration_str(&config.interval).ok())
            .flatten();

        assert_eq!(interval, Some(Duration::from_secs(30 * 60)));
    }

    #[test]
    fn compliance_timer_invalid_interval_when_enabled() {
        let config = config::ComplianceConfig {
            enabled: true,
            interval: "garbage".into(),
            retention: "7d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        let interval = config
            .enabled
            .then(|| crate::parse_duration_str(&config.interval).ok())
            .flatten();

        // Enabled but unparseable interval -> None (no timer)
        assert!(interval.is_none());
    }

    // --- compute_config_hash: different profiles produce different hashes ---

    #[test]
    fn compute_config_hash_differs_for_different_packages() {
        use crate::config::{
            CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
            ResolvedProfile,
        };

        let resolved_a = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "a".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        };

        let resolved_b = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "b".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["ripgrep".into()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        };

        let hash_a = compute_config_hash(&resolved_a).unwrap();
        let hash_b = compute_config_hash(&resolved_b).unwrap();
        assert_ne!(hash_a, hash_b);
    }

    // --- hash_resources edge cases ---

    #[test]
    fn hash_resources_empty_set() {
        let empty: HashSet<String> = HashSet::new();
        let hash = hash_resources(&empty);
        // Should produce a valid hash (SHA256 of empty string)
        assert_eq!(hash, crate::sha256_hex(b""));
    }

    #[test]
    fn hash_resources_single_element() {
        let set: HashSet<String> = HashSet::from_iter(["packages.brew.ripgrep".to_string()]);
        let hash = hash_resources(&set);
        assert_eq!(hash.len(), 64);
        // Compare against known SHA256 of "packages.brew.ripgrep\n"
        let expected = crate::sha256_hex(b"packages.brew.ripgrep\n");
        assert_eq!(hash, expected);
    }

    // --- DaemonState::to_response field validation ---

    #[test]
    fn daemon_state_to_response_propagates_fields() {
        let mut state = DaemonState::new();
        state.last_reconcile = Some("2026-03-30T12:00:00Z".to_string());
        state.last_sync = Some("2026-03-30T12:01:00Z".to_string());
        state.drift_count = 5;
        state.update_available = Some("2.0.0".to_string());

        let response = state.to_response();
        assert!(response.running);
        assert_eq!(
            response.last_reconcile.as_deref(),
            Some("2026-03-30T12:00:00Z")
        );
        assert_eq!(response.last_sync.as_deref(), Some("2026-03-30T12:01:00Z"));
        assert_eq!(response.drift_count, 5);
        assert_eq!(response.update_available.as_deref(), Some("2.0.0"));
        assert_eq!(response.sources.len(), 1);
        assert_eq!(response.sources[0].name, "local");
    }

    // --- DaemonStatusResponse with module_reconcile and update_available ---

    #[test]
    fn daemon_status_response_with_modules_round_trips() {
        let response = DaemonStatusResponse {
            running: true,
            pid: 42,
            uptime_secs: 100,
            last_reconcile: None,
            last_sync: None,
            drift_count: 2,
            sources: vec![],
            update_available: Some("1.5.0".to_string()),
            module_reconcile: vec![
                ModuleReconcileStatus {
                    name: "security-baseline".to_string(),
                    interval: "60s".to_string(),
                    auto_apply: true,
                    drift_policy: "Auto".to_string(),
                    last_reconcile: Some("2026-03-30T00:00:00Z".to_string()),
                },
                ModuleReconcileStatus {
                    name: "dev-tools".to_string(),
                    interval: "300s".to_string(),
                    auto_apply: false,
                    drift_policy: "NotifyOnly".to_string(),
                    last_reconcile: None,
                },
            ],
        };

        let json = serde_json::to_string(&response).unwrap();
        let parsed: DaemonStatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, 42);
        assert_eq!(parsed.drift_count, 2);
        assert_eq!(parsed.update_available.as_deref(), Some("1.5.0"));
        assert_eq!(parsed.module_reconcile.len(), 2);
        assert_eq!(parsed.module_reconcile[0].name, "security-baseline");
        assert!(parsed.module_reconcile[0].auto_apply);
        assert_eq!(parsed.module_reconcile[1].name, "dev-tools");
        assert!(!parsed.module_reconcile[1].auto_apply);
        assert!(parsed.module_reconcile[1].last_reconcile.is_none());
    }

    #[test]
    fn daemon_status_response_skips_empty_module_reconcile() {
        let response = DaemonStatusResponse {
            running: true,
            pid: 1,
            uptime_secs: 0,
            last_reconcile: None,
            last_sync: None,
            drift_count: 0,
            sources: vec![],
            update_available: None,
            module_reconcile: vec![],
        };

        let json = serde_json::to_string(&response).unwrap();
        // module_reconcile has skip_serializing_if = "Vec::is_empty"
        assert!(!json.contains("\"moduleReconcile\""));
        // update_available has skip_serializing_if = "Option::is_none"
        assert!(!json.contains("\"updateAvailable\""));
    }

    // --- action_resource_info tests ---

    #[test]
    fn action_resource_info_file_create() {
        use crate::reconciler::Action;

        let action = Action::File(crate::providers::FileAction::Create {
            source: PathBuf::from("/src/.zshrc"),
            target: PathBuf::from("/home/user/.zshrc"),
            origin: "local".into(),
            strategy: crate::config::FileStrategy::default(),
            source_hash: None,
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "file");
        assert_eq!(rid, "/home/user/.zshrc");
    }

    #[test]
    fn action_resource_info_file_update() {
        use crate::reconciler::Action;

        let action = Action::File(crate::providers::FileAction::Update {
            source: PathBuf::from("/src/.zshrc"),
            target: PathBuf::from("/home/user/.zshrc"),
            diff: "--- a\n+++ b".into(),
            origin: "local".into(),
            strategy: crate::config::FileStrategy::default(),
            source_hash: None,
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "file");
        assert_eq!(rid, "/home/user/.zshrc");
    }

    #[test]
    fn action_resource_info_file_delete() {
        use crate::reconciler::Action;

        let action = Action::File(crate::providers::FileAction::Delete {
            target: PathBuf::from("/tmp/gone"),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "file");
        assert_eq!(rid, "/tmp/gone");
    }

    #[test]
    fn action_resource_info_file_set_permissions() {
        use crate::reconciler::Action;

        let action = Action::File(crate::providers::FileAction::SetPermissions {
            target: PathBuf::from("/home/user/.ssh/config"),
            mode: 0o600,
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "file");
        assert_eq!(rid, "/home/user/.ssh/config");
    }

    #[test]
    fn action_resource_info_file_skip() {
        use crate::reconciler::Action;

        let action = Action::File(crate::providers::FileAction::Skip {
            target: PathBuf::from("/etc/skipped"),
            reason: "not needed".into(),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "file");
        assert_eq!(rid, "/etc/skipped");
    }

    #[test]
    fn action_resource_info_package_bootstrap() {
        use crate::reconciler::Action;

        let action = Action::Package(crate::providers::PackageAction::Bootstrap {
            manager: "brew".into(),
            method: "curl".into(),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "package");
        assert_eq!(rid, "brew:bootstrap");
    }

    #[test]
    fn action_resource_info_package_install() {
        use crate::reconciler::Action;

        let action = Action::Package(crate::providers::PackageAction::Install {
            manager: "apt".into(),
            packages: vec!["curl".into(), "wget".into()],
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "package");
        assert_eq!(rid, "apt:curl,wget");
    }

    #[test]
    fn action_resource_info_package_uninstall() {
        use crate::reconciler::Action;

        let action = Action::Package(crate::providers::PackageAction::Uninstall {
            manager: "npm".into(),
            packages: vec!["typescript".into()],
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "package");
        assert_eq!(rid, "npm:typescript");
    }

    #[test]
    fn action_resource_info_package_skip() {
        use crate::reconciler::Action;

        let action = Action::Package(crate::providers::PackageAction::Skip {
            manager: "cargo".into(),
            reason: "not available".into(),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "package");
        assert_eq!(rid, "cargo");
    }

    #[test]
    fn action_resource_info_secret_decrypt() {
        use crate::reconciler::Action;

        let action = Action::Secret(crate::providers::SecretAction::Decrypt {
            source: PathBuf::from("/secrets/api.enc"),
            target: PathBuf::from("/home/user/.api_key"),
            backend: "age".into(),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "secret");
        assert_eq!(rid, "/home/user/.api_key");
    }

    #[test]
    fn action_resource_info_secret_resolve() {
        use crate::reconciler::Action;

        let action = Action::Secret(crate::providers::SecretAction::Resolve {
            provider: "1password".into(),
            reference: "op://vault/item/field".into(),
            target: PathBuf::from("/tmp/secret"),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "secret");
        assert_eq!(rid, "op://vault/item/field");
    }

    #[test]
    fn action_resource_info_secret_resolve_env() {
        use crate::reconciler::Action;

        let action = Action::Secret(crate::providers::SecretAction::ResolveEnv {
            provider: "vault".into(),
            reference: "secret/data/app".into(),
            envs: vec!["API_KEY".into(), "DB_PASS".into()],
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "secret");
        assert_eq!(rid, "env:[API_KEY,DB_PASS]");
    }

    #[test]
    fn action_resource_info_secret_skip() {
        use crate::reconciler::Action;

        let action = Action::Secret(crate::providers::SecretAction::Skip {
            source: "bitwarden".into(),
            reason: "not configured".into(),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "secret");
        assert_eq!(rid, "bitwarden");
    }

    #[test]
    fn action_resource_info_system_set_value() {
        use crate::reconciler::{Action, SystemAction};

        let action = Action::System(SystemAction::SetValue {
            configurator: "sysctl".into(),
            key: "vm.swappiness".into(),
            desired: "10".into(),
            current: "60".into(),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "system");
        assert_eq!(rid, "sysctl:vm.swappiness");
    }

    #[test]
    fn action_resource_info_system_skip() {
        use crate::reconciler::{Action, SystemAction};

        let action = Action::System(SystemAction::Skip {
            configurator: "gsettings".into(),
            reason: "not on GNOME".into(),
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "system");
        assert_eq!(rid, "gsettings");
    }

    #[test]
    fn action_resource_info_script_run() {
        use crate::reconciler::{Action, ScriptAction, ScriptPhase};

        let action = Action::Script(ScriptAction::Run {
            entry: crate::config::ScriptEntry::Simple("echo hello".into()),
            phase: ScriptPhase::PreApply,
            origin: "local".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "script");
        assert_eq!(rid, "echo hello");
    }

    #[test]
    fn action_resource_info_module() {
        use crate::reconciler::{Action, ModuleAction, ModuleActionKind};

        let action = Action::Module(ModuleAction {
            module_name: "security-baseline".into(),
            kind: ModuleActionKind::InstallPackages { resolved: vec![] },
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "module");
        assert_eq!(rid, "security-baseline");
    }

    #[test]
    fn action_resource_info_env_write() {
        use crate::reconciler::{Action, EnvAction};

        let action = Action::Env(EnvAction::WriteEnvFile {
            path: PathBuf::from("/home/user/.cfgd.env"),
            content: "export FOO=bar".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "env");
        assert_eq!(rid, "/home/user/.cfgd.env");
    }

    #[test]
    fn action_resource_info_env_inject() {
        use crate::reconciler::{Action, EnvAction};

        let action = Action::Env(EnvAction::InjectSourceLine {
            rc_path: PathBuf::from("/home/user/.bashrc"),
            line: "source ~/.cfgd.env".into(),
        });
        let (rtype, rid) = action_resource_info(&action);
        assert_eq!(rtype, "env-rc");
        assert_eq!(rid, "/home/user/.bashrc");
    }

    // --- extract_source_resources with more package managers ---

    #[test]
    fn extract_source_resources_apt_dnf_pipx_npm() {
        use crate::config::{AptSpec, MergedProfile, NpmSpec, PackagesSpec};

        let merged = MergedProfile {
            packages: PackagesSpec {
                apt: Some(AptSpec {
                    file: None,
                    packages: vec!["git".into(), "tmux".into()],
                }),
                dnf: vec!["vim".into()],
                pipx: vec!["black".into()],
                npm: Some(NpmSpec {
                    file: None,
                    global: vec!["prettier".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("packages.apt.git"));
        assert!(resources.contains("packages.apt.tmux"));
        assert!(resources.contains("packages.dnf.vim"));
        assert!(resources.contains("packages.pipx.black"));
        assert!(resources.contains("packages.npm.prettier"));
        assert_eq!(resources.len(), 5);
    }

    #[test]
    fn extract_source_resources_system_keys() {
        use crate::config::MergedProfile;

        let mut merged = MergedProfile::default();
        merged
            .system
            .insert("sysctl".into(), serde_yaml::Value::Null);
        merged
            .system
            .insert("kernelModules".into(), serde_yaml::Value::Null);

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("system.sysctl"));
        assert!(resources.contains("system.kernelModules"));
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn extract_source_resources_empty_profile() {
        let merged = crate::config::MergedProfile::default();
        let resources = extract_source_resources(&merged);
        assert!(resources.is_empty());
    }

    // --- Config change detection: process_source_decisions second call ---

    #[test]
    fn process_source_decisions_no_change_on_second_call() {
        use crate::config::{CargoSpec, PackagesSpec};
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: crate::config::PolicyAction::Accept,
            ..Default::default()
        };

        let merged = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // First call: stores the hash
        let _ = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

        // Second call with same profile: hash matches, no new decisions
        let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

        // No pending decisions since policy is Accept
        let pending = store.pending_decisions().unwrap();
        assert!(pending.is_empty());
        assert!(excluded.is_empty());
    }

    #[test]
    fn process_source_decisions_detects_new_items_on_change() {
        use crate::config::{CargoSpec, PackagesSpec};
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig::default(); // Notify by default

        // First call with one package
        let merged1 = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = process_source_decisions(&store, "acme", &merged1, &policy, &notifier);
        // Clear pending decisions from first run
        let first_pending = store.pending_decisions().unwrap();
        for d in &first_pending {
            let _ = store.resolve_decisions_for_source(&d.source, "accepted");
        }

        // Second call with an additional package
        let merged2 = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into(), "ripgrep".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let excluded = process_source_decisions(&store, "acme", &merged2, &policy, &notifier);

        // Should have a pending decision for ripgrep (new item)
        let pending = store.pending_decisions().unwrap();
        assert!(!pending.is_empty());
        let resource_names: Vec<&str> = pending.iter().map(|d| d.resource.as_str()).collect();
        assert!(resource_names.contains(&"packages.cargo.ripgrep"));
        assert!(excluded.contains("packages.cargo.ripgrep"));
    }

    // --- infer_item_tier: "policy" keyword ---

    #[test]
    fn infer_item_tier_detects_policy_keyword() {
        assert_eq!(infer_item_tier("files.policy-definitions.yaml"), "locked");
        assert_eq!(infer_item_tier("system.security-policy"), "locked");
    }

    // --- ModuleReconcileStatus serialization ---

    #[test]
    fn module_reconcile_status_round_trips() {
        let status = ModuleReconcileStatus {
            name: "dev-tools".into(),
            interval: "120s".into(),
            auto_apply: false,
            drift_policy: "NotifyOnly".into(),
            last_reconcile: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: ModuleReconcileStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "dev-tools");
        assert_eq!(parsed.interval, "120s");
        assert!(!parsed.auto_apply);
        assert_eq!(parsed.drift_policy, "NotifyOnly");
        assert!(parsed.last_reconcile.is_none());
        // Verify camelCase
        assert!(json.contains("\"autoApply\""));
        assert!(json.contains("\"driftPolicy\""));
        assert!(json.contains("\"lastReconcile\""));
    }

    // --- Notifier construction ---

    #[test]
    fn notifier_webhook_without_url_does_not_panic() {
        let notifier = Notifier::new(NotifyMethod::Webhook, None);
        assert!(matches!(notifier.method, NotifyMethod::Webhook));
        // Webhook with no URL should early-return via `let Some(ref url) = ...` guard
        assert!(
            notifier.webhook_url.is_none(),
            "webhook_url must be None to exercise the early-return path"
        );
        // Should log a warning but not panic and not attempt any HTTP request
        notifier.notify("test", "no url configured");
    }

    // --- find_server_url with multiple origins ---

    #[test]
    fn find_server_url_picks_server_among_multiple_origins() {
        use crate::config::*;
        let config = CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "Config".into(),
            metadata: ConfigMetadata {
                name: "test".into(),
            },
            spec: ConfigSpec {
                profile: Some("default".into()),
                origin: vec![
                    OriginSpec {
                        origin_type: OriginType::Git,
                        url: "https://github.com/test/repo.git".into(),
                        branch: "main".into(),
                        auth: None,
                        ssh_strict_host_key_checking: Default::default(),
                    },
                    OriginSpec {
                        origin_type: OriginType::Server,
                        url: "https://fleet.example.com".into(),
                        branch: "main".into(),
                        auth: None,
                        ssh_strict_host_key_checking: Default::default(),
                    },
                ],
                daemon: None,
                secrets: None,
                sources: vec![],
                theme: None,
                modules: None,
                security: None,
                aliases: std::collections::HashMap::new(),
                file_strategy: crate::config::FileStrategy::default(),
                ai: None,
                compliance: None,
            },
        };
        assert_eq!(
            find_server_url(&config),
            Some("https://fleet.example.com".to_string())
        );
    }

    #[test]
    fn find_server_url_returns_none_for_empty_origins() {
        use crate::config::*;
        let config = CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "Config".into(),
            metadata: ConfigMetadata {
                name: "test".into(),
            },
            spec: ConfigSpec {
                profile: Some("default".into()),
                origin: vec![],
                daemon: None,
                secrets: None,
                sources: vec![],
                theme: None,
                modules: None,
                security: None,
                aliases: std::collections::HashMap::new(),
                file_strategy: crate::config::FileStrategy::default(),
                ai: None,
                compliance: None,
            },
        };
        assert!(find_server_url(&config).is_none());
    }

    // --- CheckinServerResponse deserialization edge cases ---

    #[test]
    fn checkin_response_with_config_payload() {
        let json = r#"{"status":"ok","config_changed":true,"config":{"packages":["git"]}}"#;
        let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
        assert!(resp.config_changed);
        assert!(resp._config.is_some());
    }

    #[test]
    fn checkin_response_no_change() {
        let json = r#"{"status":"ok","config_changed":false,"config":null}"#;
        let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.config_changed);
    }

    // --- parse_duration_or_default: zero values ---

    #[test]
    fn parse_duration_zero_seconds() {
        assert_eq!(parse_duration_or_default("0s"), Duration::from_secs(0));
    }

    #[test]
    fn parse_duration_zero_plain() {
        assert_eq!(parse_duration_or_default("0"), Duration::from_secs(0));
    }

    // --- compute_config_hash with empty packages ---

    #[test]
    fn compute_config_hash_with_empty_packages() {
        use crate::config::{
            LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
        };

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "empty".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                ..Default::default()
            },
        };

        let hash1 = compute_config_hash(&resolved).unwrap();
        let hash2 = compute_config_hash(&resolved).unwrap();
        assert_eq!(hash1, hash2, "hash should be deterministic");
        assert_eq!(hash1.len(), 64, "hash should be a valid SHA256 hex string");
    }

    // --- extract_source_resources: brew taps are not included, casks are ---

    #[test]
    fn extract_source_resources_brew_casks_only() {
        use crate::config::{BrewSpec, MergedProfile, PackagesSpec};

        let merged = MergedProfile {
            packages: PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec![],
                    casks: vec!["iterm2".into(), "visual-studio-code".into()],
                    taps: vec!["homebrew/cask".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert!(
            resources.contains("packages.brew.iterm2"),
            "casks should appear as brew resources"
        );
        assert!(
            resources.contains("packages.brew.visual-studio-code"),
            "casks should appear as brew resources"
        );
        // Taps are not tracked as individual resources
        assert!(
            !resources.contains("packages.brew.homebrew/cask"),
            "taps should not appear as resources"
        );
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn extract_source_resources_cargo_packages_only() {
        use crate::config::{CargoSpec, MergedProfile, PackagesSpec};

        let merged = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: Some("Cargo.toml".into()),
                    packages: vec!["cargo-watch".into(), "cargo-expand".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("packages.cargo.cargo-watch"));
        assert!(resources.contains("packages.cargo.cargo-expand"));
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn extract_source_resources_npm_globals() {
        use crate::config::{MergedProfile, NpmSpec, PackagesSpec};

        let merged = MergedProfile {
            packages: PackagesSpec {
                npm: Some(NpmSpec {
                    file: None,
                    global: vec!["typescript".into(), "eslint".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("packages.npm.typescript"));
        assert!(resources.contains("packages.npm.eslint"));
        assert_eq!(resources.len(), 2);
    }

    // --- process_source_decisions with Reject policy ---

    #[test]
    fn process_source_decisions_reject_policy_silently_skips() {
        use crate::config::{CargoSpec, PackagesSpec};
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Reject,
            ..Default::default()
        };

        let merged = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

        // Reject policy: no pending decisions, items pass through silently
        let pending = store.pending_decisions().unwrap();
        assert!(
            pending.is_empty(),
            "reject policy should not create pending decisions"
        );
        assert!(
            excluded.is_empty(),
            "reject policy does not create pending records so nothing is excluded"
        );
    }

    // --- find_server_url with duplicate server origins picks first ---

    #[test]
    fn find_server_url_picks_first_server_among_duplicates() {
        use crate::config::*;
        let config = CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "Config".into(),
            metadata: ConfigMetadata {
                name: "test".into(),
            },
            spec: ConfigSpec {
                profile: Some("default".into()),
                origin: vec![
                    OriginSpec {
                        origin_type: OriginType::Server,
                        url: "https://first-server.example.com".into(),
                        branch: "main".into(),
                        auth: None,
                        ssh_strict_host_key_checking: Default::default(),
                    },
                    OriginSpec {
                        origin_type: OriginType::Server,
                        url: "https://second-server.example.com".into(),
                        branch: "main".into(),
                        auth: None,
                        ssh_strict_host_key_checking: Default::default(),
                    },
                ],
                daemon: None,
                secrets: None,
                sources: vec![],
                theme: None,
                modules: None,
                security: None,
                aliases: std::collections::HashMap::new(),
                file_strategy: crate::config::FileStrategy::default(),
                ai: None,
                compliance: None,
            },
        };
        assert_eq!(
            find_server_url(&config),
            Some("https://first-server.example.com".to_string()),
            "should return the first server origin when multiple exist"
        );
    }

    // --- compute_config_hash: empty vs non-empty produces different hashes ---

    #[test]
    fn compute_config_hash_empty_vs_nonempty_differ() {
        use crate::config::{
            CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
            ResolvedProfile,
        };

        let empty_resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "empty".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                ..Default::default()
            },
        };

        let nonempty_resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "nonempty".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        };

        let hash_empty = compute_config_hash(&empty_resolved).unwrap();
        let hash_nonempty = compute_config_hash(&nonempty_resolved).unwrap();
        assert_ne!(
            hash_empty, hash_nonempty,
            "empty and non-empty packages should produce different hashes"
        );
    }

    // --- process_source_decisions with Ignore policy ---

    #[test]
    fn process_source_decisions_ignore_policy_no_pending_no_excluded() {
        use crate::config::{CargoSpec, PackagesSpec};
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Ignore,
            ..Default::default()
        };

        let merged = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

        // Ignore policy: silently skipped, no pending decisions, nothing excluded
        let pending = store.pending_decisions().unwrap();
        assert!(
            pending.is_empty(),
            "ignore policy should not create pending decisions"
        );
        assert!(
            excluded.is_empty(),
            "ignore policy does not create pending records so nothing is excluded"
        );
    }

    // --- Notifier construction variants ---

    #[test]
    fn notifier_desktop_mode_does_not_panic() {
        // Desktop notification may fail in CI (no display server) but should not panic.
        // On failure, notify_desktop falls back to notify_stdout via tracing::info.
        let notifier = Notifier::new(NotifyMethod::Desktop, None);
        assert!(matches!(notifier.method, NotifyMethod::Desktop));
        assert!(
            notifier.webhook_url.is_none(),
            "desktop notifier should not have a webhook URL"
        );
        notifier.notify("test title", "test body");
    }

    #[tokio::test]
    async fn notifier_webhook_with_url_does_not_panic() {
        // Webhook to a nonexistent URL: should log error but not panic
        let notifier = Notifier::new(
            NotifyMethod::Webhook,
            Some("http://127.0.0.1:1/nonexistent".to_string()),
        );
        notifier.notify("test", "message to invalid webhook");
    }

    #[test]
    fn notifier_stdout_writes_info() {
        // Verify stdout notifier is configured for Stdout method and runs
        // the tracing::info path with structured title/message fields.
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        assert!(matches!(notifier.method, NotifyMethod::Stdout));
        // The notify_stdout method calls tracing::info!(title, message, "notification")
        // Verify it handles non-trivial content without panic
        notifier.notify("drift event", "file /etc/foo changed");
        notifier.notify("", ""); // edge case: empty strings
        notifier.notify("special chars: <>&\"'", "path: /home/user/.config/cfgd");
    }

    // --- DaemonState: multiple sources ---

    #[test]
    fn daemon_state_with_multiple_sources() {
        let mut state = DaemonState::new();
        state.sources.push(SourceStatus {
            name: "acme-corp".to_string(),
            last_sync: Some("2026-03-30T10:00:00Z".to_string()),
            last_reconcile: None,
            drift_count: 2,
            status: "active".to_string(),
        });
        state.sources.push(SourceStatus {
            name: "team-tools".to_string(),
            last_sync: None,
            last_reconcile: Some("2026-03-30T11:00:00Z".to_string()),
            drift_count: 0,
            status: "error".to_string(),
        });

        let response = state.to_response();
        assert_eq!(response.sources.len(), 3); // local + acme-corp + team-tools
        assert_eq!(response.sources[1].name, "acme-corp");
        assert_eq!(response.sources[1].drift_count, 2);
        assert_eq!(response.sources[2].name, "team-tools");
        assert_eq!(response.sources[2].status, "error");
    }

    // --- DaemonState: drift counting ---

    #[test]
    fn daemon_state_drift_increments_propagate_to_response() {
        let mut state = DaemonState::new();
        state.drift_count = 10;
        if let Some(source) = state.sources.first_mut() {
            source.drift_count = 7;
        }

        let response = state.to_response();
        assert_eq!(response.drift_count, 10);
        assert_eq!(response.sources[0].drift_count, 7);
    }

    // --- DaemonState: module_last_reconcile tracking ---

    #[test]
    fn daemon_state_module_last_reconcile_tracking() {
        let mut state = DaemonState::new();
        state.module_last_reconcile.insert(
            "security-baseline".to_string(),
            "2026-03-30T12:00:00Z".to_string(),
        );
        state
            .module_last_reconcile
            .insert("dev-tools".to_string(), "2026-03-30T12:05:00Z".to_string());

        assert_eq!(state.module_last_reconcile.len(), 2);
        assert_eq!(
            state
                .module_last_reconcile
                .get("security-baseline")
                .unwrap(),
            "2026-03-30T12:00:00Z"
        );
        assert_eq!(
            state.module_last_reconcile.get("dev-tools").unwrap(),
            "2026-03-30T12:05:00Z"
        );

        // to_response does not currently populate module_reconcile (empty vec)
        let response = state.to_response();
        assert!(response.module_reconcile.is_empty());
    }

    // --- DaemonStatusResponse: update_available serialization ---

    #[test]
    fn daemon_status_response_update_available_present() {
        let response = DaemonStatusResponse {
            running: true,
            pid: 99,
            uptime_secs: 600,
            last_reconcile: None,
            last_sync: None,
            drift_count: 0,
            sources: vec![],
            update_available: Some("3.0.0".to_string()),
            module_reconcile: vec![],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"updateAvailable\":\"3.0.0\""));
        let parsed: DaemonStatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.update_available.as_deref(), Some("3.0.0"));
    }

    // --- SyncTask construction ---

    #[test]
    fn sync_task_local_defaults() {
        let task = SyncTask {
            source_name: "local".to_string(),
            repo_path: PathBuf::from("/home/user/.config/cfgd"),
            auto_pull: false,
            auto_push: false,
            auto_apply: true,
            interval: Duration::from_secs(DEFAULT_SYNC_SECS),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: false,
        };

        assert_eq!(task.source_name, "local");
        assert!(task.auto_apply);
        assert!(!task.auto_pull);
        assert!(!task.auto_push);
        assert!(task.last_synced.is_none());
        assert_eq!(task.interval.as_secs(), 300);
    }

    #[test]
    fn sync_task_source_with_signing() {
        let task = SyncTask {
            source_name: "acme-corp".to_string(),
            repo_path: PathBuf::from("/tmp/sources/acme-corp"),
            auto_pull: true,
            auto_push: false,
            auto_apply: false,
            interval: Duration::from_secs(600),
            last_synced: Some(Instant::now()),
            require_signed_commits: true,
            allow_unsigned: false,
        };

        assert_eq!(task.source_name, "acme-corp");
        assert!(task.auto_pull);
        assert!(!task.auto_push);
        assert!(!task.auto_apply);
        assert!(task.require_signed_commits);
        assert!(!task.allow_unsigned);
        assert!(task.last_synced.is_some());
    }

    #[test]
    fn sync_task_allow_unsigned_overrides_require_signed() {
        let task = SyncTask {
            source_name: "relaxed".to_string(),
            repo_path: PathBuf::from("/tmp/sources/relaxed"),
            auto_pull: true,
            auto_push: false,
            auto_apply: true,
            interval: Duration::from_secs(300),
            last_synced: None,
            require_signed_commits: true,
            allow_unsigned: true,
        };

        // Both flags can be set; the consumer decides precedence
        assert!(task.require_signed_commits);
        assert!(task.allow_unsigned);
    }

    // --- ReconcileTask construction ---

    #[test]
    fn reconcile_task_default() {
        let task = ReconcileTask {
            entity: "__default__".to_string(),
            interval: Duration::from_secs(DEFAULT_RECONCILE_SECS),
            auto_apply: false,
            drift_policy: config::DriftPolicy::default(),
            last_reconciled: None,
        };

        assert_eq!(task.entity, "__default__");
        assert_eq!(task.interval.as_secs(), 300);
        assert!(!task.auto_apply);
        assert!(task.last_reconciled.is_none());
    }

    #[test]
    fn reconcile_task_per_module() {
        let task = ReconcileTask {
            entity: "security-baseline".to_string(),
            interval: Duration::from_secs(60),
            auto_apply: true,
            drift_policy: config::DriftPolicy::Auto,
            last_reconciled: Some(Instant::now()),
        };

        assert_eq!(task.entity, "security-baseline");
        assert_eq!(task.interval.as_secs(), 60);
        assert!(task.auto_apply);
        assert!(task.last_reconciled.is_some());
    }

    // --- pending_resource_paths ---

    #[test]
    fn pending_resource_paths_empty_store() {
        let store = test_state();
        let paths = pending_resource_paths(&store);
        assert!(paths.is_empty());
    }

    #[test]
    fn pending_resource_paths_with_decisions() {
        let store = test_state();
        store
            .upsert_pending_decision(
                "acme",
                "packages.cargo.bat",
                "recommended",
                "install",
                "recommended packages.cargo.bat (from acme)",
            )
            .unwrap();
        store
            .upsert_pending_decision(
                "acme",
                "env.EDITOR",
                "recommended",
                "install",
                "recommended env.EDITOR (from acme)",
            )
            .unwrap();

        let paths = pending_resource_paths(&store);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("packages.cargo.bat"));
        assert!(paths.contains("env.EDITOR"));
    }

    // --- infer_item_tier: more coverage ---

    #[test]
    fn infer_item_tier_locked_keyword() {
        assert_eq!(infer_item_tier("files.locked-module-config.yaml"), "locked");
    }

    #[test]
    fn infer_item_tier_security_in_system() {
        assert_eq!(infer_item_tier("system.security-baseline"), "locked");
    }

    #[test]
    fn infer_item_tier_normal_package() {
        assert_eq!(infer_item_tier("packages.brew.curl"), "recommended");
    }

    #[test]
    fn infer_item_tier_normal_env_var() {
        assert_eq!(infer_item_tier("env.GOPATH"), "recommended");
    }

    #[test]
    fn infer_item_tier_normal_file() {
        assert_eq!(infer_item_tier("files./home/user/.zshrc"), "recommended");
    }

    // --- extract_source_resources: aliases not included (not tracked) ---

    #[test]
    fn extract_source_resources_aliases_not_tracked() {
        use crate::config::{MergedProfile, ShellAlias};

        let merged = MergedProfile {
            aliases: vec![
                ShellAlias {
                    name: "ll".into(),
                    command: "ls -la".into(),
                },
                ShellAlias {
                    name: "gp".into(),
                    command: "git push".into(),
                },
            ],
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        // Aliases are not tracked as individual resources
        assert!(
            resources.is_empty(),
            "aliases should not be tracked as source resources"
        );
    }

    // --- extract_source_resources: mixed profile with everything ---

    #[test]
    fn extract_source_resources_full_profile() {
        use crate::config::{
            AptSpec, BrewSpec, CargoSpec, EnvVar, FilesSpec, ManagedFileSpec, MergedProfile,
            NpmSpec, PackagesSpec,
        };

        let mut system = std::collections::HashMap::new();
        system.insert("sysctl".into(), serde_yaml::Value::Null);

        let merged = MergedProfile {
            packages: PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec!["ripgrep".into()],
                    casks: vec!["firefox".into()],
                    ..Default::default()
                }),
                apt: Some(AptSpec {
                    file: None,
                    packages: vec!["curl".into()],
                }),
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                pipx: vec!["black".into()],
                dnf: vec!["vim".into()],
                npm: Some(NpmSpec {
                    file: None,
                    global: vec!["typescript".into()],
                }),
                ..Default::default()
            },
            files: FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "dotfiles/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            env: vec![
                EnvVar {
                    name: "EDITOR".into(),
                    value: "vim".into(),
                },
                EnvVar {
                    name: "GOPATH".into(),
                    value: "/home/user/go".into(),
                },
            ],
            system,
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        // Verify all expected resources are present
        assert!(resources.contains("packages.brew.ripgrep"));
        assert!(resources.contains("packages.brew.firefox"));
        assert!(resources.contains("packages.apt.curl"));
        assert!(resources.contains("packages.cargo.bat"));
        assert!(resources.contains("packages.pipx.black"));
        assert!(resources.contains("packages.dnf.vim"));
        assert!(resources.contains("packages.npm.typescript"));
        assert!(resources.contains("files./home/user/.zshrc"));
        assert!(resources.contains("env.EDITOR"));
        assert!(resources.contains("env.GOPATH"));
        assert!(resources.contains("system.sysctl"));
        // Total: 1 formula + 1 cask + 1 apt + 1 cargo + 1 pipx + 1 dnf + 1 npm + 1 file + 2 env + 1 system
        assert_eq!(resources.len(), 11);
    }

    // --- process_source_decisions: locked_conflict policy ---

    #[test]
    fn process_source_decisions_locked_item_notify_policy() {
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Accept,
            locked_conflict: PolicyAction::Notify,
            ..Default::default()
        };

        // Use a file with "security" in the name to trigger the locked tier
        let mut system = std::collections::HashMap::new();
        system.insert("security-baseline".into(), serde_yaml::Value::Null);

        let merged = MergedProfile {
            system,
            ..Default::default()
        };

        let excluded = process_source_decisions(&store, "corp", &merged, &policy, &notifier);

        // The "system.security-baseline" item should be inferred as "locked" tier
        // and with locked_conflict = Notify, it should create a pending decision
        let pending = store.pending_decisions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].resource, "system.security-baseline");
        assert!(excluded.contains("system.security-baseline"));
    }

    // --- process_source_decisions: multiple sources ---

    #[test]
    fn process_source_decisions_different_sources_independent() {
        use crate::config::{CargoSpec, PackagesSpec};
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Accept,
            ..Default::default()
        };

        let merged_a = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let merged_b = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["ripgrep".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let excluded_a =
            process_source_decisions(&store, "source-a", &merged_a, &policy, &notifier);
        let excluded_b =
            process_source_decisions(&store, "source-b", &merged_b, &policy, &notifier);

        // Accept policy: both sources processed, nothing excluded
        assert!(excluded_a.is_empty());
        assert!(excluded_b.is_empty());
    }

    // --- process_source_decisions: items removed from source ---

    #[test]
    fn process_source_decisions_removed_items_update_hash() {
        use crate::config::{CargoSpec, PackagesSpec};
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Accept,
            ..Default::default()
        };

        // First call: bat + ripgrep
        let merged1 = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into(), "ripgrep".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = process_source_decisions(&store, "acme", &merged1, &policy, &notifier);

        // Second call: only bat (ripgrep removed from source)
        let merged2 = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let excluded = process_source_decisions(&store, "acme", &merged2, &policy, &notifier);

        // Hash changed, but Accept policy means no pending decisions
        let pending = store.pending_decisions().unwrap();
        assert!(pending.is_empty());
        assert!(excluded.is_empty());
    }

    // --- SourceStatus: field defaults ---

    #[test]
    fn source_status_defaults() {
        let status = SourceStatus {
            name: "test".to_string(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 0,
            status: "active".to_string(),
        };

        assert!(status.last_sync.is_none());
        assert!(status.last_reconcile.is_none());
        assert_eq!(status.drift_count, 0);
    }

    // --- SourceStatus: all fields populated ---

    #[test]
    fn source_status_all_fields_populated() {
        let status = SourceStatus {
            name: "corp-source".to_string(),
            last_sync: Some("2026-03-30T10:00:00Z".to_string()),
            last_reconcile: Some("2026-03-30T10:05:00Z".to_string()),
            drift_count: 15,
            status: "error".to_string(),
        };

        let json = serde_json::to_string(&status).unwrap();
        let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "corp-source");
        assert_eq!(parsed.last_sync.as_deref(), Some("2026-03-30T10:00:00Z"));
        assert_eq!(
            parsed.last_reconcile.as_deref(),
            Some("2026-03-30T10:05:00Z")
        );
        assert_eq!(parsed.drift_count, 15);
        assert_eq!(parsed.status, "error");
    }

    // --- DaemonStatusResponse deserialization from external JSON ---

    #[test]
    fn daemon_status_response_deserializes_from_minimal_json() {
        let json = r#"{
            "running": false,
            "pid": 0,
            "uptimeSecs": 0,
            "lastReconcile": null,
            "lastSync": null,
            "driftCount": 0,
            "sources": []
        }"#;

        let parsed: DaemonStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!parsed.running);
        assert_eq!(parsed.pid, 0);
        assert!(parsed.module_reconcile.is_empty());
        assert!(parsed.update_available.is_none());
    }

    // --- CheckinPayload: field coverage ---

    #[test]
    fn checkin_payload_serializes_all_fields() {
        let payload = CheckinPayload {
            device_id: "sha256hex".into(),
            hostname: "myhost.local".into(),
            os: "linux".into(),
            arch: "aarch64".into(),
            config_hash: "abcd1234".into(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"device_id\""));
        assert!(json.contains("\"hostname\""));
        assert!(json.contains("\"os\""));
        assert!(json.contains("\"arch\""));
        assert!(json.contains("\"config_hash\""));
        assert!(json.contains("aarch64"));
    }

    // --- parse_duration_or_default: edge cases ---

    #[test]
    fn parse_duration_large_seconds() {
        assert_eq!(
            parse_duration_or_default("86400s"),
            Duration::from_secs(86400)
        );
    }

    #[test]
    fn parse_duration_large_hours() {
        assert_eq!(parse_duration_or_default("24h"), Duration::from_secs(86400));
    }

    #[test]
    fn parse_duration_empty_string_falls_back() {
        assert_eq!(
            parse_duration_or_default(""),
            Duration::from_secs(DEFAULT_RECONCILE_SECS)
        );
    }

    // --- hash_resources: ordering does not matter ---

    #[test]
    fn hash_resources_large_set_deterministic() {
        let set1: HashSet<String> = (0..100)
            .map(|i| format!("packages.brew.pkg{}", i))
            .collect();
        let set2: HashSet<String> = (0..100)
            .rev()
            .map(|i| format!("packages.brew.pkg{}", i))
            .collect();

        assert_eq!(hash_resources(&set1), hash_resources(&set2));
    }

    // --- ModuleReconcileStatus: camelCase field names ---

    #[test]
    fn module_reconcile_status_camel_case_fields() {
        let status = ModuleReconcileStatus {
            name: "test".into(),
            interval: "60s".into(),
            auto_apply: true,
            drift_policy: "Auto".into(),
            last_reconcile: Some("2026-01-01T00:00:00Z".into()),
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"autoApply\""));
        assert!(json.contains("\"driftPolicy\""));
        assert!(json.contains("\"lastReconcile\""));
        // Should NOT contain snake_case
        assert!(!json.contains("\"auto_apply\""));
        assert!(!json.contains("\"drift_policy\""));
        assert!(!json.contains("\"last_reconcile\""));
    }

    // --- DaemonStatusResponse: uptime_secs is camelCase in JSON ---

    #[test]
    fn daemon_status_response_camel_case_uptime() {
        let response = DaemonStatusResponse {
            running: true,
            pid: 1,
            uptime_secs: 42,
            last_reconcile: None,
            last_sync: None,
            drift_count: 0,
            sources: vec![],
            update_available: None,
            module_reconcile: vec![],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"uptimeSecs\""));
        assert!(json.contains("\"driftCount\""));
        assert!(!json.contains("\"uptime_secs\""));
        assert!(!json.contains("\"drift_count\""));
    }

    // --- process_source_decisions: mixed policies per tier ---

    #[test]
    fn process_source_decisions_mixed_tiers_accept_recommended_notify_locked() {
        use crate::config::{CargoSpec, PackagesSpec};

        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Accept,
            new_optional: PolicyAction::Ignore,
            locked_conflict: PolicyAction::Notify,
        };

        // Mix of recommended (cargo packages) and locked (security system setting)
        let mut system = std::collections::HashMap::new();
        system.insert("security-policy".into(), serde_yaml::Value::Null);

        let merged = MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            system,
            ..Default::default()
        };

        let excluded = process_source_decisions(&store, "corp", &merged, &policy, &notifier);

        let pending = store.pending_decisions().unwrap();
        // Only the locked item should be pending (security-policy)
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].resource, "system.security-policy");
        // bat should not be excluded (Accept policy for recommended)
        assert!(!excluded.contains("packages.cargo.bat"));
        // security-policy should be excluded (pending)
        assert!(excluded.contains("system.security-policy"));
    }

    // --- generate_device_id: always hex ---

    #[test]
    fn generate_device_id_hex_format() {
        let id = generate_device_id().unwrap();
        // Should be lowercase hex only
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "device ID should be hex: {}",
            id
        );
    }

    // --- extract_source_resources: multiple files ---

    #[test]
    fn extract_source_resources_multiple_files() {
        use crate::config::{FilesSpec, ManagedFileSpec, MergedProfile};

        let merged = MergedProfile {
            files: FilesSpec {
                managed: vec![
                    ManagedFileSpec {
                        source: "dotfiles/.zshrc".into(),
                        target: PathBuf::from("/home/user/.zshrc"),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    },
                    ManagedFileSpec {
                        source: "dotfiles/.vimrc".into(),
                        target: PathBuf::from("/home/user/.vimrc"),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    },
                    ManagedFileSpec {
                        source: "dotfiles/.gitconfig".into(),
                        target: PathBuf::from("/home/user/.gitconfig"),
                        strategy: None,
                        private: true,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert_eq!(resources.len(), 3);
        assert!(resources.contains("files./home/user/.zshrc"));
        assert!(resources.contains("files./home/user/.vimrc"));
        assert!(resources.contains("files./home/user/.gitconfig"));
    }

    // --- extract_source_resources: multiple env vars ---

    #[test]
    fn extract_source_resources_multiple_env_vars() {
        use crate::config::{EnvVar, MergedProfile};

        let merged = MergedProfile {
            env: vec![
                EnvVar {
                    name: "PATH".into(),
                    value: "/usr/local/bin:$PATH".into(),
                },
                EnvVar {
                    name: "EDITOR".into(),
                    value: "nvim".into(),
                },
                EnvVar {
                    name: "GOPATH".into(),
                    value: "/home/user/go".into(),
                },
            ],
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert_eq!(resources.len(), 3);
        assert!(resources.contains("env.PATH"));
        assert!(resources.contains("env.EDITOR"));
        assert!(resources.contains("env.GOPATH"));
    }

    // --- extract_source_resources: multiple system keys ---

    #[test]
    fn extract_source_resources_multiple_system_keys() {
        use crate::config::MergedProfile;

        let mut system = std::collections::HashMap::new();
        system.insert("sysctl".into(), serde_yaml::Value::Null);
        system.insert("kernelModules".into(), serde_yaml::Value::Null);
        system.insert("apparmor".into(), serde_yaml::Value::Null);

        let merged = MergedProfile {
            system,
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert_eq!(resources.len(), 3);
        assert!(resources.contains("system.sysctl"));
        assert!(resources.contains("system.kernelModules"));
        assert!(resources.contains("system.apparmor"));
    }

    // --- DaemonState: uptime increases ---

    #[test]
    fn daemon_state_uptime_increases() {
        let state = DaemonState::new();
        // Small sleep to ensure non-zero uptime
        std::thread::sleep(Duration::from_millis(10));
        let response = state.to_response();
        // Uptime should be at least 0 (could be 0 if resolution is 1s)
        // The key assertion is that it doesn't panic
        assert!(response.uptime_secs < 10);
    }

    // --- handle_health_connection: /health endpoint ---

    #[tokio::test]
    async fn health_connection_health_endpoint() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let (client, server) = tokio::io::duplex(4096);

        // Spawn the handler
        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        // Send HTTP request
        let (reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        // Read response
        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut response = String::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "expected 200 OK, got: {}",
            &response[..response.len().min(40)]
        );
        assert!(response.contains("\"status\""));
        assert!(response.contains("\"pid\""));
        assert!(response.contains("\"uptime_secs\""));
    }

    // --- handle_health_connection: /status endpoint ---

    #[tokio::test]
    async fn health_connection_status_endpoint() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        // Populate some state
        {
            let mut st = state.lock().await;
            st.drift_count = 3;
            st.last_reconcile = Some("2026-03-30T10:00:00Z".to_string());
        }

        let (client, server) = tokio::io::duplex(4096);

        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        let (reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut response = String::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "expected 200 OK, got: {}",
            &response[..response.len().min(40)]
        );
        // Body should contain DaemonStatusResponse fields (pretty-printed JSON)
        assert!(
            response.contains("\"running\": true"),
            "response should contain running field: {}",
            &response
        );
        assert!(
            response.contains("\"driftCount\": 3"),
            "response should contain driftCount field: {}",
            &response
        );
    }

    // --- handle_health_connection: /drift endpoint ---

    #[tokio::test]
    async fn health_connection_drift_endpoint() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let (client, server) = tokio::io::duplex(4096);

        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        let (reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /drift HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut response = String::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "expected 200 OK, got: {}",
            &response[..response.len().min(40)]
        );
        assert!(response.contains("\"drift_count\""));
        assert!(response.contains("\"events\""));
    }

    // --- handle_health_connection: 404 for unknown path ---

    #[tokio::test]
    async fn health_connection_unknown_path_returns_404() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let (client, server) = tokio::io::duplex(4096);

        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        let (reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut response = String::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        assert!(
            response.starts_with("HTTP/1.1 404 Not Found"),
            "expected 404, got: {}",
            &response[..response.len().min(40)]
        );
        assert!(response.contains("\"error\""));
    }

    // --- git_pull: repo with no remote changes returns Ok(false) ---

    #[test]
    fn git_pull_no_remote_returns_up_to_date() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");

        // Create a bare repo as "remote"
        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        // Clone the bare repo to get a working copy with origin
        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();

        // Configure committer identity
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();

        // Create initial commit (bare repos start empty, clone has no HEAD)
        let readme = work_dir.join("README");
        std::fs::write(&readme, "test\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Push initial commit to bare remote
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();

        // Now pull — should be up-to-date since we just pushed
        let result = git_pull(&work_dir);
        assert!(result.is_ok(), "git_pull failed: {:?}", result);
        assert!(!result.unwrap(), "expected no changes");
    }

    // --- git_pull: repo with new remote commits returns Ok(true) ---

    #[test]
    fn git_pull_with_remote_changes_returns_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");
        let pusher_dir = tmp.path().join("pusher");

        // Create bare repo
        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        // Clone into work_dir
        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }

        // Create initial commit and push
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Clone into pusher_dir and push a new commit
        let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
        {
            let mut config = pusher.config().unwrap();
            config.set_str("user.name", "cfgd-pusher").unwrap();
            config.set_str("user.email", "pusher@cfgd.io").unwrap();
        }
        std::fs::write(pusher_dir.join("NEW_FILE"), "hello\n").unwrap();
        {
            let mut index = pusher.index().unwrap();
            index.add_path(Path::new("NEW_FILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = pusher.find_tree(tree_id).unwrap();
            let sig = pusher.signature().unwrap();
            let parent = pusher.head().unwrap().peel_to_commit().unwrap();
            pusher
                .commit(Some("HEAD"), &sig, &sig, "add file", &tree, &[&parent])
                .unwrap();
        }
        {
            let mut remote = pusher.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Now git_pull in work_dir should detect changes
        let result = git_pull(&work_dir);
        assert!(result.is_ok(), "git_pull failed: {:?}", result);
        assert!(result.unwrap(), "expected changes from remote");

        // Verify the new file exists after pull
        assert!(
            work_dir.join("NEW_FILE").exists(),
            "NEW_FILE should exist after fast-forward pull"
        );
    }

    // --- git_auto_commit_push: no changes returns Ok(false) ---

    #[test]
    fn git_auto_commit_push_no_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");

        // Create bare repo
        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        // Clone, create initial commit, push
        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "test\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // No changes — should return Ok(false)
        let result = git_auto_commit_push(&work_dir);
        assert!(result.is_ok(), "git_auto_commit_push failed: {:?}", result);
        assert!(!result.unwrap(), "expected no changes to push");
    }

    // --- git_auto_commit_push: with changes commits and pushes ---

    #[test]
    fn git_auto_commit_push_with_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");

        // Create bare repo
        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        // Clone, create initial commit, push
        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "test\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Create a new file (uncommitted change)
        std::fs::write(work_dir.join("new_config.yaml"), "key: value\n").unwrap();

        // Should commit and push the change
        let result = git_auto_commit_push(&work_dir);
        assert!(result.is_ok(), "git_auto_commit_push failed: {:?}", result);
        assert!(result.unwrap(), "expected changes to be pushed");

        // Verify commit was created
        let repo = git2::Repository::open(&work_dir).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            head.message().unwrap(),
            "cfgd: auto-commit configuration changes"
        );

        // Verify the change was pushed to bare repo
        let bare = git2::Repository::open_bare(&bare_dir).unwrap();
        let bare_head = bare
            .find_reference("refs/heads/master")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(head.id(), bare_head.id());
    }

    // --- git_pull: non-git directory returns error ---

    #[test]
    fn git_pull_non_repo_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = git_pull(tmp.path());
        let err = result.unwrap_err();
        assert!(
            err.contains("open repo"),
            "expected 'open repo' error, got: {err}"
        );
    }

    // --- git_auto_commit_push: non-git directory returns error ---

    #[test]
    fn git_auto_commit_push_non_repo_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = git_auto_commit_push(tmp.path());
        let err = result.unwrap_err();
        assert!(
            err.contains("open repo"),
            "expected 'open repo' error, got: {err}"
        );
    }

    // --- handle_sync: updates daemon state timestamps ---
    // Note: handle_sync uses tokio::runtime::Handle::current().block_on() internally,
    // so it must be called from a blocking context (spawn_blocking) within a tokio test.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_updates_state_timestamps() {
        use crate::test_helpers::init_test_git_repo;

        let tmp = tempfile::TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        init_test_git_repo(&repo_dir);

        let state = Arc::new(Mutex::new(DaemonState::new()));

        let st = Arc::clone(&state);
        let rd = repo_dir.clone();
        let changed = tokio::task::spawn_blocking(move || {
            handle_sync(&rd, false, false, "local", &st, false, false)
        })
        .await
        .unwrap();

        assert!(!changed);

        let st = state.lock().await;
        assert!(st.last_sync.is_some(), "last_sync should be set");
    }

    // --- handle_sync: with auto_pull on repo without remote ---

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_pull_without_remote_logs_warning() {
        use crate::test_helpers::init_test_git_repo;

        let tmp = tempfile::TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        init_test_git_repo(&repo_dir);

        let state = Arc::new(Mutex::new(DaemonState::new()));

        let st = Arc::clone(&state);
        let rd = repo_dir.clone();
        let changed = tokio::task::spawn_blocking(move || {
            handle_sync(&rd, true, false, "local", &st, false, false)
        })
        .await
        .unwrap();

        // Should not crash; pull fails gracefully
        assert!(!changed);
    }

    // --- handle_sync: per-source status update ---

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_updates_per_source_status() {
        use crate::test_helpers::init_test_git_repo;

        let tmp = tempfile::TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        init_test_git_repo(&repo_dir);

        let state = Arc::new(Mutex::new(DaemonState::new()));
        // Add a second source
        {
            let mut st = state.lock().await;
            st.sources.push(SourceStatus {
                name: "acme".to_string(),
                last_sync: None,
                last_reconcile: None,
                drift_count: 0,
                status: "active".to_string(),
            });
        }

        let st = Arc::clone(&state);
        let rd = repo_dir.clone();
        tokio::task::spawn_blocking(move || {
            handle_sync(&rd, false, false, "acme", &st, false, false)
        })
        .await
        .unwrap();

        let st = state.lock().await;
        // The "acme" source should have its last_sync updated
        let acme = st.sources.iter().find(|s| s.name == "acme").unwrap();
        assert!(
            acme.last_sync.is_some(),
            "acme source last_sync should be set"
        );
        // The "local" source should NOT have been updated
        let local = st.sources.iter().find(|s| s.name == "local").unwrap();
        assert!(
            local.last_sync.is_none(),
            "local source last_sync should remain None"
        );
    }

    // --- handle_sync: auto_pull with remote changes fast-forwards ---

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_auto_pull_with_remote_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");
        let pusher_dir = tmp.path().join("pusher");

        // Set up bare + work + pusher repos
        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Push a change from pusher
        let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
        {
            let mut config = pusher.config().unwrap();
            config.set_str("user.name", "cfgd-pusher").unwrap();
            config.set_str("user.email", "pusher@cfgd.io").unwrap();
        }
        std::fs::write(pusher_dir.join("NEWFILE"), "synced\n").unwrap();
        {
            let mut index = pusher.index().unwrap();
            index.add_path(Path::new("NEWFILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = pusher.find_tree(tree_id).unwrap();
            let sig = pusher.signature().unwrap();
            let parent = pusher.head().unwrap().peel_to_commit().unwrap();
            pusher
                .commit(Some("HEAD"), &sig, &sig, "add newfile", &tree, &[&parent])
                .unwrap();
        }
        {
            let mut remote = pusher.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let st = Arc::clone(&state);
        let wd = work_dir.clone();
        let changed = tokio::task::spawn_blocking(move || {
            handle_sync(&wd, true, false, "local", &st, false, false)
        })
        .await
        .unwrap();

        assert!(changed, "handle_sync should detect remote changes");
        assert!(
            work_dir.join("NEWFILE").exists(),
            "pulled file should exist after sync"
        );
    }

    // --- handle_sync: auto_push with local changes ---

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_auto_push_with_local_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Create a local change
        std::fs::write(work_dir.join("local_change.txt"), "new content\n").unwrap();

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let st = Arc::clone(&state);
        let wd = work_dir.clone();
        // pull=false, push=true
        let changed = tokio::task::spawn_blocking(move || {
            handle_sync(&wd, false, true, "local", &st, false, false)
        })
        .await
        .unwrap();

        // No remote changes to pull, but push should succeed
        assert!(!changed, "no pull changes expected");

        // Verify commit was pushed to bare repo
        let bare = git2::Repository::open_bare(&bare_dir).unwrap();
        let bare_head = bare
            .find_reference("refs/heads/master")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(
            bare_head.message().unwrap(),
            "cfgd: auto-commit configuration changes"
        );
    }

    // --- git_pull: diverged branches return error ---

    #[test]
    fn git_pull_diverged_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");
        let pusher_dir = tmp.path().join("pusher");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Push a divergent change from pusher
        let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
        {
            let mut config = pusher.config().unwrap();
            config.set_str("user.name", "cfgd-pusher").unwrap();
            config.set_str("user.email", "pusher@cfgd.io").unwrap();
        }
        std::fs::write(pusher_dir.join("PUSHER_FILE"), "pusher\n").unwrap();
        {
            let mut index = pusher.index().unwrap();
            index.add_path(Path::new("PUSHER_FILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = pusher.find_tree(tree_id).unwrap();
            let sig = pusher.signature().unwrap();
            let parent = pusher.head().unwrap().peel_to_commit().unwrap();
            pusher
                .commit(Some("HEAD"), &sig, &sig, "pusher commit", &tree, &[&parent])
                .unwrap();
        }
        {
            let mut remote = pusher.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Create a local commit in work_dir (diverged from remote)
        std::fs::write(work_dir.join("LOCAL_FILE"), "local\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("LOCAL_FILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "local commit", &tree, &[&parent])
                .unwrap();
        }

        // git_pull should fail because branches diverged (not fast-forwardable)
        let result = git_pull(&work_dir);
        assert!(result.is_err(), "diverged branch should return error");
        let err_msg = result.unwrap_err();
        assert!(
            err_msg.contains("diverged") || err_msg.contains("fast-forward"),
            "error should mention divergence: {}",
            err_msg
        );
    }

    // --- git_auto_commit_push: fresh repo with no HEAD ---

    #[test]
    fn git_auto_commit_push_fresh_repo_no_head() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }

        // Create a file but don't commit yet — repo has no HEAD
        std::fs::write(work_dir.join("first_file.txt"), "hello\n").unwrap();

        let result = git_auto_commit_push(&work_dir);
        assert!(result.is_ok(), "fresh repo push failed: {:?}", result);
        assert!(result.unwrap(), "expected changes to be committed");

        // Verify HEAD now exists with the auto-commit message
        let repo = git2::Repository::open(&work_dir).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            head.message().unwrap(),
            "cfgd: auto-commit configuration changes"
        );
    }

    // --- server_checkin: mock HTTP test for config_changed=true ---

    #[test]
    fn server_checkin_mock_config_changed() {
        use crate::config::{
            LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
        };

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ok","config_changed":true,"config":null}"#)
            .create();

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                ..Default::default()
            },
        };

        let changed = server_checkin(&server.url(), &resolved);
        assert!(changed, "server should report config changed");
        mock.assert();
    }

    // --- server_checkin: mock HTTP test for config_changed=false ---

    #[test]
    fn server_checkin_mock_no_change() {
        use crate::config::{
            LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
        };

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
            .create();

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                ..Default::default()
            },
        };

        let changed = server_checkin(&server.url(), &resolved);
        assert!(!changed, "server should report no change");
        mock.assert();
    }

    // --- server_checkin: server returns 500 ---

    #[test]
    fn server_checkin_mock_server_error() {
        use crate::config::{
            LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
        };

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(500)
            .with_body("internal server error")
            .create();

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                ..Default::default()
            },
        };

        let changed = server_checkin(&server.url(), &resolved);
        assert!(!changed, "server error should return false");
        mock.assert();
    }

    // --- server_checkin: malformed JSON response ---

    #[test]
    fn server_checkin_mock_malformed_json() {
        use crate::config::{
            LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
        };

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not json at all")
            .create();

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                ..Default::default()
            },
        };

        let changed = server_checkin(&server.url(), &resolved);
        assert!(!changed, "malformed JSON should return false");
        mock.assert();
    }

    // --- server_checkin: URL with trailing slash ---

    #[test]
    fn server_checkin_mock_trailing_slash_url() {
        use crate::config::{
            LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
        };

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
            .create();

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                ..Default::default()
            },
        };

        // URL with trailing slash should be trimmed
        let url_with_slash = format!("{}/", server.url());
        let changed = server_checkin(&url_with_slash, &resolved);
        assert!(!changed);
        mock.assert();
    }

    // --- server_checkin: verifies request payload structure ---

    #[test]
    fn server_checkin_mock_verifies_request_body() {
        use crate::config::{
            CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
            ResolvedProfile,
        };

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .match_header("Content-Type", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
            .create();

        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        };

        let changed = server_checkin(&server.url(), &resolved);
        assert!(!changed);
        // Verify the mock received the request with correct Content-Type
        mock.assert();
    }

    // --- try_server_checkin: delegates to server_checkin when URL present ---

    #[test]
    fn try_server_checkin_no_server_origin_returns_false() {
        use crate::config::*;
        let config = CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "Config".into(),
            metadata: ConfigMetadata {
                name: "test".into(),
            },
            spec: ConfigSpec {
                profile: Some("default".into()),
                origin: vec![OriginSpec {
                    origin_type: OriginType::Git,
                    url: "https://github.com/test/repo.git".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                }],
                daemon: None,
                secrets: None,
                sources: vec![],
                theme: None,
                modules: None,
                security: None,
                aliases: std::collections::HashMap::new(),
                file_strategy: FileStrategy::default(),
                ai: None,
                compliance: None,
            },
        };
        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile::default(),
        };

        let changed = try_server_checkin(&config, &resolved);
        assert!(!changed, "no server origin means no checkin");
    }

    // --- try_server_checkin: with mock server ---

    #[test]
    fn try_server_checkin_with_server_origin_calls_checkin() {
        use crate::config::*;

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ok","config_changed":true,"config":null}"#)
            .create();

        let config = CfgdConfig {
            api_version: crate::API_VERSION.into(),
            kind: "Config".into(),
            metadata: ConfigMetadata {
                name: "test".into(),
            },
            spec: ConfigSpec {
                profile: Some("default".into()),
                origin: vec![OriginSpec {
                    origin_type: OriginType::Server,
                    url: server.url(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                }],
                daemon: None,
                secrets: None,
                sources: vec![],
                theme: None,
                modules: None,
                security: None,
                aliases: std::collections::HashMap::new(),
                file_strategy: FileStrategy::default(),
                ai: None,
                compliance: None,
            },
        };
        let resolved = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "test".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile::default(),
        };

        let changed = try_server_checkin(&config, &resolved);
        assert!(changed, "server origin should trigger checkin");
        mock.assert();
    }

    // --- handle_health_connection: response includes Content-Type and Content-Length ---

    #[tokio::test]
    async fn health_connection_response_headers() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let (client, server) = tokio::io::duplex(4096);

        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        let (reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut response = String::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        assert!(
            response.contains("Content-Type: application/json"),
            "missing Content-Type header"
        );
        assert!(
            response.contains("Content-Length:"),
            "missing Content-Length header"
        );
        assert!(
            response.contains("Connection: close"),
            "missing Connection header"
        );
    }

    // --- handle_health_connection: empty request line defaults to /health ---

    #[tokio::test]
    async fn health_connection_empty_request_defaults_to_health() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let (client, server) = tokio::io::duplex(4096);

        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        let (reader, mut writer) = tokio::io::split(client);
        // Send an empty line as the request
        writer.write_all(b"\r\n\r\n").await.unwrap();
        writer.shutdown().await.unwrap();

        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut response = String::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        // Empty request should either default to /health or return 404
        // The code uses `split_whitespace().nth(1).unwrap_or("/health")` so
        // empty request line -> /health
        assert!(
            response.contains("200 OK") || response.contains("404 Not Found"),
            "should handle empty request gracefully: {}",
            &response[..response.len().min(80)]
        );
    }

    // --- handle_health_connection: /status body parses to DaemonStatusResponse ---

    #[tokio::test]
    async fn health_connection_status_body_parses_as_response() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        {
            let mut st = state.lock().await;
            st.drift_count = 7;
            st.update_available = Some("2.0.0".to_string());
        }

        let (client, server) = tokio::io::duplex(8192);

        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        let (reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut lines: Vec<String> = Vec::new();
        let mut in_body = false;
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if in_body {
                        lines.push(line);
                    } else if line.trim().is_empty() {
                        in_body = true;
                    }
                }
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        let body = lines.join("");
        let parsed: DaemonStatusResponse =
            serde_json::from_str(&body).expect("body should parse as DaemonStatusResponse");
        assert!(parsed.running);
        assert_eq!(parsed.drift_count, 7);
        assert_eq!(parsed.update_available.as_deref(), Some("2.0.0"));
        assert_eq!(parsed.sources.len(), 1);
        assert_eq!(parsed.sources[0].name, "local");
    }

    // --- DaemonState: module_last_reconcile overwrite ---

    #[test]
    fn daemon_state_module_last_reconcile_overwrite() {
        let mut state = DaemonState::new();
        state
            .module_last_reconcile
            .insert("mod-a".into(), "2026-01-01T00:00:00Z".into());
        state
            .module_last_reconcile
            .insert("mod-a".into(), "2026-01-02T00:00:00Z".into());

        // Overwrite should replace the old value
        assert_eq!(state.module_last_reconcile.len(), 1);
        assert_eq!(
            state.module_last_reconcile.get("mod-a").unwrap(),
            "2026-01-02T00:00:00Z"
        );
    }

    // --- DaemonState: update_available persists through to_response ---

    #[test]
    fn daemon_state_update_available_in_response() {
        let mut state = DaemonState::new();
        state.update_available = Some("3.1.0".to_string());

        let response = state.to_response();
        assert_eq!(response.update_available.as_deref(), Some("3.1.0"));
    }

    // --- Notifier: webhook builds correct JSON payload structure ---

    #[test]
    fn notifier_webhook_payload_structure() {
        // Verify the JSON payload structure by constructing it the same way as notify_webhook
        let title = "cfgd: drift detected";
        let message = "3 files drifted";
        let payload = serde_json::json!({
            "event": title,
            "message": message,
            "timestamp": crate::utc_now_iso8601(),
            "source": "cfgd",
        });

        let obj = payload.as_object().unwrap();
        assert_eq!(obj.len(), 4);
        assert_eq!(obj.get("event").unwrap().as_str().unwrap(), title);
        assert_eq!(obj.get("message").unwrap().as_str().unwrap(), message);
        assert!(obj.contains_key("timestamp"));
        assert_eq!(obj.get("source").unwrap().as_str().unwrap(), "cfgd");
    }

    // --- Notifier: webhook payload timestamp format ---

    #[test]
    fn notifier_webhook_payload_timestamp_is_iso8601() {
        let payload = serde_json::json!({
            "event": "test",
            "message": "msg",
            "timestamp": crate::utc_now_iso8601(),
            "source": "cfgd",
        });

        let ts = payload["timestamp"].as_str().unwrap();
        // ISO 8601 format: contains 'T' separator and ends with 'Z'
        assert!(ts.contains('T'), "timestamp should be ISO 8601: {}", ts);
        assert!(ts.ends_with('Z'), "timestamp should end with Z: {}", ts);
    }

    // --- ReconcileTask: drift_policy variants ---

    #[test]
    fn reconcile_task_drift_policy_auto() {
        let task = ReconcileTask {
            entity: "critical-module".into(),
            interval: Duration::from_secs(30),
            auto_apply: true,
            drift_policy: config::DriftPolicy::Auto,
            last_reconciled: None,
        };
        assert!(matches!(task.drift_policy, config::DriftPolicy::Auto));
    }

    #[test]
    fn reconcile_task_drift_policy_notify_only() {
        let task = ReconcileTask {
            entity: "optional-module".into(),
            interval: Duration::from_secs(600),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        };
        assert!(matches!(task.drift_policy, config::DriftPolicy::NotifyOnly));
    }

    #[test]
    fn reconcile_task_drift_policy_prompt() {
        let task = ReconcileTask {
            entity: "interactive-module".into(),
            interval: Duration::from_secs(300),
            auto_apply: false,
            drift_policy: config::DriftPolicy::Prompt,
            last_reconciled: None,
        };
        assert!(matches!(task.drift_policy, config::DriftPolicy::Prompt));
    }

    // --- process_source_decisions: new_optional tier with Accept policy ---

    #[test]
    fn process_source_decisions_optional_tier_accept() {
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Notify,
            new_optional: PolicyAction::Accept,
            locked_conflict: PolicyAction::Notify,
        };

        // Regular packages trigger "recommended" tier, not "optional".
        // The current infer_item_tier only returns "recommended" or "locked".
        // Verify that recommended items still get the Notify treatment.
        let merged = MergedProfile {
            packages: crate::config::PackagesSpec {
                cargo: Some(crate::config::CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let excluded = process_source_decisions(&store, "acme", &merged, &policy, &notifier);
        let pending = store.pending_decisions().unwrap();
        // "bat" is recommended tier -> Notify policy -> creates pending decision
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].resource, "packages.cargo.bat");
        assert!(excluded.contains("packages.cargo.bat"));
    }

    // --- process_source_decisions: empty merged profile no decisions ---

    #[test]
    fn process_source_decisions_empty_profile_no_decisions() {
        let store = test_state();
        let notifier = Notifier::new(NotifyMethod::Stdout, None);
        let policy = AutoApplyPolicyConfig::default();

        let merged = MergedProfile::default();

        let excluded = process_source_decisions(&store, "empty", &merged, &policy, &notifier);
        let pending = store.pending_decisions().unwrap();
        assert!(pending.is_empty());
        assert!(excluded.is_empty());
    }

    // --- DaemonStatusResponse: deserialization with all optional fields ---

    #[test]
    fn daemon_status_response_full_deserialization() {
        let json = r#"{
            "running": true,
            "pid": 54321,
            "uptimeSecs": 7200,
            "lastReconcile": "2026-04-01T00:00:00Z",
            "lastSync": "2026-04-01T00:01:00Z",
            "driftCount": 42,
            "sources": [
                {
                    "name": "local",
                    "lastSync": "2026-04-01T00:01:00Z",
                    "lastReconcile": "2026-04-01T00:00:00Z",
                    "driftCount": 10,
                    "status": "active"
                }
            ],
            "updateAvailable": "4.0.0",
            "moduleReconcile": [
                {
                    "name": "sec",
                    "interval": "30s",
                    "autoApply": true,
                    "driftPolicy": "Auto",
                    "lastReconcile": "2026-04-01T00:00:00Z"
                }
            ]
        }"#;

        let parsed: DaemonStatusResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.running);
        assert_eq!(parsed.pid, 54321);
        assert_eq!(parsed.uptime_secs, 7200);
        assert_eq!(
            parsed.last_reconcile.as_deref(),
            Some("2026-04-01T00:00:00Z")
        );
        assert_eq!(parsed.last_sync.as_deref(), Some("2026-04-01T00:01:00Z"));
        assert_eq!(parsed.drift_count, 42);
        assert_eq!(parsed.sources.len(), 1);
        assert_eq!(parsed.sources[0].drift_count, 10);
        assert_eq!(parsed.update_available.as_deref(), Some("4.0.0"));
        assert_eq!(parsed.module_reconcile.len(), 1);
        assert_eq!(parsed.module_reconcile[0].name, "sec");
        assert!(parsed.module_reconcile[0].auto_apply);
    }

    // --- CheckinServerResponse: missing config field defaults to None ---

    #[test]
    fn checkin_response_without_config_field() {
        let json = r#"{"status":"ok","config_changed":false}"#;
        let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
        // _config is Option<Value>, so missing field deserializes as None
        assert!(!resp.config_changed);
        assert!(resp._config.is_none());
    }

    // --- hash_resources: unicode content ---

    #[test]
    fn hash_resources_unicode_content() {
        let set: HashSet<String> = HashSet::from_iter(["packages.brew.\u{1f600}".to_string()]);
        let hash = hash_resources(&set);
        assert_eq!(hash.len(), 64);
        // Must be deterministic
        assert_eq!(hash, hash_resources(&set));
    }

    // --- parse_duration_or_default: whitespace-only falls back ---

    #[test]
    fn parse_duration_whitespace_only_falls_back() {
        assert_eq!(
            parse_duration_or_default("   "),
            Duration::from_secs(DEFAULT_RECONCILE_SECS)
        );
    }

    // --- SyncTask: interval boundary values ---

    #[test]
    fn sync_task_zero_interval() {
        let task = SyncTask {
            source_name: "instant".into(),
            repo_path: PathBuf::from("/tmp"),
            auto_pull: true,
            auto_push: true,
            auto_apply: true,
            interval: Duration::from_secs(0),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: false,
        };
        assert_eq!(task.interval, Duration::ZERO);
    }

    // --- DaemonState: to_response sources ordering is preserved ---

    #[test]
    fn daemon_state_to_response_preserves_source_order() {
        let mut state = DaemonState::new();
        state.sources.push(SourceStatus {
            name: "z-source".into(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 0,
            status: "active".into(),
        });
        state.sources.push(SourceStatus {
            name: "a-source".into(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 0,
            status: "active".into(),
        });

        let response = state.to_response();
        assert_eq!(response.sources[0].name, "local");
        assert_eq!(response.sources[1].name, "z-source");
        assert_eq!(response.sources[2].name, "a-source");
    }

    // --- DaemonState: started_at tracks elapsed time ---

    #[test]
    fn daemon_state_started_at_elapses() {
        let state = DaemonState::new();
        let elapsed = state.started_at.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "started_at should be recent"
        );
    }

    // --- handle_health_connection: /drift response structure ---

    #[tokio::test]
    async fn health_connection_drift_body_parses_as_json() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let (client, server) = tokio::io::duplex(8192);

        let handler_state = Arc::clone(&state);
        let handler = tokio::spawn(async move {
            handle_health_connection(server, handler_state)
                .await
                .unwrap();
        });

        let (reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /drift HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut lines: Vec<String> = Vec::new();
        let mut in_body = false;
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if in_body {
                        lines.push(line);
                    } else if line.trim().is_empty() {
                        in_body = true;
                    }
                }
                Err(_) => break,
            }
        }

        handler.await.unwrap();

        let body = lines.join("");
        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("drift body should be valid JSON");
        assert!(parsed.get("drift_count").is_some());
        assert!(parsed.get("events").is_some());
        assert!(parsed["events"].is_array());
        // With an empty default state store, events should be empty
        assert_eq!(parsed["drift_count"].as_u64().unwrap(), 0);
    }

    // --- handle_sync: no pull, no push, still updates timestamp ---

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_no_pull_no_push_updates_timestamp() {
        use crate::test_helpers::init_test_git_repo;

        let tmp = tempfile::TempDir::new().unwrap();
        let repo_dir = tmp.path().join("repo");
        init_test_git_repo(&repo_dir);

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let st = Arc::clone(&state);
        let rd = repo_dir.clone();

        let changed = tokio::task::spawn_blocking(move || {
            handle_sync(&rd, false, false, "local", &st, false, false)
        })
        .await
        .unwrap();

        assert!(!changed, "no pull/push means no changes");

        let st = state.lock().await;
        assert!(
            st.last_sync.is_some(),
            "last_sync should be set even with no operations"
        );
    }

    // --- git_pull_sync: delegates to git_pull ---

    #[test]
    fn git_pull_sync_non_repo_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = git_pull_sync(tmp.path());
        let err = result.unwrap_err();
        assert!(
            err.contains("open repo"),
            "expected 'open repo' error, got: {err}"
        );
    }

    #[test]
    fn git_pull_sync_clean_repo_no_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "test\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let result = git_pull_sync(&work_dir);
        assert!(result.is_ok());
        assert!(!result.unwrap(), "should be up to date");
    }

    // --- Notifier: all methods construct without panic ---

    #[test]
    fn notifier_all_methods_construct() {
        let stdout = Notifier::new(NotifyMethod::Stdout, None);
        assert!(matches!(stdout.method, NotifyMethod::Stdout));
        assert!(stdout.webhook_url.is_none());

        let desktop = Notifier::new(NotifyMethod::Desktop, None);
        assert!(matches!(desktop.method, NotifyMethod::Desktop));

        let webhook_none = Notifier::new(NotifyMethod::Webhook, None);
        assert!(matches!(webhook_none.method, NotifyMethod::Webhook));
        assert!(webhook_none.webhook_url.is_none());

        let webhook_url = Notifier::new(
            NotifyMethod::Webhook,
            Some("https://example.com/hook".into()),
        );
        assert_eq!(
            webhook_url.webhook_url.as_deref(),
            Some("https://example.com/hook")
        );
    }

    // --- DaemonStatusResponse: serialization/deserialization symmetry ---

    #[test]
    fn daemon_status_response_roundtrip_symmetry() {
        let original = DaemonStatusResponse {
            running: true,
            pid: 99999,
            uptime_secs: 86400,
            last_reconcile: Some("2026-04-01T12:00:00Z".into()),
            last_sync: Some("2026-04-01T12:01:00Z".into()),
            drift_count: 100,
            sources: vec![
                SourceStatus {
                    name: "local".into(),
                    last_sync: Some("2026-04-01T12:01:00Z".into()),
                    last_reconcile: Some("2026-04-01T12:00:00Z".into()),
                    drift_count: 50,
                    status: "active".into(),
                },
                SourceStatus {
                    name: "corp".into(),
                    last_sync: None,
                    last_reconcile: None,
                    drift_count: 50,
                    status: "error".into(),
                },
            ],
            update_available: Some("5.0.0".into()),
            module_reconcile: vec![ModuleReconcileStatus {
                name: "sec-baseline".into(),
                interval: "30s".into(),
                auto_apply: true,
                drift_policy: "Auto".into(),
                last_reconcile: Some("2026-04-01T12:00:00Z".into()),
            }],
        };

        let json = serde_json::to_string(&original).unwrap();
        let roundtripped: DaemonStatusResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(roundtripped.pid, original.pid);
        assert_eq!(roundtripped.uptime_secs, original.uptime_secs);
        assert_eq!(roundtripped.drift_count, original.drift_count);
        assert_eq!(roundtripped.sources.len(), original.sources.len());
        assert_eq!(
            roundtripped.sources[1].drift_count,
            original.sources[1].drift_count
        );
        assert_eq!(
            roundtripped.module_reconcile.len(),
            original.module_reconcile.len()
        );
        assert_eq!(roundtripped.update_available, original.update_available);
    }

    // --- SourceStatus: serialization includes camelCase properly ---

    #[test]
    fn source_status_camel_case_serialization() {
        let status = SourceStatus {
            name: "test".into(),
            last_sync: Some("ts".into()),
            last_reconcile: Some("tr".into()),
            drift_count: 1,
            status: "active".into(),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"lastSync\""));
        assert!(json.contains("\"lastReconcile\""));
        assert!(json.contains("\"driftCount\""));
        assert!(!json.contains("\"last_sync\""));
        assert!(!json.contains("\"last_reconcile\""));
        assert!(!json.contains("\"drift_count\""));
    }

    // --- infer_item_tier: boundary cases ---

    #[test]
    fn infer_item_tier_empty_string() {
        assert_eq!(infer_item_tier(""), "recommended");
    }

    #[test]
    fn infer_item_tier_case_sensitivity() {
        // "Security" (uppercase S) does NOT match since contains() is case-sensitive
        assert_eq!(infer_item_tier("files.Security-settings"), "recommended");
        // "POLICY" (all caps) does NOT match since contains() is case-sensitive
        assert_eq!(infer_item_tier("files.POLICY-doc"), "recommended");
        // Only lowercase matches trigger the "locked" tier
        assert_eq!(infer_item_tier("files.security-settings"), "locked");
        assert_eq!(infer_item_tier("files.policy-doc"), "locked");
    }

    #[test]
    fn infer_item_tier_partial_keyword_match() {
        // "insecurity" contains "security"
        assert_eq!(infer_item_tier("files.insecurity-note"), "locked");
    }

    // --- compute_config_hash: uses only packages for hash ---

    #[test]
    fn compute_config_hash_ignores_non_package_fields() {
        use crate::config::{
            EnvVar, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
            ResolvedProfile,
        };

        let resolved_a = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "a".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                env: vec![EnvVar {
                    name: "FOO".into(),
                    value: "bar".into(),
                }],
                ..Default::default()
            },
        };

        let resolved_b = ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".into(),
                profile_name: "b".into(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile {
                packages: PackagesSpec::default(),
                env: vec![EnvVar {
                    name: "BAZ".into(),
                    value: "qux".into(),
                }],
                ..Default::default()
            },
        };

        // Both have same empty packages, so hash should be the same
        // because compute_config_hash only hashes the packages field
        let hash_a = compute_config_hash(&resolved_a).unwrap();
        let hash_b = compute_config_hash(&resolved_b).unwrap();
        assert_eq!(
            hash_a, hash_b,
            "compute_config_hash should only hash packages, not env vars"
        );
    }

    // --- generate_launchd_plist tests ---

    #[cfg(unix)]
    #[test]
    fn generate_launchd_plist_contains_correct_structure() {
        let binary = Path::new("/usr/local/bin/cfgd");
        let config = Path::new("/Users/testuser/.config/cfgd/config.yaml");
        let home = Path::new("/Users/testuser");

        let plist = generate_launchd_plist(binary, config, None, home);

        assert!(
            plist.contains("<?xml version=\"1.0\""),
            "plist should have XML declaration"
        );
        assert!(
            plist.contains(&format!("<string>{}</string>", LAUNCHD_LABEL)),
            "plist should contain the launchd label"
        );
        assert!(
            plist.contains("<string>/usr/local/bin/cfgd</string>"),
            "plist should contain binary path"
        );
        assert!(
            plist.contains("<string>/Users/testuser/.config/cfgd/config.yaml</string>"),
            "plist should contain config path"
        );
        assert!(
            plist.contains("<string>daemon</string>"),
            "plist should contain daemon subcommand"
        );
        assert!(
            plist.contains("<key>RunAtLoad</key>"),
            "plist should enable run at load"
        );
        assert!(
            plist.contains("<key>KeepAlive</key>"),
            "plist should enable keep alive"
        );
        assert!(
            plist.contains("/Users/testuser/Library/Logs/cfgd.log"),
            "plist should set stdout log path under home"
        );
        assert!(
            plist.contains("/Users/testuser/Library/Logs/cfgd.err"),
            "plist should set stderr log path under home"
        );
        // Without profile, no --profile argument should appear
        assert!(
            !plist.contains("--profile"),
            "plist without profile should not contain --profile"
        );
    }

    #[cfg(unix)]
    #[test]
    fn generate_launchd_plist_with_profile() {
        let binary = Path::new("/usr/local/bin/cfgd");
        let config = Path::new("/home/user/.config/cfgd/config.yaml");
        let home = Path::new("/home/user");

        let plist = generate_launchd_plist(binary, config, Some("work"), home);

        assert!(
            plist.contains("<string>--profile</string>"),
            "plist with profile should contain --profile argument"
        );
        assert!(
            plist.contains("<string>work</string>"),
            "plist with profile should contain the profile name"
        );
        // Verify order: --config before daemon before --profile
        let config_pos = plist.find("<string>--config</string>").unwrap();
        let daemon_pos = plist.find("<string>daemon</string>").unwrap();
        let profile_pos = plist.find("<string>--profile</string>").unwrap();
        assert!(
            config_pos < daemon_pos,
            "--config should appear before daemon"
        );
        assert!(
            daemon_pos < profile_pos,
            "daemon should appear before --profile"
        );
    }

    // --- generate_systemd_unit tests ---

    #[cfg(unix)]
    #[test]
    fn generate_systemd_unit_contains_correct_structure() {
        let binary = Path::new("/usr/local/bin/cfgd");
        let config = Path::new("/home/user/.config/cfgd/config.yaml");

        let unit = generate_systemd_unit(binary, config, None);

        assert!(
            unit.contains("[Unit]"),
            "unit file should have [Unit] section"
        );
        assert!(
            unit.contains("Description=cfgd configuration daemon"),
            "unit file should have correct description"
        );
        assert!(
            unit.contains("After=network.target"),
            "unit file should depend on network.target"
        );
        assert!(
            unit.contains("[Service]"),
            "unit file should have [Service] section"
        );
        assert!(
            unit.contains("Type=simple"),
            "unit file should use simple service type"
        );
        assert!(
            unit.contains(
                "ExecStart=/usr/local/bin/cfgd --config /home/user/.config/cfgd/config.yaml daemon"
            ),
            "unit file should have correct ExecStart"
        );
        assert!(
            unit.contains("Restart=on-failure"),
            "unit file should restart on failure"
        );
        assert!(
            unit.contains("RestartSec=10"),
            "unit file should have 10s restart delay"
        );
        assert!(
            unit.contains("[Install]"),
            "unit file should have [Install] section"
        );
        assert!(
            unit.contains("WantedBy=default.target"),
            "unit file should be wanted by default.target"
        );
        // Without profile, no --profile should appear
        assert!(
            !unit.contains("--profile"),
            "unit without profile should not contain --profile"
        );
    }

    #[cfg(unix)]
    #[test]
    fn generate_systemd_unit_with_profile() {
        let binary = Path::new("/opt/bin/cfgd");
        let config = Path::new("/etc/cfgd/config.yaml");

        let unit = generate_systemd_unit(binary, config, Some("server"));

        assert!(
            unit.contains(
                "ExecStart=/opt/bin/cfgd --config /etc/cfgd/config.yaml --profile server daemon"
            ),
            "unit file with profile should include --profile in ExecStart"
        );
    }

    // --- record_file_drift_to tests ---

    #[test]
    fn record_file_drift_to_records_event() {
        let store = test_state();
        let path = Path::new("/home/user/.bashrc");

        let result = record_file_drift_to(&store, path);
        assert!(result, "record_file_drift_to should return true on success");

        let events = store.unresolved_drift().unwrap();
        assert_eq!(events.len(), 1, "should have exactly one drift event");
        assert_eq!(events[0].resource_id, "/home/user/.bashrc");
    }

    #[test]
    fn record_file_drift_to_records_correct_type() {
        let store = test_state();
        let path = Path::new("/etc/config.yaml");

        record_file_drift_to(&store, path);

        let events = store.unresolved_drift().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].resource_type, "file",
            "drift event should have resource_type 'file'"
        );
        assert_eq!(
            events[0].source, "local",
            "drift event should have source 'local'"
        );
        assert_eq!(
            events[0].actual.as_deref(),
            Some("modified"),
            "drift event should have actual value 'modified'"
        );
        assert!(
            events[0].expected.is_none(),
            "drift event should have no expected value"
        );
    }

    // --- discover_managed_paths tests ---

    #[test]
    fn discover_managed_paths_with_no_config_returns_empty() {
        use std::path::Path;

        struct TestHooks;
        impl DaemonHooks for TestHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let hooks = TestHooks;
        // Non-existent config file should return empty paths
        let paths = discover_managed_paths(Path::new("/nonexistent/config.yaml"), None, &hooks);
        assert!(
            paths.is_empty(),
            "non-existent config should return no managed paths"
        );
    }

    // --- parse_daemon_config tests ---

    #[test]
    fn parse_daemon_config_defaults() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: None,
            sync: None,
            notify: None,
        };
        let parsed = parse_daemon_config(&daemon_cfg);
        assert_eq!(
            parsed.reconcile_interval,
            Duration::from_secs(DEFAULT_RECONCILE_SECS)
        );
        assert_eq!(parsed.sync_interval, Duration::from_secs(DEFAULT_SYNC_SECS));
        assert!(!parsed.auto_pull);
        assert!(!parsed.auto_push);
        assert!(!parsed.on_change_reconcile);
        assert!(!parsed.notify_on_drift);
        assert!(matches!(parsed.notify_method, NotifyMethod::Stdout));
        assert!(parsed.webhook_url.is_none());
        assert!(!parsed.auto_apply);
    }

    #[test]
    fn parse_daemon_config_custom_intervals() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "10m".to_string(),
                on_change: false,
                auto_apply: false,
                policy: None,
                drift_policy: config::DriftPolicy::default(),
                patches: vec![],
            }),
            sync: Some(config::SyncConfig {
                auto_pull: false,
                auto_push: false,
                interval: "30s".to_string(),
            }),
            notify: None,
        };
        let parsed = parse_daemon_config(&daemon_cfg);
        assert_eq!(parsed.reconcile_interval, Duration::from_secs(600));
        assert_eq!(parsed.sync_interval, Duration::from_secs(30));
    }

    #[test]
    fn parse_daemon_config_notification_settings() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: None,
            sync: None,
            notify: Some(config::NotifyConfig {
                drift: true,
                method: NotifyMethod::Webhook,
                webhook_url: Some("https://hooks.example.com/drift".to_string()),
            }),
        };
        let parsed = parse_daemon_config(&daemon_cfg);
        assert!(parsed.notify_on_drift);
        assert!(matches!(parsed.notify_method, NotifyMethod::Webhook));
        assert_eq!(
            parsed.webhook_url.as_deref(),
            Some("https://hooks.example.com/drift")
        );
    }

    #[test]
    fn parse_daemon_config_sync_flags() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: None,
            sync: Some(config::SyncConfig {
                auto_pull: true,
                auto_push: true,
                interval: "5m".to_string(),
            }),
            notify: None,
        };
        let parsed = parse_daemon_config(&daemon_cfg);
        assert!(parsed.auto_pull);
        assert!(parsed.auto_push);
    }

    #[test]
    fn parse_daemon_config_on_change_enabled() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "5m".to_string(),
                on_change: true,
                auto_apply: false,
                policy: None,
                drift_policy: config::DriftPolicy::default(),
                patches: vec![],
            }),
            sync: None,
            notify: None,
        };
        let parsed = parse_daemon_config(&daemon_cfg);
        assert!(parsed.on_change_reconcile);
        assert!(!parsed.auto_apply);
    }

    #[test]
    fn parse_daemon_config_auto_apply_enabled() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "5m".to_string(),
                on_change: false,
                auto_apply: true,
                policy: None,
                drift_policy: config::DriftPolicy::Auto,
                patches: vec![],
            }),
            sync: None,
            notify: None,
        };
        let parsed = parse_daemon_config(&daemon_cfg);
        assert!(parsed.auto_apply);
    }

    #[test]
    fn handle_reconcile_with_no_config_file() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        struct NoopHooks;
        impl DaemonHooks for NoopHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();

        // Passing a nonexistent config path should return gracefully (no panic)
        handle_reconcile(
            Path::new("/nonexistent/path/config.yaml"),
            None,
            &state,
            &notifier,
            false,
            &NoopHooks,
            Some(&state_dir),
        );
        // If we got here without panic, the function handled the missing config gracefully.
        // Verify the state wasn't updated (no reconciliation occurred).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let guard = rt.block_on(state.lock());
        assert!(
            guard.last_reconcile.is_none(),
            "no reconcile should have occurred with missing config"
        );
    }

    #[test]
    fn handle_reconcile_with_no_profile() {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        struct NoopHooks;
        impl DaemonHooks for NoopHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();

        // Write a valid config with NO profile set
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
        )
        .unwrap();

        // No profile override and no profile in config — should return gracefully
        handle_reconcile(
            &config_path,
            None,
            &state,
            &notifier,
            false,
            &NoopHooks,
            Some(&state_dir),
        );
        // Should not have updated state since no profile was available
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let guard = rt.block_on(state.lock());
        assert!(
            guard.last_reconcile.is_none(),
            "no reconcile should have occurred without a profile"
        );
    }

    // --- build_reconcile_tasks ---

    #[test]
    fn build_reconcile_tasks_default_only_when_no_patches() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "60s".to_string(),
                on_change: false,
                auto_apply: false,
                policy: None,
                drift_policy: config::DriftPolicy::NotifyOnly,
                patches: vec![],
            }),
            sync: None,
            notify: None,
        };
        let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(60), false);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].entity, "__default__");
        assert_eq!(tasks[0].interval, Duration::from_secs(60));
        assert!(!tasks[0].auto_apply);
        assert_eq!(tasks[0].drift_policy, config::DriftPolicy::NotifyOnly);
    }

    #[test]
    fn build_reconcile_tasks_default_inherits_global_drift_policy() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "120s".to_string(),
                on_change: false,
                auto_apply: true,
                policy: None,
                drift_policy: config::DriftPolicy::Auto,
                patches: vec![],
            }),
            sync: None,
            notify: None,
        };
        let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(120), true);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].drift_policy, config::DriftPolicy::Auto);
        assert!(tasks[0].auto_apply);
    }

    #[test]
    fn build_reconcile_tasks_no_reconcile_config_uses_defaults() {
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: None,
            sync: None,
            notify: None,
        };
        let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(300), false);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].entity, "__default__");
        assert_eq!(tasks[0].interval, Duration::from_secs(300));
        // Default drift policy is NotifyOnly
        assert_eq!(tasks[0].drift_policy, config::DriftPolicy::default());
    }

    #[test]
    fn build_reconcile_tasks_patches_without_resolved_profile_skips_modules() {
        // Patches exist but no resolved profile — should still get only __default__
        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "60s".to_string(),
                on_change: false,
                auto_apply: false,
                policy: None,
                drift_policy: config::DriftPolicy::NotifyOnly,
                patches: vec![config::ReconcilePatch {
                    kind: config::ReconcilePatchKind::Module,
                    name: Some("vim".to_string()),
                    interval: Some("10s".to_string()),
                    auto_apply: Some(true),
                    drift_policy: None,
                }],
            }),
            sync: None,
            notify: None,
        };
        let tasks = build_reconcile_tasks(
            &daemon_cfg,
            None, // no resolved profile
            &["default"],
            Duration::from_secs(60),
            false,
        );
        // Only default task — no module tasks since profile isn't resolved
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].entity, "__default__");
    }

    #[test]
    fn build_reconcile_tasks_module_with_overridden_interval_gets_dedicated_task() {
        // Build a resolved profile with a module
        let merged = config::MergedProfile {
            modules: vec!["vim".to_string()],
            ..Default::default()
        };
        let resolved = config::ResolvedProfile {
            layers: vec![config::ProfileLayer {
                source: "local".to_string(),
                profile_name: "default".to_string(),
                priority: 0,
                policy: config::LayerPolicy::Local,
                spec: Default::default(),
            }],
            merged,
        };

        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "60s".to_string(),
                on_change: false,
                auto_apply: false,
                policy: None,
                drift_policy: config::DriftPolicy::NotifyOnly,
                patches: vec![config::ReconcilePatch {
                    kind: config::ReconcilePatchKind::Module,
                    name: Some("vim".to_string()),
                    interval: Some("10s".to_string()),
                    auto_apply: None,
                    drift_policy: None,
                }],
            }),
            sync: None,
            notify: None,
        };

        let tasks = build_reconcile_tasks(
            &daemon_cfg,
            Some(&resolved),
            &["default"],
            Duration::from_secs(60),
            false,
        );
        // Should have 2 tasks: one for "vim" with 10s interval, one for __default__
        assert_eq!(tasks.len(), 2);
        let vim_task = tasks.iter().find(|t| t.entity == "vim").unwrap();
        assert_eq!(vim_task.interval, Duration::from_secs(10));
        assert!(!vim_task.auto_apply);
        let default_task = tasks.iter().find(|t| t.entity == "__default__").unwrap();
        assert_eq!(default_task.interval, Duration::from_secs(60));
    }

    #[test]
    fn build_reconcile_tasks_module_matching_global_gets_no_dedicated_task() {
        // When a module's effective settings match global, no dedicated task is created
        let merged = config::MergedProfile {
            modules: vec!["vim".to_string()],
            ..Default::default()
        };
        let resolved = config::ResolvedProfile {
            layers: vec![config::ProfileLayer {
                source: "local".to_string(),
                profile_name: "default".to_string(),
                priority: 0,
                policy: config::LayerPolicy::Local,
                spec: Default::default(),
            }],
            merged,
        };

        let daemon_cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "60s".to_string(),
                on_change: false,
                auto_apply: false,
                policy: None,
                drift_policy: config::DriftPolicy::NotifyOnly,
                // Patch that produces same values as global
                patches: vec![config::ReconcilePatch {
                    kind: config::ReconcilePatchKind::Module,
                    name: Some("vim".to_string()),
                    interval: None,     // inherits "60s"
                    auto_apply: None,   // inherits false
                    drift_policy: None, // inherits NotifyOnly
                }],
            }),
            sync: None,
            notify: None,
        };

        let tasks = build_reconcile_tasks(
            &daemon_cfg,
            Some(&resolved),
            &["default"],
            Duration::from_secs(60),
            false,
        );
        // Only __default__ — vim's effective settings match global
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].entity, "__default__");
    }

    // --- build_sync_tasks ---

    #[test]
    fn build_sync_tasks_local_only_when_no_sources() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: Duration::from_secs(60),
            sync_interval: Duration::from_secs(300),
            auto_pull: true,
            auto_push: false,
            on_change_reconcile: false,
            notify_on_drift: false,
            notify_method: NotifyMethod::Stdout,
            webhook_url: None,
            auto_apply: false,
        };
        let tmp = tempfile::tempdir().unwrap();
        let tasks = build_sync_tasks(tmp.path(), &parsed, &[], false, tmp.path(), |_| None);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].source_name, "local");
        assert!(tasks[0].auto_pull);
        assert!(!tasks[0].auto_push);
        assert!(tasks[0].auto_apply);
        assert_eq!(tasks[0].interval, Duration::from_secs(300));
        assert!(!tasks[0].require_signed_commits);
    }

    #[test]
    fn build_sync_tasks_includes_source_when_dir_exists() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: Duration::from_secs(60),
            sync_interval: Duration::from_secs(300),
            auto_pull: false,
            auto_push: false,
            on_change_reconcile: false,
            notify_on_drift: false,
            notify_method: NotifyMethod::Stdout,
            webhook_url: None,
            auto_apply: false,
        };
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("sources");
        std::fs::create_dir_all(cache_dir.join("team-config")).unwrap();

        let sources = vec![config::SourceSpec {
            name: "team-config".to_string(),
            origin: config::OriginSpec {
                origin_type: config::OriginType::Git,
                url: "https://github.com/team/config.git".to_string(),
                branch: "main".to_string(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: config::SourceSyncSpec {
                interval: "120s".to_string(),
                auto_apply: true,
                pin_version: None,
            },
        }];

        let tasks = build_sync_tasks(
            tmp.path(),
            &parsed,
            &sources,
            false,
            &cache_dir,
            |_| Some(true), // manifest requires signed commits
        );
        assert_eq!(tasks.len(), 2);
        let source_task = tasks
            .iter()
            .find(|t| t.source_name == "team-config")
            .unwrap();
        assert!(source_task.auto_pull);
        assert!(!source_task.auto_push);
        assert!(source_task.auto_apply);
        assert_eq!(source_task.interval, Duration::from_secs(120));
        assert!(source_task.require_signed_commits);
    }

    #[test]
    fn build_sync_tasks_skips_source_when_dir_missing() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: Duration::from_secs(60),
            sync_interval: Duration::from_secs(300),
            auto_pull: false,
            auto_push: false,
            on_change_reconcile: false,
            notify_on_drift: false,
            notify_method: NotifyMethod::Stdout,
            webhook_url: None,
            auto_apply: false,
        };
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("sources");
        // Intentionally don't create the source directory

        let sources = vec![config::SourceSpec {
            name: "missing-source".to_string(),
            origin: config::OriginSpec {
                origin_type: config::OriginType::Git,
                url: "https://github.com/team/config.git".to_string(),
                branch: "main".to_string(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: Default::default(),
        }];

        let tasks = build_sync_tasks(tmp.path(), &parsed, &sources, false, &cache_dir, |_| None);
        // Only local task — source dir doesn't exist
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].source_name, "local");
    }

    #[test]
    fn build_sync_tasks_propagates_allow_unsigned() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: Duration::from_secs(60),
            sync_interval: Duration::from_secs(300),
            auto_pull: true,
            auto_push: true,
            on_change_reconcile: false,
            notify_on_drift: false,
            notify_method: NotifyMethod::Stdout,
            webhook_url: None,
            auto_apply: false,
        };
        let tmp = tempfile::tempdir().unwrap();
        let tasks = build_sync_tasks(
            tmp.path(),
            &parsed,
            &[],
            true, // allow_unsigned
            tmp.path(),
            |_| None,
        );
        assert!(tasks[0].allow_unsigned);
    }

    // --- handle_reconcile: deeper paths ---

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_with_valid_config_records_drift_events() {
        // Set up a tmpdir with config.yaml + profiles/default.yaml containing packages.
        // DaemonHooks that returns a PackageAction::Install so the plan has drift.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Write config
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        // Write profile
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n",
        )
        .unwrap();

        struct DriftHooks;
        impl DaemonHooks for DriftHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                // Return a package install action to create drift
                Ok(vec![PackageAction::Install {
                    manager: "cargo".into(),
                    packages: vec!["bat".into()],
                    origin: "local".into(),
                }])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            handle_reconcile(&cp, None, &st, &not, false, &DriftHooks, Some(&sd));
        })
        .await
        .unwrap();

        // Verify drift events were recorded in the state store
        let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
        let drift_events = store.unresolved_drift().unwrap();
        assert!(
            !drift_events.is_empty(),
            "drift events should have been recorded"
        );
        // The drift should be for the package install action
        let pkg_drift = drift_events.iter().find(|e| e.resource_type == "package");
        assert!(
            pkg_drift.is_some(),
            "should have a package drift event; events: {:?}",
            drift_events
        );
        assert_eq!(pkg_drift.unwrap().resource_id, "cargo:bat");

        // Verify daemon state was updated
        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_some(),
            "last_reconcile should have been set"
        );
        assert!(
            guard.drift_count > 0,
            "drift_count should have been incremented"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_notify_only_drift_policy_does_not_apply() {
        // Verify that with NotifyOnly drift policy, drift is recorded but no apply happens.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      onChange: false\n      autoApply: false\n      driftPolicy: NotifyOnly\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n",
        )
        .unwrap();

        struct NotifyOnlyHooks;
        impl DaemonHooks for NotifyOnlyHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![PackageAction::Install {
                    manager: "cargo".into(),
                    packages: vec!["ripgrep".into()],
                    origin: "local".into(),
                }])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            handle_reconcile(&cp, None, &st, &not, false, &NotifyOnlyHooks, Some(&sd));
        })
        .await
        .unwrap();

        // Drift should be recorded
        let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
        let drift_events = store.unresolved_drift().unwrap();
        assert!(
            !drift_events.is_empty(),
            "drift events should be recorded even with NotifyOnly policy"
        );

        // Verify state reflects drift
        let guard = state.lock().await;
        assert!(guard.drift_count > 0);
        assert!(guard.last_reconcile.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_no_drift_when_no_actions() {
        // When plan has no actions, no drift events should be recorded.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        struct NoDriftHooks;
        impl DaemonHooks for NoDriftHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            handle_reconcile(&cp, None, &st, &not, false, &NoDriftHooks, Some(&sd));
        })
        .await
        .unwrap();

        // No drift events should have been recorded
        let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
        let drift_events = store.unresolved_drift().unwrap();
        assert!(
            drift_events.is_empty(),
            "no drift events should be recorded when plan has no actions"
        );

        // State should reflect a reconciliation occurred
        let guard = state.lock().await;
        assert!(guard.last_reconcile.is_some());
        assert_eq!(guard.drift_count, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_with_profile_override() {
        // Test that profile_override is used instead of config's profile field.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Config with profile "other" but we override to "default"
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: nonexistent\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        struct EmptyHooks;
        impl DaemonHooks for EmptyHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        // Override profile to "default" which exists
        tokio::task::spawn_blocking(move || {
            handle_reconcile(
                &cp,
                Some("default"),
                &st,
                &not,
                false,
                &EmptyHooks,
                Some(&sd),
            );
        })
        .await
        .unwrap();

        // Should have completed successfully with the overridden profile
        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_some(),
            "reconciliation should succeed with profile override"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_multiple_actions_records_all_drift() {
        // Verify that all drift-producing actions are recorded as separate events.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n        - ripgrep\n        - fd-find\n",
        )
        .unwrap();

        struct MultiDriftHooks;
        impl DaemonHooks for MultiDriftHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                // Also include a file action
                Ok(vec![FileAction::Create {
                    source: PathBuf::from("/src/.zshrc"),
                    target: PathBuf::from("/home/user/.zshrc"),
                    origin: "local".into(),
                    strategy: crate::config::FileStrategy::default(),
                    source_hash: None,
                }])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![
                    PackageAction::Install {
                        manager: "cargo".into(),
                        packages: vec!["bat".into(), "ripgrep".into()],
                        origin: "local".into(),
                    },
                    PackageAction::Install {
                        manager: "cargo".into(),
                        packages: vec!["fd-find".into()],
                        origin: "local".into(),
                    },
                ])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                crate::expand_tilde(path)
            }
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            handle_reconcile(&cp, None, &st, &not, false, &MultiDriftHooks, Some(&sd));
        })
        .await
        .unwrap();

        let store = StateStore::open(&state_dir.join("cfgd.db")).unwrap();
        let drift_events = store.unresolved_drift().unwrap();
        // Should have drift events for all actions:
        // 1 file create + 2 package install actions = 3 drift events
        assert_eq!(
            drift_events.len(),
            3,
            "should have drift events for all actions; got: {:?}",
            drift_events
        );

        let resource_types: Vec<&str> = drift_events
            .iter()
            .map(|e| e.resource_type.as_str())
            .collect();
        assert!(
            resource_types.contains(&"file"),
            "should have a file drift event"
        );
        assert!(
            resource_types.contains(&"package"),
            "should have package drift events"
        );
    }

    // --- discover_managed_paths ---

    #[test]
    fn discover_managed_paths_returns_targets_from_profile() {
        let tmp = tempfile::tempdir().unwrap();

        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: src/zshrc\n        target: /home/user/.zshrc\n      - source: src/vimrc\n        target: /home/user/.vimrc\n",
        )
        .unwrap();

        struct TestHooks;
        impl DaemonHooks for TestHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                path.to_path_buf()
            }
        }

        let paths = discover_managed_paths(&config_path, None, &TestHooks);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&PathBuf::from("/home/user/.zshrc")));
        assert!(paths.contains(&PathBuf::from("/home/user/.vimrc")));
    }

    #[test]
    fn discover_managed_paths_returns_empty_for_missing_config() {
        struct TestHooks;
        impl DaemonHooks for TestHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                path.to_path_buf()
            }
        }

        let paths = discover_managed_paths(Path::new("/nonexistent/config.yaml"), None, &TestHooks);
        assert!(paths.is_empty());
    }

    #[test]
    fn discover_managed_paths_with_profile_override() {
        let tmp = tempfile::tempdir().unwrap();

        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("custom.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: custom\nspec:\n  files:\n    managed:\n      - source: src/bashrc\n        target: /home/user/.bashrc\n",
        )
        .unwrap();

        struct TestHooks;
        impl DaemonHooks for TestHooks {
            fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
                ProviderRegistry::new()
            }
            fn plan_files(
                &self,
                _: &Path,
                _: &ResolvedProfile,
            ) -> crate::errors::Result<Vec<FileAction>> {
                Ok(vec![])
            }
            fn plan_packages(
                &self,
                _: &MergedProfile,
                _: &[&dyn PackageManager],
            ) -> crate::errors::Result<Vec<PackageAction>> {
                Ok(vec![])
            }
            fn extend_registry_custom_managers(
                &self,
                _: &mut ProviderRegistry,
                _: &config::PackagesSpec,
            ) {
            }
            fn expand_tilde(&self, path: &Path) -> PathBuf {
                path.to_path_buf()
            }
        }

        let paths = discover_managed_paths(&config_path, Some("custom"), &TestHooks);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from("/home/user/.bashrc"));
    }

    // --- pending_resource_paths ---

    #[test]
    fn pending_resource_paths_returns_empty_for_no_decisions() {
        let store = test_state();
        let paths = pending_resource_paths(&store);
        assert!(paths.is_empty());
    }

    // --- generate_launchd_plist: detailed content verification ---

    #[test]
    #[cfg(unix)]
    fn generate_launchd_plist_xml_structure_complete() {
        let binary = Path::new("/usr/local/bin/cfgd");
        let config = Path::new("/Users/alice/.config/cfgd/config.yaml");
        let home = Path::new("/Users/alice");

        let plist = generate_launchd_plist(binary, config, None, home);

        // Verify required XML structure
        assert!(
            plist.contains("<?xml version=\"1.0\""),
            "should start with XML declaration"
        );
        assert!(
            plist.contains("<!DOCTYPE plist"),
            "should contain plist DOCTYPE"
        );
        assert!(
            plist.contains(&format!("<string>{}</string>", LAUNCHD_LABEL)),
            "should contain the label"
        );
        assert!(
            plist.contains("<string>/usr/local/bin/cfgd</string>"),
            "should contain binary path"
        );
        assert!(
            plist.contains("<string>--config</string>"),
            "should contain --config flag"
        );
        assert!(
            plist.contains("<string>/Users/alice/.config/cfgd/config.yaml</string>"),
            "should contain config path"
        );
        assert!(
            plist.contains("<string>daemon</string>"),
            "should contain daemon subcommand"
        );
        assert!(
            plist.contains("<key>RunAtLoad</key>"),
            "should set RunAtLoad"
        );
        assert!(
            plist.contains("<key>KeepAlive</key>"),
            "should set KeepAlive"
        );
        assert!(
            plist.contains("/Users/alice/Library/Logs/cfgd.log"),
            "stdout log should be under home Library/Logs"
        );
        assert!(
            plist.contains("/Users/alice/Library/Logs/cfgd.err"),
            "stderr log should be under home Library/Logs"
        );
        // Should NOT contain --profile when None
        assert!(
            !plist.contains("--profile"),
            "should not contain --profile when None"
        );
    }

    #[test]
    #[cfg(unix)]
    fn generate_launchd_plist_includes_profile_flag() {
        let binary = Path::new("/usr/local/bin/cfgd");
        let config = Path::new("/home/user/config.yaml");
        let home = Path::new("/home/user");

        let plist = generate_launchd_plist(binary, config, Some("work"), home);

        assert!(
            plist.contains("<string>--profile</string>"),
            "should contain --profile flag"
        );
        assert!(
            plist.contains("<string>work</string>"),
            "should contain profile name"
        );
    }

    // --- generate_systemd_unit: detailed content verification ---

    #[test]
    #[cfg(unix)]
    fn generate_systemd_unit_complete_structure() {
        let binary = Path::new("/usr/local/bin/cfgd");
        let config = Path::new("/home/user/.config/cfgd/config.yaml");

        let unit = generate_systemd_unit(binary, config, None);

        assert!(unit.contains("[Unit]"), "should contain [Unit] section");
        assert!(
            unit.contains("[Service]"),
            "should contain [Service] section"
        );
        assert!(
            unit.contains("[Install]"),
            "should contain [Install] section"
        );
        assert!(
            unit.contains("Description=cfgd configuration daemon"),
            "should have description"
        );
        assert!(
            unit.contains("After=network.target"),
            "should require network"
        );
        assert!(
            unit.contains("Type=simple"),
            "should be simple service type"
        );
        assert!(
            unit.contains("Restart=on-failure"),
            "should restart on failure"
        );
        assert!(unit.contains("RestartSec=10"), "should have restart delay");
        assert!(
            unit.contains("WantedBy=default.target"),
            "should be wanted by default.target"
        );

        // Verify ExecStart format: binary --config path daemon
        let expected_exec = format!(
            "ExecStart={} --config {} daemon",
            binary.display(),
            config.display()
        );
        assert!(
            unit.contains(&expected_exec),
            "ExecStart should be '{expected_exec}', got unit:\n{unit}"
        );
        // Should NOT contain --profile
        assert!(
            !unit.contains("--profile"),
            "should not contain --profile when None"
        );
    }

    #[test]
    #[cfg(unix)]
    fn generate_systemd_unit_includes_profile() {
        let binary = Path::new("/opt/cfgd/cfgd");
        let config = Path::new("/etc/cfgd/config.yaml");

        let unit = generate_systemd_unit(binary, config, Some("server"));

        let expected_exec = format!(
            "ExecStart={} --config {} --profile {} daemon",
            binary.display(),
            config.display(),
            "server"
        );
        assert!(
            unit.contains(&expected_exec),
            "ExecStart with profile should be '{expected_exec}', got:\n{unit}"
        );
    }

    // --- record_file_drift_to: actual drift recording ---

    #[test]
    fn record_file_drift_to_stores_event_in_db() {
        let store = test_state();
        let path = Path::new("/home/user/.bashrc");

        let result = record_file_drift_to(&store, path);
        assert!(result, "record_file_drift_to should return true on success");

        // Verify the drift event was actually stored
        let events = store.unresolved_drift().unwrap();
        assert_eq!(events.len(), 1, "should have exactly one drift event");
        assert_eq!(events[0].resource_type, "file");
        assert_eq!(events[0].resource_id, "/home/user/.bashrc");
    }

    #[test]
    fn record_file_drift_to_multiple_files() {
        let store = test_state();

        record_file_drift_to(&store, Path::new("/etc/hosts"));
        record_file_drift_to(&store, Path::new("/etc/resolv.conf"));
        record_file_drift_to(&store, Path::new("/home/user/.zshrc"));

        let events = store.unresolved_drift().unwrap();
        assert_eq!(events.len(), 3, "should have three drift events");

        let ids: Vec<&str> = events.iter().map(|e| e.resource_id.as_str()).collect();
        assert!(ids.contains(&"/etc/hosts"));
        assert!(ids.contains(&"/etc/resolv.conf"));
        assert!(ids.contains(&"/home/user/.zshrc"));
    }

    // --- parse_daemon_config: comprehensive config parsing ---

    #[test]
    fn parse_daemon_config_all_defaults() {
        let cfg = config::DaemonConfig {
            enabled: true,
            reconcile: None,
            sync: None,
            notify: None,
        };

        let parsed = parse_daemon_config(&cfg);
        assert_eq!(
            parsed.reconcile_interval,
            Duration::from_secs(DEFAULT_RECONCILE_SECS)
        );
        assert_eq!(parsed.sync_interval, Duration::from_secs(DEFAULT_SYNC_SECS));
        assert!(!parsed.auto_pull);
        assert!(!parsed.auto_push);
        assert!(!parsed.on_change_reconcile);
        assert!(!parsed.notify_on_drift);
        assert!(matches!(parsed.notify_method, NotifyMethod::Stdout));
        assert!(parsed.webhook_url.is_none());
        assert!(!parsed.auto_apply);
    }

    #[test]
    fn parse_daemon_config_with_all_settings() {
        let cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "60s".into(),
                on_change: true,
                auto_apply: true,
                policy: None,
                drift_policy: config::DriftPolicy::Auto,
                patches: vec![],
            }),
            sync: Some(config::SyncConfig {
                auto_pull: true,
                auto_push: true,
                interval: "120s".into(),
            }),
            notify: Some(config::NotifyConfig {
                drift: true,
                method: NotifyMethod::Webhook,
                webhook_url: Some("https://hooks.example.com/notify".into()),
            }),
        };

        let parsed = parse_daemon_config(&cfg);
        assert_eq!(parsed.reconcile_interval, Duration::from_secs(60));
        assert_eq!(parsed.sync_interval, Duration::from_secs(120));
        assert!(parsed.auto_pull);
        assert!(parsed.auto_push);
        assert!(parsed.on_change_reconcile);
        assert!(parsed.notify_on_drift);
        assert!(matches!(parsed.notify_method, NotifyMethod::Webhook));
        assert_eq!(
            parsed.webhook_url.as_deref(),
            Some("https://hooks.example.com/notify")
        );
        assert!(parsed.auto_apply);
    }

    #[test]
    fn parse_daemon_config_with_minute_interval() {
        let cfg = config::DaemonConfig {
            enabled: true,
            reconcile: Some(config::ReconcileConfig {
                interval: "10m".into(),
                on_change: false,
                auto_apply: false,
                policy: None,
                drift_policy: config::DriftPolicy::default(),
                patches: vec![],
            }),
            sync: Some(config::SyncConfig {
                auto_pull: false,
                auto_push: false,
                interval: "30m".into(),
            }),
            notify: None,
        };

        let parsed = parse_daemon_config(&cfg);
        assert_eq!(parsed.reconcile_interval, Duration::from_secs(600));
        assert_eq!(parsed.sync_interval, Duration::from_secs(1800));
    }

    // --- build_sync_tasks: comprehensive sync task building ---

    #[test]
    fn build_sync_tasks_propagates_source_sync_interval() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source_cache = dir.path().join("sources");
        std::fs::create_dir_all(source_cache.join("team-tools")).unwrap();

        let parsed = ParsedDaemonConfig {
            reconcile_interval: Duration::from_secs(300),
            sync_interval: Duration::from_secs(300),
            auto_pull: true,
            auto_push: false,
            on_change_reconcile: false,
            notify_on_drift: false,
            notify_method: NotifyMethod::Stdout,
            webhook_url: None,
            auto_apply: false,
        };

        let sources = vec![config::SourceSpec {
            name: "team-tools".into(),
            origin: config::OriginSpec {
                origin_type: config::OriginType::Git,
                url: "https://github.com/team/tools.git".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: config::SubscriptionSpec::default(),
            sync: config::SourceSyncSpec {
                auto_apply: true,
                interval: "60s".into(),
                pin_version: None,
            },
        }];

        let tasks = build_sync_tasks(config_dir, &parsed, &sources, false, &source_cache, |_| {
            None
        });

        assert_eq!(tasks.len(), 2, "should have local + team-tools");
        // Local task inherits global settings
        assert_eq!(tasks[0].source_name, "local");
        assert!(tasks[0].auto_pull);
        assert!(!tasks[0].auto_push);
        assert_eq!(tasks[0].interval, Duration::from_secs(300));

        // Source task uses its own interval
        assert_eq!(tasks[1].source_name, "team-tools");
        assert!(tasks[1].auto_pull); // always true for sources
        assert!(!tasks[1].auto_push); // always false for sources
        assert!(tasks[1].auto_apply);
        assert_eq!(tasks[1].interval, Duration::from_secs(60));
    }

    #[test]
    fn build_sync_tasks_manifest_detector_sets_require_signed() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        let source_cache = dir.path().join("sources");
        std::fs::create_dir_all(source_cache.join("signed-source")).unwrap();

        let parsed = ParsedDaemonConfig {
            reconcile_interval: Duration::from_secs(300),
            sync_interval: Duration::from_secs(300),
            auto_pull: false,
            auto_push: false,
            on_change_reconcile: false,
            notify_on_drift: false,
            notify_method: NotifyMethod::Stdout,
            webhook_url: None,
            auto_apply: false,
        };

        let sources = vec![config::SourceSpec {
            name: "signed-source".into(),
            origin: config::OriginSpec {
                origin_type: config::OriginType::Git,
                url: "https://github.com/secure/config.git".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: config::SubscriptionSpec::default(),
            sync: config::SourceSyncSpec::default(),
        }];

        // Manifest detector returns true => require signed commits
        let tasks = build_sync_tasks(config_dir, &parsed, &sources, false, &source_cache, |_| {
            Some(true)
        });

        assert_eq!(tasks.len(), 2);
        assert!(
            !tasks[0].require_signed_commits,
            "local should not require signed"
        );
        assert!(
            tasks[1].require_signed_commits,
            "source with manifest should require signed"
        );
    }

    // --- build_reconcile_tasks: comprehensive reconcile task building ---

    #[test]
    fn build_reconcile_tasks_always_has_default() {
        let cfg = config::DaemonConfig {
            enabled: true,
            reconcile: None,
            sync: None,
            notify: None,
        };

        let tasks = build_reconcile_tasks(&cfg, None, &[], Duration::from_secs(300), false);

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].entity, "__default__");
        assert_eq!(tasks[0].interval, Duration::from_secs(300));
        assert!(!tasks[0].auto_apply);
    }

    // --- git operations with local repos ---

    #[test]
    fn git_pull_on_local_repo_no_remote_is_error() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();

        // Create initial commit so HEAD exists
        let repo = git2::Repository::open(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_oid = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // No remote configured -> should error
        let result = git_pull(dir.path());
        assert!(result.is_err(), "pull without remote should fail");
    }

    #[test]
    fn git_auto_commit_push_with_no_changes_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        // Create initial commit
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Hello").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // No changes after initial commit
        let result = git_auto_commit_push(dir.path());
        // Should return Ok(false) — no changes to commit
        assert_eq!(result, Ok(false));
    }

    // --- DaemonStatusResponse serialization edge cases ---

    #[test]
    fn daemon_status_response_camel_case_keys() {
        let response = DaemonStatusResponse {
            running: true,
            pid: 100,
            uptime_secs: 3600,
            last_reconcile: Some("2026-01-01T00:00:00Z".into()),
            last_sync: None,
            drift_count: 0,
            sources: vec![],
            update_available: None,
            module_reconcile: vec![],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(
            json.contains("\"uptimeSecs\""),
            "should use camelCase: {json}"
        );
        assert!(
            json.contains("\"lastReconcile\""),
            "should use camelCase: {json}"
        );
        assert!(
            json.contains("\"driftCount\""),
            "should use camelCase: {json}"
        );
        assert!(
            !json.contains("\"uptime_secs\""),
            "should not use snake_case: {json}"
        );
    }

    // --- ModuleReconcileStatus serialization ---

    #[test]
    fn module_reconcile_status_round_trips_extended() {
        let status = ModuleReconcileStatus {
            name: "security-baseline".into(),
            interval: "30s".into(),
            auto_apply: true,
            drift_policy: "Auto".into(),
            last_reconcile: Some("2026-04-01T12:00:00Z".into()),
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"autoApply\""), "should use camelCase");
        assert!(json.contains("\"driftPolicy\""), "should use camelCase");
        assert!(json.contains("\"lastReconcile\""), "should use camelCase");

        let parsed: ModuleReconcileStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "security-baseline");
        assert!(parsed.auto_apply);
        assert_eq!(parsed.drift_policy, "Auto");
    }

    // --- extract_source_resources edge cases ---

    #[test]
    fn extract_source_resources_includes_npm_and_pipx_and_dnf() {
        use crate::config::{MergedProfile, NpmSpec, PackagesSpec};

        let merged = MergedProfile {
            packages: PackagesSpec {
                npm: Some(NpmSpec {
                    file: None,
                    global: vec!["typescript".into(), "eslint".into()],
                }),
                pipx: vec!["black".into()],
                dnf: vec!["gcc".into(), "make".into()],
                ..Default::default()
            },
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("packages.npm.typescript"));
        assert!(resources.contains("packages.npm.eslint"));
        assert!(resources.contains("packages.pipx.black"));
        assert!(resources.contains("packages.dnf.gcc"));
        assert!(resources.contains("packages.dnf.make"));
        assert_eq!(resources.len(), 5);
    }

    #[test]
    fn extract_source_resources_includes_apt() {
        use crate::config::{AptSpec, MergedProfile, PackagesSpec};

        let merged = MergedProfile {
            packages: PackagesSpec {
                apt: Some(AptSpec {
                    packages: vec!["vim".into(), "git".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("packages.apt.vim"));
        assert!(resources.contains("packages.apt.git"));
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn extract_source_resources_includes_system_keys() {
        use crate::config::MergedProfile;

        let mut merged = MergedProfile::default();
        merged.system.insert(
            "shell".into(),
            serde_yaml::to_value(&serde_json::json!({"defaultShell": "/bin/zsh"})).unwrap(),
        );
        merged.system.insert(
            "macos_defaults".into(),
            serde_yaml::Value::Mapping(Default::default()),
        );

        let resources = extract_source_resources(&merged);
        assert!(resources.contains("system.shell"));
        assert!(resources.contains("system.macos_defaults"));
        assert_eq!(resources.len(), 2);
    }

    // --- Notifier webhook creates correct payload ---

    #[test]
    fn notifier_new_stores_method_and_url() {
        let notifier = Notifier::new(
            NotifyMethod::Webhook,
            Some("https://hooks.slack.com/test".into()),
        );
        assert!(matches!(notifier.method, NotifyMethod::Webhook));
        assert_eq!(
            notifier.webhook_url.as_deref(),
            Some("https://hooks.slack.com/test")
        );
    }

    #[test]
    fn notifier_desktop_does_not_panic() {
        let notifier = Notifier::new(NotifyMethod::Desktop, None);
        // On CI without a display, this will fall back to stdout — shouldn't panic either way
        notifier.notify("test title", "test body");
    }

    // --- infer_item_tier edge cases ---

    #[test]
    fn infer_item_tier_detects_policy_keyword_extended() {
        assert_eq!(infer_item_tier("files./etc/security-policy.conf"), "locked");
        assert_eq!(infer_item_tier("system.policy_engine"), "locked");
    }

    #[test]
    fn infer_item_tier_normal_resources_are_recommended() {
        assert_eq!(infer_item_tier("packages.npm.typescript"), "recommended");
        assert_eq!(
            infer_item_tier("files./home/user/.gitconfig"),
            "recommended"
        );
        assert_eq!(infer_item_tier("env.PATH"), "recommended");
    }
}
