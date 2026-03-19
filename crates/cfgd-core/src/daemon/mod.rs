// Daemon — file watchers, reconciliation loop, sync, notifications, health endpoint, service management

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc};

use crate::config::{
    self, AutoApplyPolicyConfig, CfgdConfig, MergedProfile, NotifyMethod, OriginType, PolicyAction,
    ResolvedProfile,
};
use crate::errors::{DaemonError, Result};
use crate::output::Printer;
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
const DEFAULT_SOCKET_PATH: &str = "/tmp/cfgd.sock";
const DEFAULT_RECONCILE_SECS: u64 = 300; // 5m
const DEFAULT_SYNC_SECS: u64 = 300; // 5m
const LAUNCHD_LABEL: &str = "com.cfgd.daemon";
const LAUNCHD_AGENTS_DIR: &str = "Library/LaunchAgents";
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
#[serde(rename_all = "kebab-case")]
pub struct SourceStatus {
    pub name: String,
    pub last_sync: Option<String>,
    pub last_reconcile: Option<String>,
    pub drift_count: u32,
    pub status: String,
}

// --- Shared Daemon State ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
            Ok(_) => tracing::debug!("desktop notification sent: {}", title),
            Err(e) => {
                tracing::warn!("desktop notification failed: {}, falling back to stdout", e);
                self.notify_stdout(title, message);
            }
        }
    }

    fn notify_stdout(&self, title: &str, message: &str) {
        tracing::info!("[{}] {}", title, message);
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

        // Run webhook POST in a separate thread to avoid blocking the async runtime
        std::thread::spawn(move || {
            match ureq::post(&url)
                .set("Content-Type", "application/json")
                .send_string(&body)
            {
                Ok(_) => tracing::debug!("webhook notification sent to {}", url),
                Err(e) => tracing::warn!("webhook notification failed: {}", e),
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
    Ok(format!("{:x}", Sha256::digest(host.as_bytes())))
}

/// Compute a SHA256 hash of the resolved profile serialized to YAML.
fn compute_config_hash(resolved: &ResolvedProfile) -> std::result::Result<String, String> {
    let yaml = serde_yaml::to_string(&resolved.merged.packages)
        .map_err(|e| format!("failed to serialize profile for hashing: {}", e))?;
    Ok(format!("{:x}", Sha256::digest(yaml.as_bytes())))
}

/// Perform a server check-in. Returns true if the server indicates config has changed.
/// On any error, logs a warning and returns false (best-effort).
fn server_checkin(server_url: &str, resolved: &ResolvedProfile) -> bool {
    let device_id = match generate_device_id() {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("server check-in: {}", e);
            return false;
        }
    };

    let host = match hostname::get() {
        Ok(h) => h.to_string_lossy().to_string(),
        Err(e) => {
            tracing::warn!("server check-in: failed to get hostname: {}", e);
            return false;
        }
    };

    let config_hash = match compute_config_hash(resolved) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("server check-in: {}", e);
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
            tracing::warn!("server check-in: failed to serialize payload: {}", e);
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
                            "server check-in: failed to parse response (status {}): {}",
                            status,
                            e
                        );
                        false
                    }
                },
                Err(e) => {
                    tracing::warn!("server check-in: failed to read response body: {}", e);
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!("server check-in failed: {}", e);
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

    // Parse intervals
    let reconcile_interval = daemon_cfg
        .reconcile
        .as_ref()
        .map(|r| parse_duration_str(&r.interval))
        .unwrap_or(Duration::from_secs(DEFAULT_RECONCILE_SECS));

    let sync_interval = daemon_cfg
        .sync
        .as_ref()
        .map(|s| parse_duration_str(&s.interval))
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

    let notifier = Arc::new(Notifier::new(notify_method, webhook_url));
    let state = Arc::new(Mutex::new(DaemonState::new()));

    // Build sync tasks for local config and each configured source
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let allow_unsigned = cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned);

    let mut sync_tasks: Vec<SyncTask> = vec![SyncTask {
        source_name: "local".to_string(),
        repo_path: config_dir.clone(),
        auto_pull,
        auto_push,
        auto_apply: true,
        interval: sync_interval,
        last_synced: None,
        require_signed_commits: false,
        allow_unsigned,
    }];

    // Add sync tasks for each configured source
    let source_cache_dir = crate::sources::SourceManager::default_cache_dir()
        .unwrap_or_else(|_| config_dir.join(".cfgd-sources"));
    for source_spec in &cfg.spec.sources {
        let source_dir = source_cache_dir.join(&source_spec.name);
        if source_dir.exists() {
            // Read manifest to determine if signed commits are required
            let require_signed = crate::sources::detect_source_manifest(&source_dir)
                .ok()
                .flatten()
                .map(|m| m.spec.policy.constraints.require_signed_commits)
                .unwrap_or(false);

            sync_tasks.push(SyncTask {
                source_name: source_spec.name.clone(),
                repo_path: source_dir,
                auto_pull: true, // Sources are always pull-only
                auto_push: false,
                auto_apply: source_spec.sync.auto_apply,
                interval: parse_duration_str(&source_spec.sync.interval),
                last_synced: None,
                require_signed_commits: require_signed,
                allow_unsigned,
            });
        }
    }

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

    // Check for already-running daemon via socket connectivity
    let socket_path = PathBuf::from(DEFAULT_SOCKET_PATH);
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

    // Start health server
    let health_state = Arc::clone(&state);
    let health_handle = tokio::spawn(async move {
        if let Err(e) = run_health_server(&socket_path, health_state).await {
            tracing::error!("health server error: {}", e);
        }
    });

    printer.success(&format!("Health endpoint: {}", DEFAULT_SOCKET_PATH));
    printer.success(&format!(
        "Reconcile interval: {}s",
        reconcile_interval.as_secs()
    ));
    if auto_pull || auto_push {
        printer.success(&format!("Sync interval: {}s", sync_interval.as_secs()));
        printer.key_value("auto-pull", &auto_pull.to_string());
        printer.key_value("auto-push", &auto_push.to_string());
    }
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
                }
                Err(e) => {
                    tracing::warn!("startup check-in: failed to resolve profile: {}", e);
                }
            }
        })
        .await
        .map_err(|e| DaemonError::WatchError {
            message: format!("startup check-in task failed: {}", e),
        })?;
    }

    // Build per-module reconcile tasks from patches
    let reconcile_patches = daemon_cfg
        .reconcile
        .as_ref()
        .map(|r| &r.patches[..])
        .unwrap_or(&[]);

    let mut reconcile_tasks: Vec<ReconcileTask> = Vec::new();
    if !reconcile_patches.is_empty() {
        // Resolve the profile chain for patch resolution
        let profiles_dir = config_dir.join("profiles");
        let profile_name = profile_override
            .as_deref()
            .or(cfg.spec.profile.as_deref())
            .unwrap_or("default");
        let profile_chain: Vec<String> =
            if let Ok(resolved) = config::resolve_profile(profile_name, &profiles_dir) {
                resolved
                    .layers
                    .iter()
                    .map(|l| l.profile_name.clone())
                    .collect()
            } else {
                vec![profile_name.to_string()]
            };
        let chain_refs: Vec<&str> = profile_chain.iter().map(|s| s.as_str()).collect();

        // Warn on duplicate patches
        let mut seen_patches: HashMap<(String, Option<String>), usize> = HashMap::new();
        for (i, patch) in reconcile_patches.iter().enumerate() {
            let key = (format!("{:?}", patch.kind), patch.name.clone());
            if let Some(prev) = seen_patches.insert(key, i) {
                tracing::warn!(
                    "duplicate reconcile patch for {:?} {:?} at positions {} and {} — last wins",
                    patch.kind,
                    patch.name.as_deref().unwrap_or("(all)"),
                    prev,
                    i
                );
            }
        }

        // Build per-module tasks for modules that have effective overrides
        if let Ok(resolved) = config::resolve_profile(profile_name, &profiles_dir)
            && let Some(reconcile_cfg) = daemon_cfg.reconcile.as_ref()
        {
            for mod_ref in &resolved.merged.modules {
                let mod_name = crate::modules::resolve_profile_module_name(mod_ref);
                let eff = crate::resolve_effective_reconcile(mod_name, &chain_refs, reconcile_cfg);

                // Only create a dedicated task if the effective settings differ from global
                if eff.interval != reconcile_cfg.interval
                    || eff.auto_apply != reconcile_cfg.auto_apply
                    || eff.drift_policy != reconcile_cfg.drift_policy
                {
                    reconcile_tasks.push(ReconcileTask {
                        entity: mod_name.to_string(),
                        interval: parse_duration_str(&eff.interval),
                        auto_apply: eff.auto_apply,
                        drift_policy: eff.drift_policy,
                        last_reconciled: None,
                    });
                }
            }
        }
    }

    // Default task for everything not covered by module-specific tasks
    reconcile_tasks.push(ReconcileTask {
        entity: "__default__".to_string(),
        interval: reconcile_interval,
        auto_apply: daemon_cfg
            .reconcile
            .as_ref()
            .map(|r| r.auto_apply)
            .unwrap_or(false),
        drift_policy: daemon_cfg
            .reconcile
            .as_ref()
            .map(|r| r.drift_policy.clone())
            .unwrap_or_default(),
        last_reconciled: None,
    });

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

    // Skip the first immediate tick
    reconcile_timer.tick().await;
    sync_timer.tick().await;
    version_check_timer.tick().await;

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

                tracing::info!("file changed: {}", path.display());

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
                        handle_reconcile(&cp, po.as_deref(), &st, &nt, notify_drift, &*hk);
                    }).await.map_err(|e| DaemonError::WatchError {
                        message: format!("reconcile task failed: {}", e),
                    })?;
                }
            }

            _ = reconcile_timer.tick() => {
                tracing::debug!("reconcile tick");
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
                            handle_reconcile(&cp, po.as_deref(), &st, &nt, notify_drift, &*hk);
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
                    tracing::debug!("default reconcile task not due this tick");
                }
            }

            _ = sync_timer.tick() => {
                tracing::debug!("sync tick");
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
                                "Changes detected but auto-apply is disabled — run 'cfgd sync' interactively"
                            );
                        }
                    }).await.map_err(|e| DaemonError::WatchError {
                        message: format!("sync task failed: {}", e),
                    })?;
                }
            }

            _ = version_check_timer.tick() => {
                tracing::debug!("version check tick");
                let st = Arc::clone(&state);
                let nt = Arc::clone(&notifier);
                tokio::task::spawn_blocking(move || {
                    handle_version_check(&st, &nt);
                }).await.map_err(|e| DaemonError::WatchError {
                    message: format!("version check task failed: {}", e),
                })?;
            }

            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received shutdown signal");
                printer.newline();
                printer.info("Shutting down daemon...");
                break;
            }
        }
    }

    // Cleanup
    health_handle.abort();
    let socket_path = PathBuf::from(DEFAULT_SOCKET_PATH);
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
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
                            if let Err(e) = sender.blocking_send(path) {
                                tracing::warn!("file watcher event dropped: {}", e);
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
                tracing::warn!("cannot watch {}: {}", path.display(), e);
            }
        } else if let Some(parent) = path.parent() {
            // Watch parent directory so we detect file creation
            if parent.exists()
                && let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive)
            {
                tracing::warn!("cannot watch {}: {}", parent.display(), e);
            }
        }
    }

    // Watch config directory for source changes
    if config_dir.exists()
        && let Err(e) = watcher.watch(config_dir, RecursiveMode::Recursive)
    {
        tracing::warn!("cannot watch config dir {}: {}", config_dir.display(), e);
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
            tracing::warn!("cannot load config for file discovery: {}", e);
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
            tracing::warn!("cannot resolve profile for file discovery: {}", e);
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
) {
    tracing::info!("running reconciliation check");

    // Try to acquire the apply lock (non-blocking). If a CLI apply is in
    // progress, skip this reconciliation tick.
    let state_dir = match crate::state::default_state_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("reconcile: cannot determine state directory: {}", e);
            return;
        }
    };
    let _lock = match crate::acquire_apply_lock(&state_dir) {
        Ok(guard) => guard,
        Err(crate::errors::CfgdError::State(crate::errors::StateError::ApplyLockHeld {
            ref holder,
        })) => {
            tracing::debug!("reconcile: skipping — apply lock held by {}", holder);
            return;
        }
        Err(e) => {
            tracing::warn!("reconcile: cannot acquire apply lock: {}", e);
            return;
        }
    };

    let cfg = match config::load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("reconcile: config load failed: {}", e);
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
            tracing::error!("reconcile: profile resolution failed: {}", e);
            return;
        }
    };

    // Check for drift by generating a plan
    let mut registry = hooks.build_registry(&cfg);
    hooks.extend_registry_custom_managers(&mut registry, &resolved.merged.packages);
    let store = match StateStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("reconcile: state store error: {}", e);
            return;
        }
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
                        "failed to auto-reject decisions for removed source {}: {}",
                        decision.source,
                        e
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
            tracing::error!("reconcile: package planning failed: {}", e);
            return;
        }
    };

    let file_actions = match hooks.plan_files(&config_dir, &resolved) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("reconcile: file planning failed: {}", e);
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
        ) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("reconcile: module resolution failed: {}", e);
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let plan = match reconciler.plan(&resolved, file_actions, pkg_actions, resolved_modules) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("reconcile: plan generation failed: {}", e);
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
        tracing::info!("reconcile: no drift detected");
    } else {
        tracing::info!("reconcile: {} action(s) needed", effective_total);

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
                    tracing::warn!("failed to record drift: {}", e);
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
                    "drift policy is Auto — applying {} action(s)",
                    effective_total
                );
                // Auto-apply is intentionally not implemented here yet.
                // The reconciler apply requires the full CLI context (Printer,
                // file manager, secret backend). When implemented, it will call
                // reconciler.apply() with the plan. For now, drift is recorded
                // above and the user is notified to run `cfgd apply`.
                if notify_on_drift {
                    notifier.notify(
                        "cfgd: drift detected — auto-apply pending",
                        &format!(
                            "{} resource(s) drifted. Run `cfgd apply` to reconcile.",
                            effective_total
                        ),
                    );
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
                ScriptAction::Run { path, .. } => {
                    ("script".to_string(), path.display().to_string())
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
    format!("{:x}", Sha256::digest(combined.as_bytes()))
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
                    tracing::warn!("failed to record pending decision: {}", e);
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
        tracing::warn!("failed to store source config hash: {}", e);
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
                        "sync: signature verification failed after pull for '{}': {}",
                        source_name,
                        e
                    );
                    // Don't treat this as "changes" — the content is untrusted
                    return false;
                }
                tracing::info!("sync: pulled new changes from remote");
                changes = true;
            }
            Ok(false) => tracing::debug!("sync: already up to date"),
            Err(e) => tracing::warn!("sync: pull failed: {}", e),
        }
    }

    if auto_push {
        match git_auto_commit_push(repo_path) {
            Ok(true) => tracing::info!("sync: pushed local changes to remote"),
            Ok(false) => tracing::debug!("sync: nothing to push"),
            Err(e) => tracing::warn!("sync: push failed: {}", e),
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

    match crate::upgrade::check_with_cache(None) {
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
            tracing::warn!("version check failed: {}", e);
        }
    }
}

