// Daemon — file watchers, reconciliation loop, sync, notifications, health endpoint, service management

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, Mutex};

use crate::config::{self, CfgdConfig, NotifyMethod};
use crate::errors::{DaemonError, Result};
use crate::output::Printer;
use crate::state::StateStore;

const DEBOUNCE_MS: u64 = 500;
const DEFAULT_SOCKET_PATH: &str = "/tmp/cfgd.sock";
const DEFAULT_RECONCILE_SECS: u64 = 300; // 5m
const DEFAULT_SYNC_SECS: u64 = 300; // 5m
const LAUNCHD_LABEL: &str = "com.cfgd.daemon";
const LAUNCHD_AGENTS_DIR: &str = "Library/LaunchAgents";
const SYSTEMD_USER_DIR: &str = ".config/systemd/user";

// --- Sync Task (Phase 9 prep: designed as Vec<SyncTask>) ---

struct SyncTask {
    source_name: String,
    repo_path: PathBuf,
    auto_pull: bool,
    auto_push: bool,
    interval: Duration,
}

// --- Per-source status (Phase 9 prep: per-source status list) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    pub name: String,
    pub last_sync: Option<String>,
    pub last_reconcile: Option<String>,
    pub drift_count: u32,
    pub status: String,
}

// --- Shared Daemon State ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatusResponse {
    pub running: bool,
    pub pid: u32,
    pub uptime_secs: u64,
    pub last_reconcile: Option<String>,
    pub last_sync: Option<String>,
    pub drift_count: u32,
    pub sources: Vec<SourceStatus>,
}

struct DaemonState {
    started_at: Instant,
    last_reconcile: Option<String>,
    last_sync: Option<String>,
    drift_count: u32,
    sources: Vec<SourceStatus>,
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
            "timestamp": crate::state::now_iso8601(),
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

// --- Main Daemon Entry Point ---

pub async fn run_daemon(
    config_path: PathBuf,
    profile_override: Option<String>,
    printer: Arc<Printer>,
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

    // Build sync tasks (Phase 9 prep: Vec<SyncTask>)
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let sync_tasks: Vec<SyncTask> = vec![SyncTask {
        source_name: "local".to_string(),
        repo_path: config_dir.clone(),
        auto_pull,
        auto_push,
        interval: sync_interval,
    }];

    // Discover managed file paths for watching
    let managed_paths = discover_managed_paths(&config_path, profile_override.as_deref());

    // Set up file watcher channel
    let (file_tx, mut file_rx) = mpsc::channel::<PathBuf>(256);
    let _watcher = setup_file_watcher(file_tx, &managed_paths, &config_dir)?;

    // Clean up stale socket
    let socket_path = PathBuf::from(DEFAULT_SOCKET_PATH);
    if socket_path.exists() {
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

    // Debounce tracking for file events
    let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
    let debounce = Duration::from_millis(DEBOUNCE_MS);

    // Set up timers
    // Phase 9 will use per-source intervals from sync_tasks[i].interval
    let sync_interval = sync_tasks
        .first()
        .map(|t| t.interval)
        .unwrap_or(sync_interval);
    let mut reconcile_timer = tokio::time::interval(reconcile_interval);
    let mut sync_timer = tokio::time::interval(sync_interval);

    // Skip the first immediate tick
    reconcile_timer.tick().await;
    sync_timer.tick().await;

    loop {
        tokio::select! {
            Some(path) = file_rx.recv() => {
                // Debounce: skip if we saw this path recently
                let now = Instant::now();
                if let Some(last) = last_change.get(&path) {
                    if now.duration_since(*last) < debounce {
                        continue;
                    }
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
                    tokio::task::spawn_blocking(move || {
                        handle_reconcile(&cp, po.as_deref(), &st, &nt, notify_drift);
                    }).await.map_err(|e| DaemonError::WatchError {
                        message: format!("reconcile task failed: {}", e),
                    })?;
                }
            }

            _ = reconcile_timer.tick() => {
                tracing::debug!("reconcile tick");
                let cp = config_path.clone();
                let po = profile_override.clone();
                let st = Arc::clone(&state);
                let nt = Arc::clone(&notifier);
                let notify_drift = notify_on_drift;
                tokio::task::spawn_blocking(move || {
                    handle_reconcile(&cp, po.as_deref(), &st, &nt, notify_drift);
                }).await.map_err(|e| DaemonError::WatchError {
                    message: format!("reconcile task failed: {}", e),
                })?;
            }

            _ = sync_timer.tick() => {
                tracing::debug!("sync tick");
                for task in &sync_tasks {
                    let st = Arc::clone(&state);
                    let repo = task.repo_path.clone();
                    let pull = task.auto_pull;
                    let push = task.auto_push;
                    let source_name = task.source_name.clone();
                    tokio::task::spawn_blocking(move || {
                        handle_sync(&repo, pull, push, &source_name, &st);
                    }).await.map_err(|e| DaemonError::WatchError {
                        message: format!("sync task failed: {}", e),
                    })?;
                }
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
                            let _ = sender.blocking_send(path);
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
            if parent.exists() {
                if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
                    tracing::warn!("cannot watch {}: {}", parent.display(), e);
                }
            }
        }
    }

    // Watch config directory for source changes
    if config_dir.exists() {
        if let Err(e) = watcher.watch(config_dir, RecursiveMode::Recursive) {
            tracing::warn!("cannot watch config dir {}: {}", config_dir.display(), e);
        }
    }

    Ok(watcher)
}

fn discover_managed_paths(config_path: &Path, profile_override: Option<&str>) -> Vec<PathBuf> {
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
    let profile_name = profile_override.unwrap_or(&cfg.spec.profile);

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
        .map(|f| crate::files::expand_tilde(&f.target))
        .collect()
}

// --- Reconciliation Handler ---

fn handle_reconcile(
    config_path: &Path,
    profile_override: Option<&str>,
    state: &Arc<Mutex<DaemonState>>,
    notifier: &Arc<Notifier>,
    notify_on_drift: bool,
) {
    tracing::info!("running reconciliation check");

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
    let profile_name = profile_override.unwrap_or(&cfg.spec.profile);

    let resolved = match config::resolve_profile(profile_name, &profiles_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("reconcile: profile resolution failed: {}", e);
            return;
        }
    };

    // Check for drift by generating a plan
    let registry = build_daemon_registry(&cfg);
    let store = match StateStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("reconcile: state store error: {}", e);
            return;
        }
    };

    let reconciler = crate::reconciler::Reconciler::new(&registry, &store);

    let available_managers = registry.available_package_managers();
    let pkg_actions = match crate::packages::plan_packages(&resolved.merged, &available_managers) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("reconcile: package planning failed: {}", e);
            return;
        }
    };

    let mut fm = match crate::files::CfgdFileManager::new(&config_dir, &resolved) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("reconcile: file manager init failed: {}", e);
            return;
        }
    };

    let file_actions = match fm.plan(&resolved.merged) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("reconcile: file planning failed: {}", e);
            return;
        }
    };

    let plan = match reconciler.plan(&resolved, file_actions, pkg_actions) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("reconcile: plan generation failed: {}", e);
            return;
        }
    };

    let total = plan.total_actions();
    let timestamp = crate::state::now_iso8601();

    // Update daemon state
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async {
        let mut st = state.lock().await;
        st.last_reconcile = Some(timestamp.clone());
        if let Some(source) = st.sources.first_mut() {
            source.last_reconcile = Some(timestamp);
        }
    });

    if total == 0 {
        tracing::info!("reconcile: no drift detected");
    } else {
        tracing::info!("reconcile: {} action(s) needed", total);

        // Record drift events
        for phase in &plan.phases {
            for action in &phase.actions {
                let (rtype, rid) = action_resource_info(action);
                if let Err(e) = store.record_drift(&rtype, &rid, None, Some("drift detected")) {
                    tracing::warn!("failed to record drift: {}", e);
                }
            }
        }

        // Update drift count
        rt.block_on(async {
            let mut st = state.lock().await;
            st.drift_count += total as u32;
            if let Some(source) = st.sources.first_mut() {
                source.drift_count += total as u32;
            }
        });

        if notify_on_drift {
            notifier.notify(
                "cfgd: drift detected",
                &format!("{} resource(s) have drifted from desired state", total),
            );
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
    }
}