fn git_pull(repo_path: &Path) -> std::result::Result<bool, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| format!("open repo: {}", e))?;

    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| format!("find remote: {}", e))?;

    // Fetch
    let mut fetch_opts = git2::FetchOptions::new();
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(crate::git_ssh_credentials);
    fetch_opts.remote_callbacks(callbacks);

    let head = repo.head().map_err(|e| format!("get HEAD: {}", e))?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| "cannot determine branch name".to_string())?;

    remote
        .fetch(&[branch_name], Some(&mut fetch_opts), None)
        .map_err(|e| format!("fetch: {}", e))?;

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

    // Push
    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| format!("find remote: {}", e))?;

    let head = repo.head().map_err(|e| format!("get HEAD: {}", e))?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| "cannot determine branch name".to_string())?;

    let mut push_opts = git2::PushOptions::new();
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(crate::git_ssh_credentials);
    push_opts.remote_callbacks(callbacks);

    let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
    remote
        .push(&[&refspec], Some(&mut push_opts))
        .map_err(|e| format!("push: {}", e))?;

    Ok(true)
}

// --- Health Server ---

async fn run_health_server(socket_path: &Path, state: Arc<Mutex<DaemonState>>) -> Result<()> {
    let listener = UnixListener::bind(socket_path).map_err(|e| DaemonError::HealthSocketError {
        message: format!("bind {}: {}", socket_path.display(), e),
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
                tracing::debug!("health connection error: {}", e);
            }
        });
    }
}