fn build_daemon_registry(cfg: &CfgdConfig) -> crate::providers::ProviderRegistry {
    let mut registry = crate::providers::ProviderRegistry::new();
    registry.package_managers = crate::packages::all_package_managers();

    use crate::providers::system::*;
    registry
        .system_configurators
        .push(Box::new(ShellConfigurator));

    if cfg!(target_os = "macos") {
        registry
            .system_configurators
            .push(Box::new(MacosDefaultsConfigurator));
        registry
            .system_configurators
            .push(Box::new(LaunchAgentConfigurator));
    }

    if cfg!(target_os = "linux") {
        registry
            .system_configurators
            .push(Box::new(SystemdUnitConfigurator));
    }

    let (backend_name, age_key_path) = if let Some(ref secrets_cfg) = cfg.spec.secrets {
        let name = secrets_cfg.backend.as_str();
        let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
        (name.to_string(), key)
    } else {
        ("sops".to_string(), None)
    };

    registry.secret_backend = Some(crate::secrets::build_secret_backend(
        &backend_name,
        age_key_path,
    ));
    registry.secret_providers = crate::secrets::build_secret_providers();

    registry
}

// --- Sync Handler ---

fn handle_sync(
    repo_path: &Path,
    auto_pull: bool,
    auto_push: bool,
    source_name: &str,
    state: &Arc<Mutex<DaemonState>>,
) {
    let timestamp = crate::state::now_iso8601();

    if auto_pull {
        match git_pull(repo_path) {
            Ok(true) => tracing::info!("sync: pulled new changes from remote"),
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
}

fn git_pull(repo_path: &Path) -> std::result::Result<bool, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| format!("open repo: {}", e))?;

    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| format!("find remote: {}", e))?;

    // Fetch
    let mut fetch_opts = git2::FetchOptions::new();
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });
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
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });
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

    match store.record_drift("file", &path.display().to_string(), None, Some("modified")) {
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

    let stream =
        StdUnixStream::connect(&socket_path).map_err(|e| DaemonError::HealthSocketError {
            message: format!("connect: {}", e),
        })?;

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

    if let Some(rest) = s.strip_suffix('s') {
        if let Ok(n) = rest.parse::<u64>() {
            return Duration::from_secs(n);
        }
    }
    if let Some(rest) = s.strip_suffix('m') {
        if let Ok(n) = rest.parse::<u64>() {
            return Duration::from_secs(n * 60);
        }
    }
    if let Some(rest) = s.strip_suffix('h') {
        if let Ok(n) = rest.parse::<u64>() {
            return Duration::from_secs(n * 3600);
        }
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
        assert!(json.contains("drift_count"));
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
    fn launchd_plist_path() {
        let home = "/Users/testuser";
        let plist_dir = PathBuf::from(home).join(LAUNCHD_AGENTS_DIR);
        let plist_path = plist_dir.join(format!("{}.plist", LAUNCHD_LABEL));
        assert_eq!(
            plist_path.to_str().unwrap(),
            "/Users/testuser/Library/LaunchAgents/com.cfgd.daemon.plist"
        );
    }
}