async fn handle_health_connection(
    stream: tokio::net::UnixStream,
    state: Arc<Mutex<DaemonState>>,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = stream.into_split();
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

fn record_file_drift(path: &Path) -> bool {
    let store = match StateStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cannot open state store for drift recording: {}", e);
            return false;
        }
    };

    match store.record_drift(
        "file",
        &path.display().to_string(),
        None,
        Some("modified"),
        "local",
    ) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("failed to record drift: {}", e);
            false
        }
    }
}

// --- Service Management ---

pub fn install_service(config_path: &Path, profile: Option<&str>) -> Result<()> {
    let cfgd_binary = std::env::current_exe().map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("cannot determine binary path: {}", e),
    })?;

    if cfg!(target_os = "macos") {
        install_launchd_service(&cfgd_binary, config_path, profile)
    } else {
        install_systemd_service(&cfgd_binary, config_path, profile)
    }
}

pub fn uninstall_service() -> Result<()> {
    if cfg!(target_os = "macos") {
        uninstall_launchd_service()
    } else {
        uninstall_systemd_service()
    }
}

fn install_launchd_service(binary: &Path, config_path: &Path, profile: Option<&str>) -> Result<()> {
    let home = std::env::var("HOME").map_err(|_| DaemonError::ServiceInstallFailed {
        message: "HOME not set".to_string(),
    })?;
    let plist_dir = PathBuf::from(&home).join(LAUNCHD_AGENTS_DIR);
    std::fs::create_dir_all(&plist_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create LaunchAgents dir: {}", e),
    })?;

    let plist_path = plist_dir.join(format!("{}.plist", LAUNCHD_LABEL));
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let mut args = vec![
        format!("<string>{}</string>", binary.display()),
        format!("<string>--config</string>"),
        format!("<string>{}</string>", config_abs.display()),
        format!("<string>daemon</string>"),
    ];

    if let Some(p) = profile {
        args.push("<string>--profile</string>".to_string());
        args.push(format!("<string>{}</string>", p));
    }

    let args_xml = args.join("\n            ");
    let label = LAUNCHD_LABEL;

    let plist = format!(
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
    <string>{home}/Library/Logs/cfgd.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/Library/Logs/cfgd.err</string>
</dict>
</plist>"#
    );

    std::fs::write(&plist_path, plist).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("write plist: {}", e),
    })?;

    tracing::info!("installed launchd service: {}", plist_path.display());
    Ok(())
}

fn uninstall_launchd_service() -> Result<()> {
    let home = std::env::var("HOME").map_err(|_| DaemonError::ServiceInstallFailed {
        message: "HOME not set".to_string(),
    })?;
    let plist_path = PathBuf::from(&home)
        .join(LAUNCHD_AGENTS_DIR)
        .join(format!("{}.plist", LAUNCHD_LABEL));

    if plist_path.exists() {
        std::fs::remove_file(&plist_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove plist: {}", e),
        })?;
        tracing::info!("removed launchd service: {}", plist_path.display());
    }

    Ok(())
}

fn install_systemd_service(binary: &Path, config_path: &Path, profile: Option<&str>) -> Result<()> {
    let home = std::env::var("HOME").map_err(|_| DaemonError::ServiceInstallFailed {
        message: "HOME not set".to_string(),
    })?;
    let unit_dir = PathBuf::from(&home).join(SYSTEMD_USER_DIR);
    std::fs::create_dir_all(&unit_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create systemd user dir: {}", e),
    })?;

    let unit_path = unit_dir.join("cfgd.service");
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let mut exec_start = format!(
        "{} --config {} daemon",
        binary.display(),
        config_abs.display()
    );
    if let Some(p) = profile {
        exec_start = format!(
            "{} --config {} --profile {} daemon",
            binary.display(),
            config_abs.display(),
            p
        );
    }

    let unit = format!(
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
    );

    std::fs::write(&unit_path, unit).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("write unit file: {}", e),
    })?;

    tracing::info!("installed systemd user service: {}", unit_path.display());
    Ok(())
}

fn uninstall_systemd_service() -> Result<()> {
    let home = std::env::var("HOME").map_err(|_| DaemonError::ServiceInstallFailed {
        message: "HOME not set".to_string(),
    })?;
    let unit_path = PathBuf::from(&home)
        .join(SYSTEMD_USER_DIR)
        .join("cfgd.service");

    if unit_path.exists() {
        std::fs::remove_file(&unit_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove unit file: {}", e),
        })?;
        tracing::info!("removed systemd user service: {}", unit_path.display());
    }

    Ok(())
}

// --- Status Query (for cfgd daemon --status) ---

pub fn query_daemon_status() -> Result<Option<DaemonStatusResponse>> {
    let socket_path = PathBuf::from(DEFAULT_SOCKET_PATH);

    if !socket_path.exists() {
        return Ok(None);
    }

    let stream = match StdUnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => {
            // Socket file exists but connection failed — daemon is not running
            // (stale socket from a previous run)
            return Ok(None);
        }
    };

    // Set a timeout
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| DaemonError::HealthSocketError {
            message: format!("set timeout: {}", e),
        })?;

    let mut stream_ref = &stream;
    write!(
        stream_ref,
        "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| DaemonError::HealthSocketError {
        message: format!("write request: {}", e),
    })?;

    let reader = BufReader::new(&stream);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;

    for line in reader.lines() {
        let line = line.map_err(|e| DaemonError::HealthSocketError {
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

pub fn parse_duration_str(s: &str) -> Duration {
    let s = s.trim();

    if let Some(rest) = s.strip_suffix('s')
        && let Ok(n) = rest.parse::<u64>()
    {
        return Duration::from_secs(n);
    }
    if let Some(rest) = s.strip_suffix('m')
        && let Ok(n) = rest.parse::<u64>()
    {
        return Duration::from_secs(n * 60);
    }
    if let Some(rest) = s.strip_suffix('h')
        && let Ok(n) = rest.parse::<u64>()
    {
        return Duration::from_secs(n * 3600);
    }

    // Try plain number as seconds
    if let Ok(n) = s.parse::<u64>() {
        return Duration::from_secs(n);
    }

    Duration::from_secs(DEFAULT_RECONCILE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration_str("30s"), Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration_str("5m"), Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration_str("1h"), Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_plain_number() {
        assert_eq!(parse_duration_str("120"), Duration::from_secs(120));
    }

    #[test]
    fn parse_duration_invalid_falls_back() {
        assert_eq!(
            parse_duration_str("invalid"),
            Duration::from_secs(DEFAULT_RECONCILE_SECS)
        );
    }

    #[test]
    fn parse_duration_with_whitespace() {
        assert_eq!(parse_duration_str(" 10m "), Duration::from_secs(600));
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
        notifier.notify("test", "message");
    }

    #[test]
    fn source_status_serializes() {
        let status = SourceStatus {
            name: "local".to_string(),
            last_sync: Some("2026-01-01T00:00:00Z".to_string()),
            last_reconcile: None,
            drift_count: 3,
            status: "active".to_string(),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("local"));
        assert!(json.contains("drift-count"));
    }

    #[test]
    fn daemon_status_response_serializes() {
        let response = DaemonStatusResponse {
            running: true,
            pid: 12345,
            uptime_secs: 3600,
            last_reconcile: Some("2026-01-01T00:00:00Z".to_string()),
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
            module_reconcile: vec![],
        };
        let json = serde_json::to_string(&response).unwrap();
        let parsed: DaemonStatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, 12345);
        assert_eq!(parsed.uptime_secs, 3600);
    }

    #[test]
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
            },
        };
        assert_eq!(
            find_server_url(&config),
            Some("https://cfgd.example.com".to_string())
        );
    }

    #[test]
    fn checkin_payload_serializes() {
        let payload = CheckinPayload {
            device_id: "abc123".into(),
            hostname: "test-host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            config_hash: "deadbeef".into(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("device_id"));
        assert!(json.contains("abc123"));
        assert!(json.contains("config_hash"));
    }

    #[test]
    fn checkin_response_deserializes() {
        let json = r#"{"status":"ok","config_changed":true,"config":null}"#;
        let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
        assert!(resp.config_changed);
        assert_eq!(resp._status, "ok");
    }

    #[test]
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
        let store = StateStore::open_in_memory().unwrap();
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
        let store = StateStore::open_in_memory().unwrap();
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
}
