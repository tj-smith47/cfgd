use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::files::CfgdFileManager;
use crate::packages;
use crate::secrets;
use cfgd_core::composition::{self, CompositionInput, SubscriptionConfig};
use cfgd_core::config::{self, CfgdConfig, ResolvedProfile};
use cfgd_core::modules;
use cfgd_core::output::Printer;
use cfgd_core::platform::Platform;
use cfgd_core::providers::{FileAction, PackageAction, ProviderRegistry, SecretAction};
use cfgd_core::reconciler::{self, PhaseName, Reconciler};
use cfgd_core::sources::SourceManager;
use cfgd_core::state::StateStore;

const BOOTSTRAP_STATE_FILE: &str = ".cfgd-bootstrap-state";

fn default_config_file() -> PathBuf {
    cfgd_core::default_config_dir().join("cfgd.yaml")
}

#[derive(Parser)]
#[command(
    name = "cfgd",
    version,
    about = "Declarative, GitOps-style machine configuration"
)]
pub struct Cli {
    /// Path to config file
    #[arg(long, global = true, default_value_os_t = default_config_file(), env = "CFGD_CONFIG")]
    pub config: PathBuf,

    /// Profile to use (overrides config file)
    #[arg(long, global = true, env = "CFGD_PROFILE")]
    pub profile: Option<String>,

    /// Verbose output
    #[arg(long, short, global = true, env = "CFGD_VERBOSE")]
    pub verbose: bool,

    /// Suppress all non-error output
    #[arg(long, short, global = true, env = "CFGD_QUIET")]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true, env = "NO_COLOR")]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new cfgd configuration
    Init {
        /// Clone from a remote repository
        #[arg(long)]
        from: Option<String>,

        /// Reserved for multi-source support
        #[arg(long, hide = true)]
        source: Option<String>,

        /// Enroll with a cfgd-server instance
        #[arg(long, env = "CFGD_SERVER_URL")]
        server: Option<String>,

        /// Bootstrap token for server enrollment
        #[arg(long, env = "CFGD_BOOTSTRAP_TOKEN")]
        token: Option<String>,

        /// Bootstrap a single module from a repo (skips profile selection)
        #[arg(long)]
        module: Option<String>,
    },

    /// Show the execution plan
    Plan {
        /// Skip specific items by dot-notation path (e.g., packages.brew.ripgrep, system.sysctl)
        #[arg(long)]
        skip: Vec<String>,

        /// Apply only items matching dot-notation paths (e.g., packages, files)
        #[arg(long)]
        only: Vec<String>,

        /// Plan only the specified module and its dependencies
        #[arg(long)]
        module: Option<String>,
    },

    /// Apply the configuration
    Apply {
        /// Apply only a specific phase
        #[arg(long)]
        phase: Option<String>,

        /// Skip confirmation prompt
        #[arg(long, short, env = "CFGD_YES")]
        yes: bool,

        /// Skip specific items by dot-notation path (e.g., packages.brew.ripgrep, system.sysctl)
        #[arg(long)]
        skip: Vec<String>,

        /// Apply only items matching dot-notation paths (e.g., packages, files)
        #[arg(long)]
        only: Vec<String>,

        /// Apply only the specified module and its dependencies
        #[arg(long)]
        module: Option<String>,
    },

    /// Show configuration status and drift
    Status,

    /// Show detailed diffs
    Diff,

    /// Show apply history
    Log {
        /// Number of entries to show
        #[arg(long, short, default_value = "20")]
        count: u32,
    },

    /// Add a resource to the configuration
    Add {
        /// Path or resource to add
        target: String,

        /// Add as a package
        #[arg(long)]
        package: Option<String>,
    },

    /// Remove a resource from the configuration
    Remove {
        /// Path or resource to remove
        target: String,

        /// Remove a package
        #[arg(long)]
        package: Option<String>,
    },

    /// Sync with remote
    Sync,

    /// Pull remote changes
    Pull,

    /// Manage the daemon
    Daemon {
        /// Install as a system service
        #[arg(long)]
        install: bool,

        /// Uninstall the system service
        #[arg(long)]
        uninstall: bool,

        /// Show daemon status
        #[arg(long)]
        status: bool,
    },

    /// Manage secrets
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },

    /// Manage profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },

    /// Verify all managed resources match desired state
    Verify,

    /// Check system health and dependencies
    Doctor,

    /// Manage modules
    Module {
        #[command(subcommand)]
        command: ModuleCommand,
    },

    /// Manage config sources
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },

    /// Check for and install updates
    Upgrade {
        /// Only check if an update is available (exit 0 = current, exit 1 = update available)
        #[arg(long)]
        check: bool,
    },

    /// Accept or reject pending source decisions
    Decide {
        /// Action: accept or reject
        action: String,

        /// Resource path to decide on (e.g. packages.brew.k9s). Omit for batch operations.
        resource: Option<String>,

        /// Apply decision to all pending items from this source
        #[arg(long)]
        source: Option<String>,

        /// Apply decision to all pending items
        #[arg(long)]
        all: bool,
    },

    /// Check in with cfgd-server and report status
    Checkin {
        /// cfgd-server URL
        #[arg(long, env = "CFGD_SERVER_URL")]
        server_url: String,

        /// API key for authentication
        #[arg(long, env = "CFGD_API_KEY")]
        api_key: Option<String>,

        /// Device identifier (defaults to hostname)
        #[arg(long, env = "CFGD_DEVICE_ID")]
        device_id: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SourceCommand {
    /// Subscribe to a config source
    Add {
        /// Git URL of the source
        url: String,

        /// Name for this source (default: inferred from URL)
        #[arg(long)]
        name: Option<String>,

        /// Profile to subscribe to
        #[arg(long)]
        profile: Option<String>,

        /// Accept recommended items
        #[arg(long)]
        accept_recommended: bool,

        /// Priority for conflict resolution (default: 500, local config: 1000)
        #[arg(long)]
        priority: Option<u32>,
    },

    /// List subscribed sources
    List,

    /// Show details of a source
    Show {
        /// Source name
        name: String,
    },

    /// Remove a source subscription
    Remove {
        /// Source name
        name: String,

        /// Keep all resources from this source as local
        #[arg(long)]
        keep_all: bool,

        /// Remove all resources from this source
        #[arg(long)]
        remove_all: bool,
    },

    /// Update sources (fetch latest)
    Update {
        /// Specific source to update (default: all)
        name: Option<String>,
    },

    /// Override a source's recommendation
    Override {
        /// Source name
        source: String,

        /// Action: set or reject
        action: String,

        /// Resource path (e.g., variables.EDITOR, packages.brew.formulae)
        path: String,

        /// Value (for set action)
        value: Option<String>,
    },

    /// Set or view the priority of a source
    Priority {
        /// Source name
        name: String,

        /// New priority value (omit to view current)
        value: Option<u32>,
    },

    /// Replace one source with another
    Replace {
        /// Source to replace
        old_name: String,

        /// Git URL of the new source
        new_url: String,
    },
}

#[derive(Subcommand)]
pub enum SecretCommand {
    /// Encrypt a file
    Encrypt {
        /// File to encrypt
        file: PathBuf,
    },
    /// Decrypt a file
    Decrypt {
        /// File to decrypt
        file: PathBuf,
    },
    /// Edit an encrypted file
    Edit {
        /// File to edit
        file: PathBuf,
    },
    /// Initialize age key and .sops.yaml
    Init,
}

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// List available profiles
    List,
    /// Switch to a different profile
    Switch {
        /// Profile name
        name: String,
    },
    /// Show the resolved profile
    Show,
}

#[derive(Subcommand)]
pub enum ModuleCommand {
    /// List available modules and their status
    List,
    /// Show module details: packages, files, deps, resolved managers
    Show {
        /// Module name
        name: String,
    },
    /// Add a module to the active profile
    Add {
        /// Module name
        name: String,
    },
    /// Remove a module from the active profile
    Remove {
        /// Module name
        name: String,
    },
}

/// Execute the given CLI command. Returns Ok(()) on success.
pub fn execute(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    match &cli.command {
        Command::Plan { skip, only, module } => {
            cmd_plan(cli, printer, skip, only, module.as_deref())
        }
        Command::Apply {
            phase,
            yes,
            skip,
            only,
            module,
        } => cmd_apply(
            cli,
            printer,
            phase.as_deref(),
            *yes,
            skip,
            only,
            module.as_deref(),
        ),
        Command::Status => cmd_status(cli, printer),
        Command::Diff => cmd_diff(cli, printer),
        Command::Log { count } => cmd_log(printer, *count),
        Command::Verify => cmd_verify(cli, printer),
        Command::Profile { command } => match command {
            ProfileCommand::Show => cmd_profile_show(cli, printer),
            ProfileCommand::List => cmd_profile_list(cli, printer),
            ProfileCommand::Switch { name } => cmd_profile_switch(name, printer),
        },
        Command::Doctor => cmd_doctor(cli, printer),
        Command::Add { target, package } => {
            if let Some(manager) = package {
                cmd_add_package(cli, printer, manager, target)
            } else {
                cmd_add_file(cli, printer, target)
            }
        }
        Command::Remove { target, package } => {
            if let Some(manager) = package {
                cmd_remove_package(cli, printer, manager, target)
            } else {
                cmd_remove_file(cli, printer, target)
            }
        }
        Command::Init {
            from,
            source: _,
            server,
            token,
            module,
        } => {
            if server.is_some() || token.is_some() {
                cmd_init_server(printer, server.as_deref(), token.as_deref())
            } else if let (Some(from_url), Some(mod_name)) = (from.as_deref(), module.as_deref()) {
                cmd_init_module(printer, from_url, mod_name)
            } else {
                cmd_init(printer, from.as_deref())
            }
        }
        Command::Module { command } => match command {
            ModuleCommand::List => cmd_module_list(cli, printer),
            ModuleCommand::Show { name } => cmd_module_show(cli, printer, name),
            ModuleCommand::Add { name } => cmd_module_add(cli, printer, name),
            ModuleCommand::Remove { name } => cmd_module_remove(cli, printer, name),
        },
        Command::Sync => cmd_sync(cli, printer),
        Command::Pull => cmd_pull(cli, printer),
        Command::Daemon {
            install,
            uninstall,
            status,
        } => cmd_daemon(cli, printer, *install, *uninstall, *status),
        Command::Secret { command } => match command {
            SecretCommand::Encrypt { file } => cmd_secret_encrypt(cli, printer, file),
            SecretCommand::Decrypt { file } => cmd_secret_decrypt(cli, printer, file),
            SecretCommand::Edit { file } => cmd_secret_edit(cli, printer, file),
            SecretCommand::Init => cmd_secret_init(cli, printer),
        },
        Command::Source { command } => match command {
            SourceCommand::Add {
                url,
                name,
                profile,
                accept_recommended,
                priority,
            } => cmd_source_add(
                cli,
                printer,
                url,
                name.as_deref(),
                profile.as_deref(),
                *accept_recommended,
                *priority,
            ),
            SourceCommand::Priority { name, value } => {
                cmd_source_priority(cli, printer, name, *value)
            }
            SourceCommand::List => cmd_source_list(cli, printer),
            SourceCommand::Show { name } => cmd_source_show(cli, printer, name),
            SourceCommand::Remove {
                name,
                keep_all,
                remove_all,
            } => cmd_source_remove(cli, printer, name, *keep_all, *remove_all),
            SourceCommand::Update { name } => cmd_source_update(cli, printer, name.as_deref()),
            SourceCommand::Override {
                source,
                action,
                path,
                value,
            } => cmd_source_override(cli, printer, source, action, path, value.as_deref()),
            SourceCommand::Replace { old_name, new_url } => {
                cmd_source_replace(cli, printer, old_name, new_url)
            }
        },
        Command::Upgrade { check } => cmd_upgrade(printer, *check),
        Command::Decide {
            action,
            resource,
            source,
            all,
        } => cmd_decide(
            printer,
            action,
            resource.as_deref(),
            source.as_deref(),
            *all,
        ),
        Command::Checkin {
            server_url,
            api_key,
            device_id,
        } => cmd_checkin(
            cli,
            printer,
            server_url,
            api_key.as_deref(),
            device_id.as_deref(),
        ),
    }
}

// --- Bootstrap State (for resumable init) ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BootstrapState {
    repo_url: Option<String>,
    config_dir: String,
    profile: Option<String>,
    phase: BootstrapPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BootstrapPhase {
    Clone,
    ProfileSelect,
    SecretsSetup,
    Plan,
    Apply,
    Verify,
    DaemonInstall,
    Complete,
}

impl BootstrapPhase {
    fn display_name(&self) -> &'static str {
        match self {
            Self::Clone => "Clone repository",
            Self::ProfileSelect => "Select profile",
            Self::SecretsSetup => "Secrets setup",
            Self::Plan => "Generate plan",
            Self::Apply => "Apply configuration",
            Self::Verify => "Verify resources",
            Self::DaemonInstall => "Daemon setup",
            Self::Complete => "Complete",
        }
    }
}

fn save_bootstrap_state(config_dir: &Path, state: &BootstrapState) -> anyhow::Result<()> {
    let state_path = config_dir.join(BOOTSTRAP_STATE_FILE);
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(&state_path, json)?;
    Ok(())
}

fn load_bootstrap_state(config_dir: &Path) -> Option<BootstrapState> {
    let state_path = config_dir.join(BOOTSTRAP_STATE_FILE);
    if !state_path.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(&state_path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn clear_bootstrap_state(config_dir: &Path) {
    let state_path = config_dir.join(BOOTSTRAP_STATE_FILE);
    let _ = std::fs::remove_file(&state_path);
}

// --- Init Command ---

fn cmd_init(printer: &Printer, from: Option<&str>) -> anyhow::Result<()> {
    printer.header("Initialize cfgd");
    printer.newline();

    // Check prerequisites
    if !check_prerequisites(printer) {
        return Ok(());
    }

    let config_dir = if let Some(url) = from {
        // Clone from remote — check for cfgd-source.yaml
        let cloned_dir = init_from_remote(printer, url)?;
        let cloned_dir = match cloned_dir {
            Some(dir) => dir,
            None => return Ok(()),
        };

        // Source detection: if the cloned repo has cfgd-source.yaml, enter source-aware flow
        match cfgd_core::sources::detect_source_manifest(&cloned_dir) {
            Ok(Some(manifest)) => {
                return init_from_source(printer, url, &cloned_dir, manifest);
            }
            Ok(None) => {
                // Plain config repo — continue with normal flow
            }
            Err(e) => {
                printer.warning(&format!(
                    "Found cfgd-source.yaml but could not parse it: {}",
                    e
                ));
                printer.info("Continuing as a plain config repo");
            }
        }

        Some(cloned_dir)
    } else {
        // Interactive local init wizard
        init_local(printer)?
    };

    let config_dir = match config_dir {
        Some(dir) => dir,
        None => return Ok(()),
    };

    // Check for resumable bootstrap
    let mut state = load_bootstrap_state(&config_dir).unwrap_or(BootstrapState {
        repo_url: from.map(|s| s.to_string()),
        config_dir: config_dir.display().to_string(),
        profile: None,
        phase: BootstrapPhase::ProfileSelect,
    });

    if state.phase != BootstrapPhase::ProfileSelect {
        printer.info(&format!(
            "Resuming bootstrap from: {}",
            state.phase.display_name()
        ));
    }

    // Phase: Profile selection
    if state.phase == BootstrapPhase::ProfileSelect {
        let profile = bootstrap_profile_select(&config_dir, printer)?;
        match profile {
            Some(p) => {
                state.profile = Some(p);
                state.phase = BootstrapPhase::SecretsSetup;
                save_bootstrap_state(&config_dir, &state)?;
            }
            None => return Ok(()),
        }
    }

    let profile_name = match state.profile {
        Some(ref p) => p.clone(),
        None => {
            printer.error("No profile selected");
            return Ok(());
        }
    };

    // Ensure cfgd.yaml exists with the selected profile
    let config_path = config_dir.join("cfgd.yaml");
    ensure_config_file(&config_dir, &config_path, &profile_name, from)?;

    // Phase: Secrets setup
    if state.phase == BootstrapPhase::SecretsSetup {
        bootstrap_secrets_setup(&config_dir, printer)?;
        state.phase = BootstrapPhase::Plan;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Pre-bootstrap diagnostics
    if state.phase == BootstrapPhase::Plan {
        printer.newline();
        run_pre_bootstrap_diagnostics(printer)?;
    }

    // Phase: Plan
    if state.phase == BootstrapPhase::Plan {
        printer.newline();
        printer.header("Bootstrap Plan");
        printer.newline();

        let cfg = config::load_config(&config_path)?;
        let profiles_dir = config_dir.join("profiles");
        let resolved = config::resolve_profile(&profile_name, &profiles_dir)?;
        let registry = build_registry_with_config(Some(&cfg));
        let store = open_state_store()?;
        let reconciler = Reconciler::new(&registry, &store);

        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;

        let fm = CfgdFileManager::new(&config_dir, &resolved)?;
        let file_actions = fm.plan(&resolved.merged)?;

        let plan = reconciler.plan(&resolved, file_actions, pkg_actions, Vec::new())?;

        for phase in &plan.phases {
            let items = reconciler::format_plan_items(phase);
            printer.plan_phase(phase.name.display_name(), &items);
        }

        let total = plan.total_actions();
        printer.newline();
        if total == 0 {
            printer.success("Nothing to do — system is already configured");
            state.phase = BootstrapPhase::Verify;
            save_bootstrap_state(&config_dir, &state)?;
        } else {
            printer.info(&format!("{} action(s) planned", total));
            printer.newline();

            let confirmed = printer
                .prompt_confirm("Apply these changes?")
                .unwrap_or(false);
            if !confirmed {
                printer.info("Aborted — run 'cfgd init' again to resume");
                return Ok(());
            }

            state.phase = BootstrapPhase::Apply;
            save_bootstrap_state(&config_dir, &state)?;
        }
    }

    // Phase: Apply
    if state.phase == BootstrapPhase::Apply {
        printer.newline();
        printer.header("Applying Configuration");
        printer.newline();

        let cfg = config::load_config(&config_path)?;
        let profiles_dir = config_dir.join("profiles");
        let resolved = config::resolve_profile(&profile_name, &profiles_dir)?;
        let mut registry = build_registry_with_config(Some(&cfg));
        let store = open_state_store()?;

        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;

        let mut fm = CfgdFileManager::new(&config_dir, &resolved)?;
        // Set up secret providers for template rendering during apply
        let (backend_name, age_key_path) = if let Some(ref secrets_cfg) = cfg.spec.secrets {
            let name = secrets_cfg.backend.as_str();
            let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
            (name.to_string(), key)
        } else {
            ("sops".to_string(), None)
        };
        fm.set_secret_providers(
            Some(secrets::build_secret_backend(&backend_name, age_key_path)),
            secrets::build_secret_providers(),
        );
        let file_actions = fm.plan(&resolved.merged)?;

        // Register the file manager so the reconciler delegates through the trait
        registry.file_manager = Some(Box::new(fm));

        let reconciler = Reconciler::new(&registry, &store);
        let plan = reconciler.plan(&resolved, file_actions, pkg_actions, Vec::new())?;

        let result = reconciler.apply(&plan, &resolved, &config_dir, printer, None, &[])?;

        printer.newline();
        let status = print_apply_result(&result, printer);
        if status == cfgd_core::state::ApplyStatus::Partial {
            printer.info("Failed actions can be retried with 'cfgd apply'");
        } else if status == cfgd_core::state::ApplyStatus::Failed {
            printer.info("Review errors above and run 'cfgd init' to retry");
            return Ok(());
        }

        state.phase = BootstrapPhase::Verify;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Phase: Verify
    if state.phase == BootstrapPhase::Verify {
        printer.newline();
        printer.header("Verification");
        printer.newline();

        let profiles_dir = config_dir.join("profiles");
        let resolved = config::resolve_profile(&profile_name, &profiles_dir)?;
        let registry = build_registry_with_profile(&resolved.merged.packages);
        let store = open_state_store()?;

        let results = reconciler::verify(&resolved, &registry, &store, printer, &[])?;

        if !results.is_empty() {
            let (pass_count, fail_count) = print_verify_results(&results, printer);
            printer.newline();
            if fail_count == 0 {
                printer.success(&format!("All {} resource(s) verified", pass_count));
            } else {
                printer.warning(&format!(
                    "{} passed, {} failed — run 'cfgd apply' to fix",
                    pass_count, fail_count
                ));
            }
        }

        state.phase = BootstrapPhase::DaemonInstall;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Phase: Daemon install (optional)
    if state.phase == BootstrapPhase::DaemonInstall {
        printer.newline();
        let install_daemon = printer
            .prompt_confirm("Install cfgd daemon for automatic drift detection?")
            .unwrap_or(false);

        if install_daemon {
            match cfgd_core::daemon::install_service(&config_path, Some(&profile_name)) {
                Ok(()) => print_daemon_install_success(printer),
                Err(e) => {
                    printer.warning(&format!("Could not install daemon: {}", e));
                    printer.info("You can install it later with: cfgd daemon --install");
                }
            }
        } else {
            printer.info("Skipped — install later with: cfgd daemon --install");
        }

        state.phase = BootstrapPhase::Complete;
        save_bootstrap_state(&config_dir, &state)?;
    }

    // Done
    clear_bootstrap_state(&config_dir);

    printer.newline();
    printer.header("Bootstrap Complete");
    printer.newline();
    printer.success(&format!("Profile: {}", profile_name));
    printer.success(&format!("Config: {}", config_dir.display()));
    printer.newline();
    printer.info("Useful commands:");
    printer.info("  cfgd status         — view current state");
    printer.info("  cfgd plan           — preview changes");
    printer.info("  cfgd apply          — apply changes");
    printer.info("  cfgd daemon         — start drift detection");

    Ok(())
}

fn check_prerequisites(printer: &Printer) -> bool {
    let mut ok = true;

    if !which("git") {
        printer.error("git is not installed — cfgd requires git");

        if cfg!(target_os = "macos") {
            printer.info("Install with: xcode-select --install");
        } else {
            printer.info("Install with: sudo apt install git (or your package manager)");
        }
        ok = false;
    }

    ok
}

fn init_from_remote(printer: &Printer, url: &str) -> anyhow::Result<Option<PathBuf>> {
    // Determine target directory from URL
    let repo_name = url
        .rsplit('/')
        .next()
        .unwrap_or("cfgd-config")
        .trim_end_matches(".git");

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let target_dir = PathBuf::from(&home).join(format!(".{}", repo_name));

    if target_dir.exists() {
        // Check if it's already a git repo — resumable bootstrap
        if target_dir.join(".git").exists() {
            printer.info(&format!(
                "Repository already exists at {}",
                target_dir.display()
            ));
            printer.info("Pulling latest changes...");

            match cfgd_core::daemon::git_pull_sync(&target_dir) {
                Ok(true) => printer.success("Pulled new changes"),
                Ok(false) => printer.success("Already up to date"),
                Err(e) => printer.warning(&format!(
                    "Pull failed: {} — continuing with existing state",
                    e
                )),
            }

            return Ok(Some(target_dir));
        }

        printer.error(&format!(
            "Directory already exists: {} — remove it or use a different URL",
            target_dir.display()
        ));
        return Ok(None);
    }

    // Clone the repository
    printer.info(&format!("Cloning {} ...", url));

    match cfgd_core::sources::git_clone_with_fallback(url, &target_dir) {
        Ok(()) => {
            printer.success(&format!("Cloned to {}", target_dir.display()));
        }
        Err(e) => {
            printer.error(&e);
            return Ok(None);
        }
    }

    Ok(Some(target_dir))
}

fn init_local(printer: &Printer) -> anyhow::Result<Option<PathBuf>> {
    let config_dir = std::env::current_dir()?;
    let config_path = config_dir.join("cfgd.yaml");

    if config_path.exists() {
        printer.info(&format!(
            "Found existing cfgd.yaml at {}",
            config_dir.display()
        ));
        return Ok(Some(config_dir));
    }

    // Interactive wizard
    printer.subheader("New Configuration");

    let default_name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-config")
        .to_string();

    let config_name = printer.prompt_text("Config name", &default_name)?;

    let profiles_dir = config_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir)?;

    // Profile template selection
    let templates = vec![
        "minimal — packages only".to_string(),
        "standard — packages, files, system".to_string(),
        "empty — blank profile".to_string(),
    ];
    let template_choice = printer.prompt_select("Profile template", &templates)?;

    let profile_content = if template_choice.starts_with("minimal") {
        format!(
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables:
    EDITOR: "{}"
  packages: {{}}
"#,
            std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string())
        )
    } else if template_choice.starts_with("standard") {
        format!(
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables:
    EDITOR: "{}"
  packages: {{}}
  files:
    managed: []
    permissions: {{}}
  system: {{}}
"#,
            std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string())
        )
    } else {
        r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
  packages: {}
"#
        .to_string()
    };

    let profile_path = profiles_dir.join("default.yaml");
    if !profile_path.exists() {
        std::fs::write(&profile_path, &profile_content)?;
        printer.success("Created profiles/default.yaml");
    }

    // Create cfgd.yaml
    let config_content = format!(
        r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: {config_name}
spec:
  profile: default
"#
    );
    std::fs::write(&config_path, &config_content)?;
    printer.success("Created cfgd.yaml");

    // Initialize git if not already a repo
    if !config_dir.join(".git").exists() {
        match git2::Repository::init(&config_dir) {
            Ok(_) => printer.success("Initialized git repository"),
            Err(e) => printer.warning(&format!("Could not init git repo: {}", e)),
        }
    }

    // Offer git remote setup
    offer_git_remote_setup(printer, &config_dir)?;

    Ok(Some(config_dir))
}

/// Source-aware init flow for `cfgd init --from` when the cloned repo has cfgd-source.yaml.
fn init_from_source(
    printer: &Printer,
    url: &str,
    source_dir: &Path,
    manifest: cfgd_core::config::ConfigSourceDocument,
) -> anyhow::Result<()> {
    printer.newline();
    printer.subheader("Detected Config Source");
    printer.key_value("Source", &manifest.metadata.name);
    if let Some(ref version) = manifest.metadata.version {
        printer.key_value("Version", version);
    }
    if let Some(ref desc) = manifest.metadata.description {
        printer.key_value("Description", desc);
    }

    let source_name = manifest.metadata.name.clone();
    let provides = &manifest.spec.provides;
    let profile_names = config::source_profile_names(provides);

    if profile_names.is_empty() {
        printer.warning("Source provides no profiles");
        printer.info("Treating as a plain config repo instead");
        // Fall through to normal init (caller already returned if we do)
        return Ok(());
    }

    // Step 3: Platform auto-detection
    let platform = config::detect_platform();
    let platform_display = platform
        .distro
        .as_deref()
        .unwrap_or(&platform.os)
        .to_string();
    let platform_profile_path =
        config::match_platform_profile(&platform, &provides.platform_profiles);
    if let Some(ref path) = platform_profile_path {
        printer.info(&format!(
            "Detected platform: {} -> applying platform profile ({})",
            platform_display, path
        ));
    } else if !provides.platform_profiles.is_empty() {
        printer.info(&format!(
            "No platform profile match for '{}' — skipping platform layer",
            platform_display
        ));
    }

    // Step 4: Profile selection
    printer.newline();
    let selected_profile = if profile_names.len() == 1 {
        printer.info(&format!("One profile available: {}", profile_names[0]));
        profile_names[0].clone()
    } else {
        // Show detailed info if available
        if !provides.profile_details.is_empty() {
            for detail in &provides.profile_details {
                let desc = detail.description.as_deref().unwrap_or("(no description)");
                let inherits = if detail.inherits.is_empty() {
                    String::new()
                } else {
                    format!(" (inherits: {})", detail.inherits.join(", "))
                };
                printer.key_value(&detail.name, &format!("{}{}", desc, inherits));
            }
            printer.newline();
        }

        let selection = printer.prompt_select("Select a profile", &profile_names)?;
        selection.clone()
    };
    printer.success(&format!("Selected profile: {}", selected_profile));

    // Step 5: Policy tier review
    let policy_result = review_policy_tiers(printer, &manifest.spec.policy)?;

    // Step 6: Create local config with source subscription
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let config_dir = PathBuf::from(&home).join(".config").join("cfgd");
    std::fs::create_dir_all(&config_dir)?;

    // Move the source into the proper cache directory
    let cache_dir = cfgd_core::sources::SourceManager::default_cache_dir()?;
    let cached_source_dir = cache_dir.join(&source_name);
    if !cached_source_dir.exists() && source_dir != cached_source_dir {
        std::fs::create_dir_all(&cache_dir)?;
        // Copy (not move) because the clone location may be user-visible
        copy_dir_recursive(source_dir, &cached_source_dir)?;
    }

    let profiles_dir = config_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir)?;

    // Create minimal local profile
    let local_profile = r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
  packages: {}
"#;
    let profile_path = profiles_dir.join("default.yaml");
    if !profile_path.exists() {
        std::fs::write(&profile_path, local_profile)?;
    }

    // Build source subscription
    let opt_in_items = policy_result.opt_in.clone();
    let reject_value = if policy_result.rejected.is_empty() {
        String::new()
    } else {
        format!(
            "\n        reject:\n{}",
            policy_result
                .rejected
                .iter()
                .map(|r| format!("          {}: null", r))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let opt_in_section = if opt_in_items.is_empty() {
        String::new()
    } else {
        format!(
            "\n        opt-in:\n{}",
            opt_in_items
                .iter()
                .map(|o| format!("          - {}", o))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let config_content = format!(
        r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: my-machine
spec:
  profile: default
  sources:
    - name: {source_name}
      origin:
        type: git
        url: {url}
        branch: main
      subscription:
        profile: {selected_profile}
        priority: 500
        accept-recommended: {accept_rec}{opt_in_section}{reject_value}
      sync:
        interval: 1h
        auto-apply: false
"#,
        accept_rec = policy_result.accept_recommended,
    );

    let config_path = config_dir.join("cfgd.yaml");
    std::fs::write(&config_path, &config_content)?;
    printer.success(&format!("Created config at {}", config_path.display()));

    // Initialize git repo for local config
    if !config_dir.join(".git").exists() {
        match git2::Repository::init(&config_dir) {
            Ok(_) => printer.success("Initialized local git repository"),
            Err(e) => printer.warning(&format!("Could not init git repo: {}", e)),
        }
    }

    // Update state store
    let state = open_state_store()?;
    state.upsert_config_source(
        &source_name,
        url,
        "main",
        None,
        manifest.metadata.version.as_deref(),
        None,
    )?;

    // Step 6b: Pre-bootstrap diagnostics
    printer.newline();
    run_pre_bootstrap_diagnostics(printer)?;

    // Step 7: Plan + apply
    printer.newline();
    printer.header("Bootstrap Plan");
    printer.newline();

    let cfg = config::load_config(&config_path)?;
    let resolved = config::resolve_profile("default", &profiles_dir)?;

    // Compose with the source
    let cache_dir_path = cfgd_core::sources::SourceManager::default_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir_path);
    mgr.load_sources(&cfg.spec.sources, printer)?;

    let mut inputs = Vec::new();
    for source_spec in &cfg.spec.sources {
        if let Some(cached) = mgr.get(&source_spec.name) {
            let mut layers = Vec::new();
            if let Some(ref pn) = source_spec.subscription.profile {
                let src_profiles_dir = mgr.source_profiles_dir(&source_spec.name)?;
                if src_profiles_dir.exists() {
                    match config::resolve_profile(pn, &src_profiles_dir) {
                        Ok(r) => layers = r.layers,
                        Err(e) => {
                            printer.warning(&format!(
                                "Failed to resolve source profile '{}': {}",
                                pn, e
                            ));
                        }
                    }
                }
            }
            inputs.push(CompositionInput {
                source_name: source_spec.name.clone(),
                priority: source_spec.subscription.priority,
                policy: cached.manifest.spec.policy.clone(),
                constraints: cached.manifest.spec.policy.constraints.clone(),
                layers,
                subscription: SubscriptionConfig::from_spec(source_spec),
            });
        }
    }

    let composition_result = composition::compose(&resolved, &inputs)?;
    let mut effective = composition_result.resolved;

    // Resolve manifest files
    packages::resolve_manifest_packages(&mut effective.merged.packages, &config_dir)?;

    let registry = build_registry_with_config(Some(&cfg));
    let store = open_state_store()?;
    let reconciler = Reconciler::new(&registry, &store);

    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers
        .iter()
        .map(|m| m.as_ref())
        .collect();
    let pkg_actions = packages::plan_packages(&effective.merged, &all_managers)?;

    let fm = CfgdFileManager::new(&config_dir, &effective)?;
    let file_actions = fm.plan(&effective.merged)?;

    let plan = reconciler.plan(&effective, file_actions, pkg_actions, Vec::new())?;

    for phase in &plan.phases {
        let items = reconciler::format_plan_items(phase);
        printer.plan_phase(phase.name.display_name(), &items);
    }

    let total = plan.total_actions();
    printer.newline();
    if total == 0 {
        printer.success("Nothing to do — system already matches desired state");
    } else {
        printer.info(&format!("{} action(s) planned", total));
        printer.newline();

        let confirmed = printer
            .prompt_confirm("Apply these changes?")
            .unwrap_or(false);
        if !confirmed {
            printer.info("Skipped apply. Run 'cfgd apply' when ready.");
            return Ok(());
        }

        // Apply
        printer.newline();
        printer.header("Applying Configuration");
        printer.newline();

        let cfg2 = config::load_config(&config_path)?;
        let resolved2 = config::resolve_profile("default", &profiles_dir)?;
        let comp2 = composition::compose(&resolved2, &inputs)?;
        let mut eff2 = comp2.resolved;
        packages::resolve_manifest_packages(&mut eff2.merged.packages, &config_dir)?;

        let mut registry2 = build_registry_with_config(Some(&cfg2));
        let all_managers2: Vec<&dyn cfgd_core::providers::PackageManager> = registry2
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg_actions2 = packages::plan_packages(&eff2.merged, &all_managers2)?;

        let mut fm2 = CfgdFileManager::new(&config_dir, &eff2)?;
        let (backend_name, age_key_path) = if let Some(ref sc) = cfg2.spec.secrets {
            (
                sc.backend.clone(),
                sc.sops.as_ref().and_then(|s| s.age_key.clone()),
            )
        } else {
            ("sops".to_string(), None)
        };
        fm2.set_secret_providers(
            Some(secrets::build_secret_backend(&backend_name, age_key_path)),
            secrets::build_secret_providers(),
        );
        let file_actions2 = fm2.plan(&eff2.merged)?;
        registry2.file_manager = Some(Box::new(fm2));

        let reconciler2 = Reconciler::new(&registry2, &store);
        let plan2 = reconciler2.plan(&eff2, file_actions2, pkg_actions2, Vec::new())?;
        let result = reconciler2.apply(&plan2, &eff2, &config_dir, printer, None, &[])?;

        printer.newline();
        let status = print_apply_result(&result, printer);
        if status == cfgd_core::state::ApplyStatus::Failed {
            return Ok(());
        }
    }

    // Daemon install offer
    printer.newline();
    let install_daemon = printer
        .prompt_confirm("Install cfgd daemon for continuous sync?")
        .unwrap_or(false);

    if install_daemon {
        let config_path_abs = std::fs::canonicalize(&config_path).unwrap_or(config_path.clone());
        match cfgd_core::daemon::install_service(&config_path_abs, Some("default")) {
            Ok(()) => print_daemon_install_success(printer),
            Err(e) => {
                printer.warning(&format!("Could not install daemon: {}", e));
                printer.info("Install later with: cfgd daemon --install");
            }
        }
    }

    // Summary
    printer.newline();
    printer.header("Bootstrap Complete");
    printer.newline();
    printer.success(&format!("Source: {} ({})", source_name, url));
    printer.success(&format!("Profile: {}", selected_profile));
    printer.success(&format!("Config: {}", config_dir.display()));
    printer.newline();
    printer.info("Useful commands:");
    printer.info("  cfgd status         — view current state");
    printer.info("  cfgd plan           — preview changes");
    printer.info("  cfgd apply          — apply changes");
    printer.info("  cfgd source show    — view source details");

    Ok(())
}

/// Review policy tiers interactively during source-aware init.
struct PolicyReviewResult {
    accept_recommended: bool,
    opt_in: Vec<String>,
    rejected: Vec<String>,
}

fn review_policy_tiers(
    printer: &Printer,
    policy: &config::ConfigSourcePolicy,
) -> anyhow::Result<PolicyReviewResult> {
    printer.newline();
    printer.subheader("Policy Review");

    let required_count = count_policy_items(&policy.required);
    let locked_count = count_policy_items(&policy.locked);
    let recommended_count = count_policy_items(&policy.recommended);
    let optional_profiles = &policy.optional.profiles;

    // Show required + locked (mandatory, no prompt)
    if locked_count > 0 || required_count > 0 {
        printer.newline();
        printer.info("Required (always applied):");
        if locked_count > 0 {
            display_policy_items(printer, &policy.locked, "  ");
        }
        if required_count > 0 {
            display_policy_items(printer, &policy.required, "  ");
        }
    }

    // Prompt for recommended (default yes)
    let accept_recommended = if recommended_count > 0 {
        printer.newline();
        printer.info("Recommended:");
        display_policy_items(printer, &policy.recommended, "  ");
        printer.newline();
        printer
            .prompt_confirm_with_default("Accept recommended items?", true)
            .unwrap_or(true)
    } else {
        false
    };

    // Prompt for optional profiles (default no each)
    let mut opt_in = Vec::new();
    if !optional_profiles.is_empty() {
        printer.newline();
        printer.info("Optional profiles:");
        for profile in optional_profiles {
            printer.info(&format!("  {}", profile));
        }
        printer.newline();
        for profile in optional_profiles {
            let accepted = printer
                .prompt_confirm_with_default(&format!("Opt in to '{}'?", profile), false)
                .unwrap_or(false);
            if accepted {
                opt_in.push(profile.clone());
            }
        }
    }

    Ok(PolicyReviewResult {
        accept_recommended,
        opt_in,
        rejected: Vec::new(),
    })
}

/// Offer to set up a git remote for the config repo.
fn offer_git_remote_setup(printer: &Printer, config_dir: &Path) -> anyhow::Result<()> {
    // Check if remote already exists
    if let Ok(repo) = git2::Repository::open(config_dir)
        && repo.find_remote("origin").is_ok()
    {
        return Ok(());
    }

    let setup = printer
        .prompt_confirm_with_default("Set up a git remote for this config repo?", false)
        .unwrap_or(false);
    if !setup {
        return Ok(());
    }

    let options = vec![
        "Enter URL manually".to_string(),
        "I'll set it up later".to_string(),
    ];

    // Offer gh repo create as an option if gh is available
    let has_gh = which("gh");
    let options = if has_gh {
        vec![
            "Create with gh (GitHub CLI)".to_string(),
            "Enter URL manually".to_string(),
            "I'll set it up later".to_string(),
        ]
    } else {
        options
    };

    let choice = printer.prompt_select("How to set up the remote?", &options)?;

    if choice.starts_with("Create with gh") {
        // Print the command for the user — we can't shell out from cli/ per the architecture rules
        let repo_name = config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cfgd-config");
        printer.newline();
        printer.info("Run this command to create and push:");
        printer.info(&format!(
            "  gh repo create {} --private --source=. --push",
            repo_name
        ));
    } else if choice.starts_with("Enter URL") {
        let url = printer.prompt_text("Remote URL (git@... or https://...)", "")?;
        if !url.is_empty() {
            match git2::Repository::open(config_dir) {
                Ok(repo) => match repo.remote("origin", &url) {
                    Ok(_) => printer.success(&format!("Added remote 'origin' -> {}", url)),
                    Err(e) => printer.warning(&format!("Could not add remote: {}", e)),
                },
                Err(e) => printer.warning(&format!("Could not open repo: {}", e)),
            }
        }
    }

    Ok(())
}

/// Run quick pre-bootstrap diagnostics before plan/apply.
fn run_pre_bootstrap_diagnostics(printer: &Printer) -> anyhow::Result<()> {
    printer.subheader("Pre-Bootstrap Diagnostics");

    let mut all_ok = true;

    // Git
    if which("git") {
        printer.success("git: found");
    } else {
        printer.error("git: not found — required for cfgd");
        all_ok = false;
    }

    // Package manager availability
    let registry = build_registry();
    let mut shown = std::collections::HashSet::new();
    for mgr in &registry.package_managers {
        let name = mgr.name();
        if name == "brew-tap" || name == "brew-cask" {
            continue;
        }
        if !shown.insert(name.to_string()) {
            continue;
        }
        if mgr.is_available() {
            printer.success(&format!("{}: available", name));
        } else if mgr.can_bootstrap() {
            printer.info(&format!(
                "{}: not found — will be auto-bootstrapped if needed",
                name
            ));
        }
    }

    // State store
    match StateStore::open_default() {
        Ok(_) => printer.success("State store: accessible"),
        Err(e) => {
            printer.warning(&format!("State store: {}", e));
            all_ok = false;
        }
    }

    if !all_ok {
        printer.newline();
        printer.warning("Some checks failed — bootstrap may encounter issues");
    }

    Ok(())
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    cfgd_core::copy_dir_recursive(src, dst)?;
    Ok(())
}

fn bootstrap_profile_select(
    config_dir: &Path,
    printer: &Printer,
) -> anyhow::Result<Option<String>> {
    let profiles_dir = config_dir.join("profiles");

    if !profiles_dir.exists() {
        printer.warning("No profiles directory found");
        return Ok(None);
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(&profiles_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            profiles.push(stem.to_string());
        }
    }

    if profiles.is_empty() {
        printer.warning("No profile files found in profiles/");
        return Ok(None);
    }

    profiles.sort();

    let selected = if profiles.len() == 1 {
        let name = &profiles[0];
        printer.info(&format!("Found one profile: {}", name));
        name.clone()
    } else {
        printer.info(&format!("Found {} profiles:", profiles.len()));

        // Show profile summaries
        for name in &profiles {
            let path = profiles_dir.join(format!("{}.yaml", name));
            if let Ok(doc) = config::load_profile(&path) {
                let pkg_count = count_packages(&doc.spec);
                let file_count = doc
                    .spec
                    .files
                    .as_ref()
                    .map(|f| f.managed.len())
                    .unwrap_or(0);
                let inherits = if doc.spec.inherits.is_empty() {
                    String::new()
                } else {
                    format!(" (inherits: {})", doc.spec.inherits.join(", "))
                };
                printer.key_value(
                    name,
                    &format!("{} packages, {} files{}", pkg_count, file_count, inherits),
                );
            }
        }

        printer.newline();
        match printer.prompt_select("Select a profile", &profiles) {
            Ok(selected) => selected.clone(),
            Err(_) => {
                printer.info("No profile selected — aborted");
                return Ok(None);
            }
        }
    };

    printer.success(&format!("Selected profile: {}", selected));
    Ok(Some(selected))
}

fn count_packages(spec: &config::ProfileSpec) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = spec.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
        count += pkgs.pipx.len();
        count += pkgs.dnf.len();
    }
    count
}

fn bootstrap_secrets_setup(config_dir: &Path, printer: &Printer) -> anyhow::Result<()> {
    printer.newline();
    printer.subheader("Secrets Setup");

    let health = secrets::check_secrets_health(config_dir, None);

    if health.sops_available {
        let version_str = health.sops_version.as_deref().unwrap_or("unknown version");
        printer.success(&format!("sops: found ({})", version_str));
    } else {
        printer.info("sops: not installed (optional — required for secret management)");
        printer.info("  Install: https://github.com/getsops/sops#install");
    }

    if health.age_key_exists {
        if let Some(ref path) = health.age_key_path {
            printer.success(&format!("age key: {}", path.display()));
        }
    } else if health.sops_available {
        // Offer to generate age key
        let generate = printer
            .prompt_confirm("Generate age encryption key for secrets?")
            .unwrap_or(false);

        if generate {
            match secrets::init_age_key(config_dir) {
                Ok(key_path) => {
                    printer.success(&format!("Age key generated: {}", key_path.display()));
                }
                Err(e) => {
                    printer.warning(&format!("Could not generate age key: {}", e));
                    printer.info("Generate later with: cfgd secret init");
                }
            }
        } else {
            printer.info("Skipped — generate later with: cfgd secret init");
        }
    }

    // Check for external secret providers
    for (name, available) in &health.providers {
        if *available {
            printer.success(&format!("provider {}: available", name));
        }
    }

    Ok(())
}

fn ensure_config_file(
    config_dir: &Path,
    config_path: &Path,
    profile_name: &str,
    from_url: Option<&str>,
) -> anyhow::Result<()> {
    if config_path.exists() {
        // Update profile in existing config
        let contents = std::fs::read_to_string(config_path)?;
        let mut cfg = config::parse_config(&contents, config_path)?;
        if cfg.spec.profile != profile_name {
            cfg.spec.profile = profile_name.to_string();
            let yaml = serde_yaml::to_string(&cfg)?;
            std::fs::write(config_path, &yaml)?;
        }
        return Ok(());
    }

    // Generate new cfgd.yaml
    let name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-config");

    let origin_section = if let Some(url) = from_url {
        format!(
            r#"  origin:
    type: git
    url: {}
    branch: main
"#,
            url
        )
    } else {
        String::new()
    };

    let config_content = format!(
        r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: {}
spec:
  profile: {}
{}"#,
        name, profile_name, origin_section
    );

    std::fs::write(config_path, &config_content)?;
    Ok(())
}

fn load_config_and_profile(
    cli: &Cli,
    printer: &Printer,
) -> anyhow::Result<(CfgdConfig, ResolvedProfile)> {
    let cfg = config::load_config(&cli.config)?;
    let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);

    printer.key_value("Config", &cli.config.display().to_string());
    printer.key_value("Profile", profile_name);

    let resolved = config::resolve_profile(profile_name, &profiles_dir(cli))?;
    Ok((cfg, resolved))
}

fn build_registry() -> ProviderRegistry {
    build_registry_with_config(None)
}

/// DaemonHooks implementation for the workstation binary.
/// Provides concrete provider wiring so cfgd-core's daemon can plan packages/files.
struct WorkstationDaemonHooks;

impl cfgd_core::daemon::DaemonHooks for WorkstationDaemonHooks {
    fn build_registry(&self, config: &CfgdConfig) -> ProviderRegistry {
        build_registry_with_config(Some(config))
    }

    fn plan_files(
        &self,
        config_dir: &std::path::Path,
        resolved: &ResolvedProfile,
    ) -> cfgd_core::errors::Result<Vec<FileAction>> {
        let mut fm = CfgdFileManager::new(config_dir, resolved)?;
        let cfg = config::load_config(&config_dir.join("cfgd.yaml"))?;
        let (backend_name, age_key_path) = if let Some(ref secrets_cfg) = cfg.spec.secrets {
            let name = secrets_cfg.backend.as_str();
            let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
            (name.to_string(), key)
        } else {
            ("sops".to_string(), None)
        };
        let backend = secrets::build_secret_backend(&backend_name, age_key_path);
        let providers = secrets::build_secret_providers();
        fm.set_secret_providers(Some(backend), providers);
        fm.plan(&resolved.merged)
    }

    fn plan_packages(
        &self,
        profile: &cfgd_core::config::MergedProfile,
        managers: &[&dyn cfgd_core::providers::PackageManager],
    ) -> cfgd_core::errors::Result<Vec<cfgd_core::providers::PackageAction>> {
        packages::plan_packages(profile, managers)
    }

    fn extend_registry_custom_managers(
        &self,
        registry: &mut ProviderRegistry,
        packages: &cfgd_core::config::PackagesSpec,
    ) {
        registry
            .package_managers
            .extend(crate::packages::custom_managers(&packages.custom));
    }

    fn expand_tilde(&self, path: &std::path::Path) -> std::path::PathBuf {
        crate::files::expand_tilde(path)
    }
}

fn build_registry_with_profile(spec: &cfgd_core::config::PackagesSpec) -> ProviderRegistry {
    let mut registry = build_registry();
    registry
        .package_managers
        .extend(packages::custom_managers(&spec.custom));
    registry
}

fn build_registry_with_config(cfg: Option<&CfgdConfig>) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.package_managers = packages::all_package_managers();

    // Register system configurators based on OS
    use crate::system::*;
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

    // Environment configurator is available on all Unix systems
    registry
        .system_configurators
        .push(Box::new(EnvironmentConfigurator));

    // Node/infrastructure system configurators (Linux-only, availability-gated)
    registry
        .system_configurators
        .push(Box::new(SysctlConfigurator));
    registry
        .system_configurators
        .push(Box::new(KernelModuleConfigurator));
    registry
        .system_configurators
        .push(Box::new(ContainerdConfigurator));
    registry
        .system_configurators
        .push(Box::new(KubeletConfigurator));
    registry
        .system_configurators
        .push(Box::new(AppArmorConfigurator));
    registry
        .system_configurators
        .push(Box::new(SeccompConfigurator));
    registry
        .system_configurators
        .push(Box::new(CertificateConfigurator));

    // Register secret backend and providers
    let (backend_name, age_key_path) = if let Some(cfg) = cfg
        && let Some(ref secrets_cfg) = cfg.spec.secrets
    {
        let name = secrets_cfg.backend.as_str();
        let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
        (name.to_string(), key)
    } else {
        ("sops".to_string(), None)
    };

    registry.secret_backend = Some(secrets::build_secret_backend(&backend_name, age_key_path));
    registry.secret_providers = secrets::build_secret_providers();

    registry
}

fn print_daemon_install_success(printer: &Printer) {
    if cfg!(target_os = "macos") {
        printer.success("Installed launchd service: com.cfgd.daemon");
        printer.info("Load with: launchctl load ~/Library/LaunchAgents/com.cfgd.daemon.plist");
    } else {
        printer.success("Installed systemd user service: cfgd.service");
        printer.info("Enable with: systemctl --user enable --now cfgd.service");
    }
}

fn open_state_store() -> anyhow::Result<StateStore> {
    Ok(StateStore::open_default()?)
}

/// Display apply result summary via Printer. Returns the status for caller control flow.
fn print_apply_result(
    result: &cfgd_core::reconciler::ApplyResult,
    printer: &Printer,
) -> cfgd_core::state::ApplyStatus {
    match result.status {
        cfgd_core::state::ApplyStatus::Success => {
            printer.success(&format!(
                "Apply complete — {} action(s) succeeded",
                result.succeeded()
            ));
        }
        cfgd_core::state::ApplyStatus::Partial => {
            printer.warning(&format!(
                "Apply partially complete — {} succeeded, {} failed",
                result.succeeded(),
                result.failed()
            ));
        }
        cfgd_core::state::ApplyStatus::Failed => {
            printer.error(&format!(
                "Apply failed — {} action(s) failed",
                result.failed()
            ));
        }
    }
    result.status.clone()
}

fn cmd_plan(
    cli: &Cli,
    printer: &Printer,
    skip: &[String],
    only: &[String],
    module_filter: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Plan");

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let mut registry = build_registry_with_config(Some(&cfg));
    let state = open_state_store()?;

    // Compose with sources if configured
    let source_variables = if !cfg.spec.sources.is_empty() {
        let composition_result = compose_with_sources(cli, &resolved, printer)?;
        let sv = composition_result.source_variables;
        (Some(composition_result.resolved), sv)
    } else {
        (None, std::collections::HashMap::new())
    };
    let mut effective_resolved = source_variables.0.unwrap_or(resolved);
    let source_variables = source_variables.1;

    // Resolve manifest files (Brewfile, package.json, etc.) into package lists
    packages::resolve_manifest_packages(&mut effective_resolved.merged.packages, &config_dir)?;

    // Extend registry with custom managers from resolved profile
    registry.package_managers.extend(packages::custom_managers(
        &effective_resolved.merged.packages.custom,
    ));

    let reconciler = Reconciler::new(&registry, &state);

    // Resolve modules
    let module_names = if let Some(mod_name) = module_filter {
        vec![mod_name.to_string()]
    } else {
        effective_resolved.merged.modules.clone()
    };

    let resolved_modules = if !module_names.is_empty() {
        let platform = Platform::detect();
        let mgr_map = managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        modules::resolve_modules(&module_names, &config_dir, &cache_base, &platform, &mgr_map)?
    } else {
        Vec::new()
    };

    // If --module is set, skip profile-level packages/files
    let module_only = module_filter.is_some();
    let (pkg_actions, file_actions, fm) = if module_only {
        (Vec::new(), Vec::new(), None)
    } else {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg = packages::plan_packages(&effective_resolved.merged, &all_managers)?;
        let mut file_mgr = CfgdFileManager::new(&config_dir, &effective_resolved)?;
        if !source_variables.is_empty() {
            file_mgr.set_source_variables(&source_variables);
        }
        let fa = file_mgr.plan(&effective_resolved.merged)?;
        (pkg, fa, Some(file_mgr))
    };

    let mut plan = reconciler.plan(
        &effective_resolved,
        file_actions,
        pkg_actions,
        resolved_modules,
    )?;

    // Apply --skip / --only filters
    filter_plan(&mut plan, skip, only);

    // Show pending decisions (not included in this plan)
    if let Ok(pending) = state.pending_decisions()
        && !pending.is_empty()
    {
        printer.newline();
        printer.subheader("Pending Decisions (not included in this plan)");
        for d in &pending {
            printer.info(&format!(
                "  {} {} — {} by {} (run `cfgd decide accept/reject`)",
                d.tier, d.resource, d.action, d.source,
            ));
        }
    }

    printer.newline();

    for phase in &plan.phases {
        let items = reconciler::format_plan_items(phase);
        printer.plan_phase(phase.name.display_name(), &items);
    }

    // Show diffs for file updates
    if let Some(ref fm) = fm {
        for phase in &plan.phases {
            if phase.name != PhaseName::Files {
                continue;
            }
            for action in &phase.actions {
                if let reconciler::Action::File(FileAction::Update { source, target, .. }) = action
                    && let Ok(target_content) = std::fs::read_to_string(target)
                {
                    let source_content = if crate::files::is_tera_template(source) {
                        fm.render_template_for_display(source).unwrap_or_default()
                    } else {
                        std::fs::read_to_string(source).unwrap_or_default()
                    };
                    printer.newline();
                    printer.subheader(&format!("{}", target.display()));
                    printer.diff(&target_content, &source_content);
                }
            }
        }
    }

    printer.newline();
    let total = plan.total_actions();
    if total == 0 {
        printer.success("Nothing to do — all phases empty");
    } else {
        printer.info(&format!("{} action(s) planned", total));
    }

    Ok(())
}

fn cmd_apply(
    cli: &Cli,
    printer: &Printer,
    phase: Option<&str>,
    yes: bool,
    skip: &[String],
    only: &[String],
    module_filter: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Apply");

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let mut registry = build_registry_with_config(Some(&cfg));
    let state = open_state_store()?;

    // Validate phase name if provided
    let phase_filter = if let Some(p) = phase {
        match p.parse::<PhaseName>() {
            Ok(pn) => Some(pn),
            Err(_) => {
                printer.error(&format!(
                    "Unknown phase '{}'. Valid phases: modules, system, packages, files, secrets, scripts",
                    p
                ));
                return Ok(());
            }
        }
    } else {
        None
    };

    // Compose with sources if configured
    let source_variables = if !cfg.spec.sources.is_empty() {
        let composition_result = compose_with_sources(cli, &resolved, printer)?;
        let sv = composition_result.source_variables;
        (Some(composition_result.resolved), sv)
    } else {
        (None, std::collections::HashMap::new())
    };
    let mut effective_resolved = source_variables.0.unwrap_or(resolved);
    let source_variables = source_variables.1;

    // Resolve manifest files (Brewfile, package.json, etc.) into package lists
    packages::resolve_manifest_packages(&mut effective_resolved.merged.packages, &config_dir)?;

    // Extend registry with custom package managers from config
    registry.package_managers.extend(packages::custom_managers(
        &effective_resolved.merged.packages.custom,
    ));

    // Resolve modules
    let module_names = if let Some(mod_name) = module_filter {
        vec![mod_name.to_string()]
    } else {
        effective_resolved.merged.modules.clone()
    };

    let resolved_modules = if !module_names.is_empty() {
        let platform = Platform::detect();
        let mgr_map = managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        modules::resolve_modules(&module_names, &config_dir, &cache_base, &platform, &mgr_map)?
    } else {
        Vec::new()
    };

    // If --module is set, skip profile-level packages/files
    let module_only = module_filter.is_some();
    let (pkg_actions, file_actions) = if module_only {
        (Vec::new(), Vec::new())
    } else {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg = packages::plan_packages(&effective_resolved.merged, &all_managers)?;

        let mut fm = CfgdFileManager::new(&config_dir, &effective_resolved)?;
        if !source_variables.is_empty() {
            fm.set_source_variables(&source_variables);
        }
        let (backend_name, age_key_path) = if let Some(ref secrets_cfg) = cfg.spec.secrets {
            let name = secrets_cfg.backend.as_str();
            let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
            (name.to_string(), key)
        } else {
            ("sops".to_string(), None)
        };
        fm.set_secret_providers(
            Some(secrets::build_secret_backend(&backend_name, age_key_path)),
            secrets::build_secret_providers(),
        );
        let fa = fm.plan(&effective_resolved.merged)?;

        // Register the file manager so the reconciler delegates through the trait
        registry.file_manager = Some(Box::new(fm));
        (pkg, fa)
    };

    let reconciler = Reconciler::new(&registry, &state);
    let mut plan = reconciler.plan(
        &effective_resolved,
        file_actions,
        pkg_actions,
        resolved_modules.clone(),
    )?;

    // Apply --skip / --only filters
    filter_plan(&mut plan, skip, only);

    // Check if filtered plan has actions
    let has_actions = if let Some(ref pf) = phase_filter {
        plan.phases
            .iter()
            .any(|p| &p.name == pf && !p.actions.is_empty())
    } else {
        !plan.is_empty()
    };

    if !has_actions {
        printer.success("Nothing to do — everything is up to date");
        return Ok(());
    }

    // Show what will change
    printer.newline();
    for phase_item in &plan.phases {
        if let Some(ref pf) = phase_filter
            && &phase_item.name != pf
        {
            continue;
        }
        let items = reconciler::format_plan_items(phase_item);
        if !items.is_empty() {
            printer.plan_phase(phase_item.name.display_name(), &items);
        }
    }

    // Confirm
    if !yes {
        printer.newline();
        let confirmed = printer
            .prompt_confirm("Apply these changes?")
            .unwrap_or(false);
        if !confirmed {
            printer.info("Aborted");
            return Ok(());
        }
    }

    printer.newline();

    // Apply
    let result = reconciler.apply(
        &plan,
        &effective_resolved,
        &config_dir,
        printer,
        phase_filter.as_ref(),
        &resolved_modules,
    )?;

    printer.newline();
    print_apply_result(&result, printer);

    Ok(())
}

fn cmd_status(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Status");

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let state = open_state_store()?;

    printer.newline();

    // Last apply
    if let Some(last) = state.last_apply()? {
        printer.subheader("Last Apply");
        printer.key_value("Time", &last.timestamp);
        printer.key_value("Profile", &last.profile);
        printer.key_value(
            "Status",
            match last.status {
                cfgd_core::state::ApplyStatus::Success => "success",
                cfgd_core::state::ApplyStatus::Partial => "partial",
                cfgd_core::state::ApplyStatus::Failed => "failed",
            },
        );
        if let Some(ref summary) = last.summary {
            printer.key_value("Summary", summary);
        }
    } else {
        printer.info("No applies recorded yet");
    }

    // Drift summary
    let drift_events = state.unresolved_drift()?;
    printer.newline();
    printer.subheader("Drift");
    if drift_events.is_empty() {
        printer.success("No drift detected");
    } else {
        for event in &drift_events {
            let source_info = if event.source != "local" {
                format!(" [{}]", event.source)
            } else {
                String::new()
            };
            printer.warning(&format!(
                "{} {} — want: {}, have: {}{}",
                event.resource_type,
                event.resource_id,
                event.expected.as_deref().unwrap_or("?"),
                event.actual.as_deref().unwrap_or("?"),
                source_info,
            ));
        }
    }

    // Config sources
    if !cfg.spec.sources.is_empty() {
        printer.newline();
        printer.subheader("Config Sources");
        let source_records = state.config_sources()?;
        if source_records.is_empty() {
            for source in &cfg.spec.sources {
                printer.key_value(&source.name, "not yet fetched");
            }
        } else {
            let rows: Vec<Vec<String>> = source_records
                .iter()
                .map(|s| {
                    vec![
                        s.name.clone(),
                        s.status.clone(),
                        s.source_version.clone().unwrap_or_else(|| "-".into()),
                        s.last_fetched.clone().unwrap_or_else(|| "never".into()),
                    ]
                })
                .collect();
            printer.table(&["Source", "Status", "Version", "Last Fetched"], &rows);
        }
    }

    // Pending decisions
    let pending = state.pending_decisions()?;
    if !pending.is_empty() {
        printer.newline();
        printer.subheader("Pending Decisions");
        display_pending_decisions(printer, &pending);
    }

    // Modules
    if !resolved.merged.modules.is_empty() {
        printer.newline();
        printer.subheader("Modules");

        let config_dir = config_dir(cli);
        let all_modules = modules::load_modules(&config_dir).unwrap_or_default();
        let module_states = state.module_states().unwrap_or_default();
        let state_map: std::collections::HashMap<String, cfgd_core::state::ModuleStateRecord> =
            module_states
                .into_iter()
                .map(|s| (s.module_name.clone(), s))
                .collect();

        for mod_name in &resolved.merged.modules {
            let (pkg_count, file_count) = if let Some(m) = all_modules.get(mod_name) {
                (m.spec.packages.len(), m.spec.files.len())
            } else {
                (0, 0)
            };

            let summary = format!("{} pkgs, {} files", pkg_count, file_count);
            if let Some(state_rec) = state_map.get(mod_name) {
                if state_rec.status == "installed" {
                    printer.success(&format!("{}: {}, {}", mod_name, summary, state_rec.status));
                } else {
                    printer.warning(&format!("{}: {}, {}", mod_name, summary, state_rec.status));
                }
            } else {
                printer.info(&format!("{}: {}, not yet applied", mod_name, summary));
            }
        }
    }

    // Managed resources
    let resources = state.managed_resources()?;
    if !resources.is_empty() {
        printer.newline();
        printer.subheader("Managed Resources");
        printer.table(
            &["Type", "Resource", "Source"],
            &resources
                .iter()
                .map(|r| {
                    vec![
                        r.resource_type.clone(),
                        r.resource_id.clone(),
                        r.source.clone(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    Ok(())
}

fn cmd_log(printer: &Printer, count: u32) -> anyhow::Result<()> {
    printer.header("Apply History");

    let state = open_state_store()?;
    let history = state.history(count)?;

    if history.is_empty() {
        printer.newline();
        printer.info("No applies recorded yet");
        return Ok(());
    }

    printer.newline();
    printer.table(
        &["ID", "Time", "Profile", "Status", "Summary"],
        &history
            .iter()
            .map(|record| {
                vec![
                    record.id.to_string(),
                    record.timestamp.clone(),
                    record.profile.clone(),
                    match record.status {
                        cfgd_core::state::ApplyStatus::Success => "success".to_string(),
                        cfgd_core::state::ApplyStatus::Partial => "partial".to_string(),
                        cfgd_core::state::ApplyStatus::Failed => "failed".to_string(),
                    },
                    record.summary.clone().unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect::<Vec<_>>(),
    );

    Ok(())
}

fn print_verify_results(results: &[reconciler::VerifyResult], printer: &Printer) -> (usize, usize) {
    let mut pass_count = 0;
    let mut fail_count = 0;

    for result in results {
        if result.matches {
            pass_count += 1;
            printer.success(&format!(
                "{} {} — {}",
                result.resource_type, result.resource_id, result.expected
            ));
        } else {
            fail_count += 1;
            printer.error(&format!(
                "{} {} — want: {}, have: {}",
                result.resource_type, result.resource_id, result.expected, result.actual
            ));
        }
    }

    (pass_count, fail_count)
}

fn cmd_verify(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Verify");

    let (_cfg, mut resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;

    let registry = build_registry_with_profile(&resolved.merged.packages);
    let state = open_state_store()?;

    printer.newline();

    let results = reconciler::verify(&resolved, &registry, &state, printer, &[])?;

    if results.is_empty() {
        printer.info("No managed resources to verify");
        return Ok(());
    }

    let (pass_count, fail_count) = print_verify_results(&results, printer);

    printer.newline();
    if fail_count == 0 {
        printer.success(&format!(
            "All {} resource(s) match desired state",
            pass_count
        ));
    } else {
        printer.warning(&format!("{} passed, {} failed", pass_count, fail_count));
    }

    Ok(())
}

fn cmd_diff(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Diff");

    let (_cfg, mut resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    // Resolve manifest files
    packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;

    let registry = build_registry_with_profile(&resolved.merged.packages);

    printer.newline();

    // File diffs
    printer.subheader("Files");
    let fm = CfgdFileManager::new(&config_dir, &resolved)?;
    fm.diff(&resolved.merged, printer)?;

    // Package drift
    printer.newline();
    printer.subheader("Packages");
    let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
        .package_managers
        .iter()
        .map(|m| m.as_ref())
        .collect();
    let pkg_actions = packages::plan_packages(&resolved.merged, &all_managers)?;
    let pkg_diffs: Vec<&PackageAction> = pkg_actions
        .iter()
        .filter(|a| !matches!(a, PackageAction::Skip { .. }))
        .collect();
    if pkg_diffs.is_empty() {
        printer.success("No package drift");
    } else {
        for action in &pkg_diffs {
            match action {
                PackageAction::Bootstrap {
                    manager, method, ..
                } => {
                    printer.warning(&format!(
                        "{}: not installed — can bootstrap via {}",
                        manager, method
                    ));
                }
                PackageAction::Install {
                    manager, packages, ..
                } => {
                    printer.warning(&format!("{}: missing — {}", manager, packages.join(", ")));
                }
                PackageAction::Uninstall {
                    manager, packages, ..
                } => {
                    printer.warning(&format!("{}: extra — {}", manager, packages.join(", ")));
                }
                PackageAction::Skip { .. } => {}
            }
        }
    }

    // System drift
    printer.newline();
    printer.subheader("System");
    let available_configurators = registry.available_system_configurators();
    let mut has_system_drift = false;
    for configurator in &available_configurators {
        let key = configurator.name();
        let desired = match resolved.merged.system.get(key) {
            Some(v) => v,
            None => continue,
        };
        match configurator.diff(desired) {
            Ok(drifts) if !drifts.is_empty() => {
                has_system_drift = true;
                for drift in &drifts {
                    printer.warning(&format!(
                        "{}.{}: want {}, have {}",
                        key, drift.key, drift.expected, drift.actual
                    ));
                }
            }
            Err(e) => {
                printer.warning(&format!("{}: error checking drift — {}", key, e));
            }
            _ => {}
        }
    }
    if !has_system_drift {
        printer.success("No system drift");
    }

    Ok(())
}

fn cmd_add_file(cli: &Cli, printer: &Printer, target: &str) -> anyhow::Result<()> {
    printer.header("Add File");

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);

    let file_path = PathBuf::from(target);
    let fm = CfgdFileManager::new(&config_dir, &resolved)?;
    let managed_spec = fm.add_file(&file_path, profile_name)?;

    // Update the profile YAML to include the new file
    let profile_path = config_dir
        .join("profiles")
        .join(format!("{}.yaml", profile_name));
    let mut doc = config::load_profile(&profile_path)?;

    let files = doc
        .spec
        .files
        .get_or_insert_with(config::FilesSpec::default);
    if !files
        .managed
        .iter()
        .any(|m| m.target == managed_spec.target)
    {
        files.managed.push(managed_spec.clone());
    }

    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&profile_path, &yaml)?;

    printer.newline();
    printer.success(&format!("Copied {} to {}", target, managed_spec.source));
    printer.success(&format!(
        "Updated profile '{}' — added to files.managed",
        profile_name
    ));
    printer.key_value("source", &managed_spec.source);
    printer.key_value("target", &managed_spec.target.display().to_string());

    Ok(())
}

fn cmd_add_package(
    cli: &Cli,
    printer: &Printer,
    manager: &str,
    package: &str,
) -> anyhow::Result<()> {
    printer.header("Add Package");

    let (cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);

    let profile_path = config_dir
        .join("profiles")
        .join(format!("{}.yaml", profile_name));
    let mut doc = config::load_profile(&profile_path)?;

    let pkgs = doc.spec.packages.get_or_insert_with(Default::default);
    packages::add_package(manager, package, pkgs)?;

    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&profile_path, &yaml)?;

    printer.newline();
    printer.success(&format!(
        "Added '{}' to {} in profile '{}'",
        package, manager, profile_name
    ));

    Ok(())
}

fn cmd_remove_package(
    cli: &Cli,
    printer: &Printer,
    manager: &str,
    package: &str,
) -> anyhow::Result<()> {
    printer.header("Remove Package");

    let (cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);

    let profile_path = config_dir
        .join("profiles")
        .join(format!("{}.yaml", profile_name));
    let mut doc = config::load_profile(&profile_path)?;

    let pkgs = doc.spec.packages.get_or_insert_with(Default::default);
    let removed = packages::remove_package(manager, package, pkgs)?;

    if removed {
        let yaml = serde_yaml::to_string(&doc)?;
        std::fs::write(&profile_path, &yaml)?;

        printer.newline();
        printer.success(&format!(
            "Removed '{}' from {} in profile '{}'",
            package, manager, profile_name
        ));
    } else {
        printer.newline();
        printer.warning(&format!(
            "'{}' not found in {} for profile '{}'",
            package, manager, profile_name
        ));
    }

    Ok(())
}

fn cmd_remove_file(cli: &Cli, printer: &Printer, target: &str) -> anyhow::Result<()> {
    printer.header("Remove File");

    let (cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);

    let profile_path = config_dir
        .join("profiles")
        .join(format!("{}.yaml", profile_name));
    let mut doc = config::load_profile(&profile_path)?;

    let target_path = crate::files::expand_tilde(&PathBuf::from(target));

    let files = match doc.spec.files.as_mut() {
        Some(f) => f,
        None => {
            printer.warning(&format!("No files managed in profile '{}'", profile_name));
            return Ok(());
        }
    };

    let original_len = files.managed.len();
    let mut removed_source = None;
    files.managed.retain(|m| {
        let m_target = crate::files::expand_tilde(&m.target);
        if m_target == target_path {
            removed_source = Some(m.source.clone());
            false
        } else {
            true
        }
    });

    if files.managed.len() == original_len {
        printer.newline();
        printer.warning(&format!(
            "'{}' not found in files.managed for profile '{}'",
            target, profile_name
        ));
        return Ok(());
    }

    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&profile_path, &yaml)?;

    printer.newline();
    printer.success(&format!(
        "Removed '{}' from profile '{}'",
        target, profile_name
    ));

    // Clean up the source copy from the config repo
    if let Some(ref source) = removed_source {
        let source_path = config_dir.join(source);
        if source_path.exists() {
            std::fs::remove_file(&source_path)?;
            printer.success(&format!("Deleted source file: {}", source));
        }
    }

    Ok(())
}

fn cmd_profile_show(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Resolved Profile");

    let (_cfg, resolved) = load_config_and_profile(cli, printer)?;

    printer.newline();
    printer.subheader("Layers");
    for layer in &resolved.layers {
        printer.key_value(
            &layer.profile_name,
            &format!("source={} priority={}", layer.source, layer.priority),
        );
    }

    printer.newline();
    printer.subheader("Variables");
    if resolved.merged.variables.is_empty() {
        printer.info("(none)");
    } else {
        let mut vars: Vec<_> = resolved.merged.variables.iter().collect();
        vars.sort_by_key(|(k, _)| (*k).clone());
        for (key, value) in vars {
            let val_str = match value {
                serde_yaml::Value::String(s) => s.clone(),
                other => format!("{:?}", other),
            };
            printer.key_value(key, &val_str);
        }
    }

    printer.newline();
    printer.subheader("Packages");
    let pkgs = &resolved.merged.packages;
    let mut has_packages = false;
    if let Some(ref brew) = pkgs.brew {
        if !brew.taps.is_empty() {
            printer.key_value("brew taps", &brew.taps.join(", "));
            has_packages = true;
        }
        if !brew.formulae.is_empty() {
            printer.key_value("brew formulae", &brew.formulae.join(", "));
            has_packages = true;
        }
        if !brew.casks.is_empty() {
            printer.key_value("brew casks", &brew.casks.join(", "));
            has_packages = true;
        }
    }
    if let Some(ref apt) = pkgs.apt
        && !apt.packages.is_empty()
    {
        printer.key_value("apt", &apt.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref cargo) = pkgs.cargo
        && !cargo.packages.is_empty()
    {
        printer.key_value("cargo", &cargo.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref npm) = pkgs.npm
        && !npm.global.is_empty()
    {
        printer.key_value("npm", &npm.global.join(", "));
        has_packages = true;
    }
    if !pkgs.pipx.is_empty() {
        printer.key_value("pipx", &pkgs.pipx.join(", "));
        has_packages = true;
    }
    if !pkgs.dnf.is_empty() {
        printer.key_value("dnf", &pkgs.dnf.join(", "));
        has_packages = true;
    }
    if !has_packages {
        printer.info("(none)");
    }

    printer.newline();
    printer.subheader("Files");
    if resolved.merged.files.managed.is_empty() {
        printer.info("(none)");
    } else {
        for file in &resolved.merged.files.managed {
            printer.key_value(&file.source, &file.target.display().to_string());
        }
    }

    if !resolved.merged.system.is_empty() {
        printer.newline();
        printer.subheader("System");
        for key in resolved.merged.system.keys() {
            printer.key_value(key, "(configured)");
        }
    }

    if !resolved.merged.secrets.is_empty() {
        printer.newline();
        printer.subheader("Secrets");
        for secret in &resolved.merged.secrets {
            printer.key_value(&secret.source, &secret.target.display().to_string());
        }
    }

    Ok(())
}

fn cmd_profile_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Available Profiles");

    let profiles_dir = profiles_dir(cli);

    if !profiles_dir.exists() {
        printer.warning(&format!(
            "Profiles directory not found: {}",
            profiles_dir.display()
        ));
        return Ok(());
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(&profiles_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            profiles.push(stem.to_string());
        }
    }

    profiles.sort();

    let active = cli.profile.clone().unwrap_or_else(|| {
        config::load_config(&cli.config)
            .map(|c| c.spec.profile)
            .unwrap_or_default()
    });

    for name in &profiles {
        if *name == active {
            printer.success(&format!("{} (active)", name));
        } else {
            printer.info(name);
        }
    }

    if profiles.is_empty() {
        printer.info("No profiles found");
    }

    Ok(())
}

fn cmd_profile_switch(name: &str, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Switch Profile");
    printer.newline();

    let config_path = PathBuf::from("cfgd.yaml");
    if !config_path.exists() {
        printer.error("No cfgd.yaml found — run 'cfgd init' first");
        return Ok(());
    }

    // Verify the target profile exists
    let profiles_dir = PathBuf::from("profiles");
    let profile_path = profiles_dir.join(format!("{}.yaml", name));
    if !profile_path.exists() {
        printer.error(&format!(
            "Profile '{}' not found at {}",
            name,
            profile_path.display()
        ));

        // List available profiles
        if profiles_dir.exists() {
            let mut available = Vec::new();
            for entry in std::fs::read_dir(&profiles_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    available.push(stem.to_string());
                }
            }
            if !available.is_empty() {
                available.sort();
                printer.info(&format!("Available profiles: {}", available.join(", ")));
            }
        }
        return Ok(());
    }

    // Read current config, update profile field, write back
    let contents = std::fs::read_to_string(&config_path)?;
    let mut cfg: config::CfgdConfig = config::parse_config(&contents, &config_path)?;
    let old_profile = cfg.spec.profile.clone();
    cfg.spec.profile = name.to_string();

    let yaml = serde_yaml::to_string(&cfg)?;
    std::fs::write(&config_path, &yaml)?;

    printer.success(&format!("Switched profile: {} → {}", old_profile, name));
    printer.info("Run 'cfgd plan' to see what would change, then 'cfgd apply' to apply");

    Ok(())
}

// --- Module Commands ---

/// Build a HashMap of manager name → &dyn PackageManager from the registry.
fn managers_map(
    registry: &ProviderRegistry,
) -> std::collections::HashMap<String, &dyn cfgd_core::providers::PackageManager> {
    registry
        .package_managers
        .iter()
        .map(|m| (m.name().to_string(), m.as_ref()))
        .collect()
}

fn cmd_module_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Modules");
    printer.newline();

    let config_dir = config_dir(cli);
    let all_modules = modules::load_modules(&config_dir)?;

    if all_modules.is_empty() {
        printer.info("No modules found");
        printer.info(&format!("Add modules to {}/modules/", config_dir.display()));
        return Ok(());
    }

    // Load profile to determine which modules are active
    let active_modules: Vec<String> = if cli.config.exists() {
        let (_, resolved) = load_config_and_profile(cli, printer)?;
        printer.newline();
        resolved.merged.modules
    } else {
        Vec::new()
    };

    // Load module state from DB
    let state = open_state_store()?;
    let module_states = state.module_states()?;
    let state_map: std::collections::HashMap<String, cfgd_core::state::ModuleStateRecord> =
        module_states
            .into_iter()
            .map(|s| (s.module_name.clone(), s))
            .collect();

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut names: Vec<String> = all_modules.keys().cloned().collect();
    names.sort();

    for name in &names {
        let module = &all_modules[name];
        let in_profile = active_modules.contains(name);
        let pkg_count = module.spec.packages.len();
        let file_count = module.spec.files.len();
        let dep_count = module.spec.depends.len();

        let status = if let Some(state_rec) = state_map.get(name) {
            state_rec.status.clone()
        } else if in_profile {
            "pending".to_string()
        } else {
            "available".to_string()
        };

        let profile_indicator = if in_profile { "yes" } else { "-" };

        rows.push(vec![
            name.clone(),
            profile_indicator.to_string(),
            status,
            format!(
                "{} pkgs, {} files, {} deps",
                pkg_count, file_count, dep_count
            ),
        ]);
    }

    printer.table(&["Module", "Active", "Status", "Contents"], &rows);

    Ok(())
}

fn cmd_module_show(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    printer.header(&format!("Module: {}", name));
    printer.newline();

    let config_dir = config_dir(cli);
    let all_modules = modules::load_modules(&config_dir)?;

    let module = match all_modules.get(name) {
        Some(m) => m,
        None => {
            printer.error(&format!("Module '{}' not found", name));
            let available: Vec<&String> = all_modules.keys().collect();
            if !available.is_empty() {
                let mut sorted: Vec<&str> = available.iter().map(|s| s.as_str()).collect();
                sorted.sort();
                printer.info(&format!("Available modules: {}", sorted.join(", ")));
            }
            return Ok(());
        }
    };

    // Basic info
    if !module.spec.depends.is_empty() {
        printer.key_value("Dependencies", &module.spec.depends.join(", "));
    }
    printer.key_value("Directory", &module.dir.display().to_string());

    // Module state from DB
    let state = open_state_store()?;
    if let Some(state_rec) = state.module_state_by_name(name)? {
        printer.key_value("Status", &state_rec.status);
        printer.key_value("Installed at", &state_rec.installed_at);
        printer.key_value("Packages hash", &state_rec.packages_hash);
        printer.key_value("Files hash", &state_rec.files_hash);
    }

    // Packages
    if !module.spec.packages.is_empty() {
        printer.newline();
        printer.subheader("Packages");

        // Try to resolve packages to show which manager would be used
        let registry = build_registry();
        let mgr_map = managers_map(&registry);
        let platform = Platform::detect();

        for entry in &module.spec.packages {
            let prefer_str = if entry.prefer.is_empty() {
                String::new()
            } else {
                format!(" (prefer: {})", entry.prefer.join(", "))
            };
            let version_str = entry
                .min_version
                .as_ref()
                .map(|v| format!(", min: {}", v))
                .unwrap_or_default();
            let alias_str = if entry.aliases.is_empty() {
                String::new()
            } else {
                let aliases: Vec<String> = entry
                    .aliases
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect();
                format!(", aliases: {}", aliases.join(", "))
            };
            let platform_str = if entry.platforms.is_empty() {
                String::new()
            } else {
                format!(", platforms: {}", entry.platforms.join("/"))
            };

            // Try resolution
            match modules::resolve_package(entry, name, &platform, &mgr_map) {
                Ok(Some(resolved)) => {
                    let ver = resolved
                        .version
                        .as_ref()
                        .map(|v| format!(" ({})", v))
                        .unwrap_or_default();
                    printer.success(&format!(
                        "{} -> {} install {}{}",
                        entry.name, resolved.manager, resolved.resolved_name, ver
                    ));
                }
                Ok(None) => {
                    printer.info(&format!(
                        "{}{} — skipped (platform filter)",
                        entry.name, platform_str
                    ));
                }
                Err(_) => {
                    printer.warning(&format!(
                        "{}{}{}{}{} — unresolved",
                        entry.name, prefer_str, version_str, alias_str, platform_str
                    ));
                }
            }
        }
    }

    // Files
    if !module.spec.files.is_empty() {
        printer.newline();
        printer.subheader("Files");
        for file in &module.spec.files {
            let git_indicator = if modules::is_git_source(&file.source) {
                " (git)"
            } else {
                ""
            };
            printer.key_value(&format!("{}{}", file.source, git_indicator), &file.target);
        }
    }

    // Scripts
    if let Some(ref scripts) = module.spec.scripts
        && !scripts.post_apply.is_empty()
    {
        printer.newline();
        printer.subheader("Post-apply Scripts");
        for script in &scripts.post_apply {
            printer.info(&format!("  {}", script));
        }
    }

    Ok(())
}

fn cmd_module_add(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    printer.header("Add Module");
    printer.newline();

    let config_dir = config_dir(cli);

    // Verify the module exists
    let all_modules = modules::load_modules(&config_dir)?;
    if !all_modules.contains_key(name) {
        printer.error(&format!(
            "Module '{}' not found in {}/modules/",
            name,
            config_dir.display()
        ));
        return Ok(());
    }

    // Load profile
    if !cli.config.exists() {
        printer.error("No cfgd.yaml found — run 'cfgd init' first");
        return Ok(());
    }

    let cfg = config::load_config(&cli.config)?;
    let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);
    let profile_path = profiles_dir(cli).join(format!("{}.yaml", profile_name));

    if !profile_path.exists() {
        printer.error(&format!("Profile '{}' not found", profile_name));
        return Ok(());
    }

    let contents = std::fs::read_to_string(&profile_path)?;
    let mut doc: config::ProfileDocument = serde_yaml::from_str(&contents)?;

    // Check if already present
    if doc.spec.modules.contains(&name.to_string()) {
        printer.info(&format!(
            "Module '{}' is already in profile '{}'",
            name, profile_name
        ));
        return Ok(());
    }

    // Add and write back
    doc.spec.modules.push(name.to_string());
    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&profile_path, &yaml)?;

    printer.success(&format!(
        "Added module '{}' to profile '{}'",
        name, profile_name
    ));
    printer.info("Run 'cfgd plan' to preview changes, then 'cfgd apply'");

    Ok(())
}

fn cmd_module_remove(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    printer.header("Remove Module");
    printer.newline();

    if !cli.config.exists() {
        printer.error("No cfgd.yaml found — run 'cfgd init' first");
        return Ok(());
    }

    let cfg = config::load_config(&cli.config)?;
    let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);
    let profile_path = profiles_dir(cli).join(format!("{}.yaml", profile_name));

    if !profile_path.exists() {
        printer.error(&format!("Profile '{}' not found", profile_name));
        return Ok(());
    }

    let contents = std::fs::read_to_string(&profile_path)?;
    let mut doc: config::ProfileDocument = serde_yaml::from_str(&contents)?;

    if !doc.spec.modules.contains(&name.to_string()) {
        printer.info(&format!(
            "Module '{}' is not in profile '{}'",
            name, profile_name
        ));
        return Ok(());
    }

    doc.spec.modules.retain(|m| m != name);
    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&profile_path, &yaml)?;

    // Clean up module state
    let state = open_state_store()?;
    let _ = state.remove_module_state(name);

    printer.success(&format!(
        "Removed module '{}' from profile '{}'",
        name, profile_name
    ));
    printer.info("Run 'cfgd apply' to remove any resources installed by this module");

    Ok(())
}

/// Resolve the secret backend from config, check availability, and validate the file exists.
/// Returns the registry on success, or prints an error and returns None.
fn resolve_secret_backend(
    cli: &Cli,
    printer: &Printer,
    file: &Path,
) -> anyhow::Result<Option<ProviderRegistry>> {
    let cfg = if cli.config.exists() {
        Some(config::load_config(&cli.config)?)
    } else {
        None
    };

    let registry = build_registry_with_config(cfg.as_ref());

    if let Some(ref backend) = registry.secret_backend {
        if !backend.is_available() {
            printer.error(&format!("{}: not installed", backend.name()));
            return Ok(None);
        }
    } else {
        printer.error("No secret backend configured");
        return Ok(None);
    }

    if !file.exists() {
        printer.error(&format!("File not found: {}", file.display()));
        return Ok(None);
    }

    Ok(Some(registry))
}

fn cmd_secret_encrypt(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    printer.header("Secret Encrypt");

    let registry = match resolve_secret_backend(cli, printer, file)? {
        Some(r) => r,
        None => return Ok(()),
    };
    // Backend existence guaranteed by resolve_secret_backend
    let backend = match registry.secret_backend.as_ref() {
        Some(b) => b,
        None => return Ok(()),
    };

    backend.encrypt_file(file)?;

    printer.newline();
    printer.success(&format!(
        "Encrypted {} via {}",
        file.display(),
        backend.name()
    ));

    Ok(())
}

fn cmd_secret_decrypt(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    printer.header("Secret Decrypt");

    let registry = match resolve_secret_backend(cli, printer, file)? {
        Some(r) => r,
        None => return Ok(()),
    };
    let backend = match registry.secret_backend.as_ref() {
        Some(b) => b,
        None => return Ok(()),
    };

    let decrypted = backend.decrypt_file(file)?;
    printer.info(&decrypted);

    Ok(())
}

fn cmd_secret_edit(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    printer.header("Secret Edit");

    let registry = match resolve_secret_backend(cli, printer, file)? {
        Some(r) => r,
        None => return Ok(()),
    };
    let backend = match registry.secret_backend.as_ref() {
        Some(b) => b,
        None => return Ok(()),
    };

    backend.edit_file(file)?;

    printer.newline();
    printer.success(&format!(
        "Edited and re-encrypted {} via {}",
        file.display(),
        backend.name()
    ));

    Ok(())
}

fn cmd_secret_init(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Secret Init");

    let config_dir = config_dir(cli);
    let key_path = secrets::init_age_key(&config_dir)?;

    printer.newline();
    printer.success(&format!("Age key: {}", key_path.display()));

    let sops_config = config_dir.join(".sops.yaml");
    if sops_config.exists() {
        printer.success(&format!(".sops.yaml: {}", sops_config.display()));
    }

    printer.info("Secrets setup complete — files can now be encrypted with 'cfgd secret encrypt'");

    Ok(())
}

fn cmd_doctor(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Doctor");
    printer.newline();

    let mut all_ok = true;

    // Check config file
    let loaded_cfg = if cli.config.exists() {
        match config::load_config(&cli.config) {
            Ok(cfg) => {
                printer.success(&format!("Config file: {} (valid)", cli.config.display()));
                printer.key_value("Name", &cfg.metadata.name);
                printer.key_value("Profile", &cfg.spec.profile);
                Some(cfg)
            }
            Err(e) => {
                printer.error(&format!("Config file: {} — {}", cli.config.display(), e));
                all_ok = false;
                None
            }
        }
    } else {
        printer.warning(&format!(
            "Config file not found: {} — run 'cfgd init' to create one",
            cli.config.display()
        ));
        all_ok = false;
        None
    };

    // Check git
    if which("git") {
        printer.success("git: found");
    } else {
        printer.error("git: not found — install git to use cfgd");
        all_ok = false;
    }

    // Secrets health check
    printer.newline();
    printer.subheader("Secrets");

    let config_dir = config_dir(cli);
    let age_key_override = loaded_cfg
        .as_ref()
        .and_then(|c| c.spec.secrets.as_ref())
        .and_then(|s| s.sops.as_ref())
        .and_then(|s| s.age_key.as_ref());

    let health = secrets::check_secrets_health(&config_dir, age_key_override.map(|p| p.as_path()));

    if health.sops_available {
        let version_str = health.sops_version.as_deref().unwrap_or("unknown version");
        printer.success(&format!("sops: found ({})", version_str));
    } else {
        printer.warning(
            "sops: not found — required for secrets (https://github.com/getsops/sops#install)",
        );
    }

    if health.age_key_exists {
        if let Some(ref path) = health.age_key_path {
            printer.success(&format!("age key: {}", path.display()));
        }
    } else {
        if let Some(ref path) = health.age_key_path {
            printer.warning(&format!(
                "age key: not found at {} — run 'cfgd init' to generate",
                path.display()
            ));
        }
    }

    if health.sops_config_exists {
        if let Some(ref path) = health.sops_config_path {
            printer.success(&format!(".sops.yaml: {}", path.display()));
        }
    } else {
        printer.warning(".sops.yaml: not found — will be generated on 'cfgd init'");
    }

    for (name, available) in &health.providers {
        if *available {
            printer.success(&format!("provider {}: available", name));
        } else {
            printer.info(&format!("provider {}: not installed (optional)", name));
        }
    }

    // Package managers
    printer.newline();
    printer.subheader("Package Managers");

    // Resolve profile to get declared managers (including custom) and build registry
    let resolved_packages = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);
        if let Ok(mut resolved) = config::resolve_profile(profile_name, &profiles_dir) {
            let _ = packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir);
            Some(resolved.merged.packages)
        } else {
            None
        }
    } else {
        None
    };

    let registry = if let Some(ref pkgs) = resolved_packages {
        build_registry_with_profile(pkgs)
    } else {
        build_registry()
    };
    let all_managers = &registry.package_managers;

    // Determine which managers are declared in config
    let declared_managers: Vec<String> = if let Some(ref pkgs) = resolved_packages {
        let mut declared = Vec::new();
        if let Some(ref brew) = pkgs.brew
            && (!brew.formulae.is_empty() || !brew.taps.is_empty() || !brew.casks.is_empty())
        {
            declared.push("brew".to_string());
        }
        if let Some(ref apt) = pkgs.apt
            && !apt.packages.is_empty()
        {
            declared.push("apt".to_string());
        }
        if let Some(ref cargo) = pkgs.cargo
            && !cargo.packages.is_empty()
        {
            declared.push("cargo".to_string());
        }
        if let Some(ref npm) = pkgs.npm
            && !npm.global.is_empty()
        {
            declared.push("npm".to_string());
        }
        if !pkgs.pipx.is_empty() {
            declared.push("pipx".to_string());
        }
        if !pkgs.dnf.is_empty() {
            declared.push("dnf".to_string());
        }
        for custom in &pkgs.custom {
            if !custom.packages.is_empty() {
                declared.push(custom.name.clone());
            }
        }
        declared
    } else {
        Vec::new()
    };

    // Deduplicate manager names for display (brew, brew-tap, brew-cask → brew)
    let mut shown_managers = std::collections::HashSet::new();
    for mgr in all_managers.iter() {
        let name = mgr.name();
        // Group brew-tap and brew-cask under "brew"
        let display_name = if name == "brew-tap" || name == "brew-cask" {
            continue; // Skip sub-managers — brew covers them
        } else {
            name
        };

        if !shown_managers.insert(display_name.to_string()) {
            continue;
        }

        let is_declared = declared_managers.iter().any(|d| d == display_name);
        let available = mgr.is_available();

        if is_declared {
            if available {
                printer.success(&format!("{}: available (declared in config)", display_name));
            } else if mgr.can_bootstrap() {
                let method = packages::bootstrap_method(mgr.as_ref());
                printer.warning(&format!(
                    "{}: not found — can auto-bootstrap via {}",
                    display_name, method
                ));
            } else {
                printer.error(&format!(
                    "{}: not found — declared in config but not available",
                    display_name
                ));
                all_ok = false;
            }
        } else if available {
            printer.info(&format!("{}: available (not used in config)", display_name));
        }
    }

    // Modules health
    let module_list: Vec<String> = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);
        config::resolve_profile(profile_name, &profiles_dir)
            .map(|r| r.merged.modules)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    if !module_list.is_empty() {
        printer.newline();
        printer.subheader("Modules");

        let all_modules = modules::load_modules(&config_dir).unwrap_or_default();
        let registry_for_modules = build_registry();
        let mgr_map = managers_map(&registry_for_modules);
        let platform = Platform::detect();

        for mod_name in &module_list {
            if let Some(module) = all_modules.get(mod_name) {
                printer.info(&format!("{}:", mod_name));
                for entry in &module.spec.packages {
                    match modules::resolve_package(entry, mod_name, &platform, &mgr_map) {
                        Ok(Some(resolved)) => {
                            // Check if installed
                            let installed = mgr_map
                                .get(&resolved.manager)
                                .and_then(|m| m.installed_packages().ok())
                                .map(|pkgs| pkgs.contains(&resolved.resolved_name))
                                .unwrap_or(false);
                            if installed {
                                let ver = resolved.version.as_deref().unwrap_or("?");
                                printer.success(&format!(
                                    "  {} {} ({}, {})",
                                    entry.name, ver, resolved.manager, resolved.resolved_name
                                ));
                            } else {
                                printer.error(&format!(
                                    "  {} — not installed ({} {})",
                                    entry.name, resolved.manager, resolved.resolved_name
                                ));
                                all_ok = false;
                            }
                        }
                        Ok(None) => {
                            printer.info(&format!("  {} — skipped (platform)", entry.name));
                        }
                        Err(e) => {
                            printer.error(&format!("  {} — {}", entry.name, e));
                            all_ok = false;
                        }
                    }
                }
            } else {
                printer.error(&format!(
                    "{}: module not found in {}/modules/",
                    mod_name,
                    config_dir.display()
                ));
                all_ok = false;
            }
        }
    }

    // Check state store
    printer.newline();
    printer.subheader("System");

    match StateStore::open_default() {
        Ok(_) => printer.success("State store: accessible"),
        Err(e) => {
            printer.warning(&format!("State store: {}", e));
        }
    }

    // Check profiles directory
    let profiles_dir = profiles_dir(cli);
    if profiles_dir.exists() {
        let count = std::fs::read_dir(&profiles_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("yaml"))
                    .count()
            })
            .unwrap_or(0);
        printer.success(&format!(
            "Profiles directory: {} ({} profiles)",
            profiles_dir.display(),
            count
        ));
    } else {
        printer.warning(&format!(
            "Profiles directory not found: {}",
            profiles_dir.display()
        ));
    }

    // Check config sources
    let doctor_config_path = &cli.config;
    if doctor_config_path.exists()
        && let Ok(cfg) = config::load_config(doctor_config_path)
        && !cfg.spec.sources.is_empty()
    {
        printer.newline();
        printer.subheader("Config Sources");
        let cache_dir = source_cache_dir().ok();
        for source in &cfg.spec.sources {
            let cached = cache_dir.as_ref().and_then(|cd| {
                if cd.join(&source.name).exists() {
                    Some(format!("cached at {}", cd.join(&source.name).display()))
                } else {
                    None
                }
            });
            match cached {
                Some(info) => printer.success(&format!("{}: {}", source.name, info)),
                None => printer.warning(&format!(
                    "{}: not cached (run 'cfgd source update')",
                    source.name
                )),
            }
        }
    }

    printer.newline();
    if all_ok {
        printer.success("All checks passed");
    } else {
        printer.error("Some checks failed — see above");
    }

    Ok(())
}

fn config_dir(cli: &Cli) -> PathBuf {
    cli.config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn profiles_dir(cli: &Cli) -> PathBuf {
    config_dir(cli).join("profiles")
}

fn cmd_daemon(
    cli: &Cli,
    printer: &Printer,
    install: bool,
    uninstall: bool,
    status: bool,
) -> anyhow::Result<()> {
    if status {
        return cmd_daemon_status(printer);
    }

    if install {
        return cmd_daemon_install(cli, printer);
    }

    if uninstall {
        return cmd_daemon_uninstall(printer);
    }

    // Run daemon in foreground
    let config_path = std::fs::canonicalize(&cli.config).unwrap_or_else(|_| cli.config.clone());
    let profile_override = cli.profile.clone();
    let printer = std::sync::Arc::new(cfgd_core::output::Printer::new(if cli.quiet {
        cfgd_core::output::Verbosity::Quiet
    } else if cli.verbose {
        cfgd_core::output::Verbosity::Verbose
    } else {
        cfgd_core::output::Verbosity::Normal
    }));

    let hooks: std::sync::Arc<dyn cfgd_core::daemon::DaemonHooks> =
        std::sync::Arc::new(WorkstationDaemonHooks);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        cfgd_core::daemon::run_daemon(config_path, profile_override, printer, hooks).await
    })?;

    Ok(())
}

fn cmd_daemon_status(printer: &Printer) -> anyhow::Result<()> {
    printer.header("Daemon Status");
    printer.newline();

    match cfgd_core::daemon::query_daemon_status()? {
        Some(status) => {
            printer.success("Daemon is running");
            printer.key_value("PID", &status.pid.to_string());
            printer.key_value("Uptime", &format!("{}s", status.uptime_secs));
            printer.key_value("Drift count", &status.drift_count.to_string());

            if let Some(ref last) = status.last_reconcile {
                printer.key_value("Last reconcile", last);
            }
            if let Some(ref last) = status.last_sync {
                printer.key_value("Last sync", last);
            }

            if let Some(ref version) = status.update_available {
                printer.newline();
                printer.warning(&format!(
                    "Update available: {} — run 'cfgd upgrade' to install",
                    version
                ));
            }

            printer.newline();
            printer.subheader("Sources");
            printer.table(
                &["Name", "Status", "Drift", "Last Sync"],
                &status
                    .sources
                    .iter()
                    .map(|s| {
                        vec![
                            s.name.clone(),
                            s.status.clone(),
                            s.drift_count.to_string(),
                            s.last_sync.clone().unwrap_or_else(|| "-".to_string()),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
        }
        None => {
            printer.warning("Daemon is not running");
            printer.info("Start with: cfgd daemon");
            printer.info("Install as service: cfgd daemon --install");
        }
    }

    Ok(())
}

fn cmd_daemon_install(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Install Daemon Service");
    printer.newline();

    cfgd_core::daemon::install_service(&cli.config, cli.profile.as_deref())?;

    print_daemon_install_success(printer);

    Ok(())
}

fn cmd_daemon_uninstall(printer: &Printer) -> anyhow::Result<()> {
    printer.header("Uninstall Daemon Service");
    printer.newline();

    if cfg!(target_os = "macos") {
        printer.info("Unloading: launchctl unload ~/Library/LaunchAgents/com.cfgd.daemon.plist");
    } else {
        printer.info("Stopping: systemctl --user disable --now cfgd.service");
    }

    cfgd_core::daemon::uninstall_service()?;
    printer.success("Daemon service removed");

    Ok(())
}

fn cmd_upgrade(printer: &Printer, check_only: bool) -> anyhow::Result<()> {
    use cfgd_core::upgrade;

    if check_only {
        let check = upgrade::check_latest(None)?;

        if check.update_available {
            printer.info(&format!(
                "Update available: {} -> {}",
                check.current, check.latest
            ));
            printer.info("Run 'cfgd upgrade' to install");
            // Exit code 1 = update available (scriptable)
            std::process::exit(1);
        } else {
            printer.success(&format!("cfgd {} is up to date", check.current));
        }

        return Ok(());
    }

    printer.header("Upgrade");
    printer.newline();

    printer.info("Checking for updates...");
    let check = upgrade::check_latest(None)?;

    if !check.update_available {
        printer.success(&format!(
            "cfgd {} is already the latest version",
            check.current
        ));
        return Ok(());
    }

    printer.info(&format!(
        "Update available: {} -> {}",
        check.current, check.latest
    ));

    let release = check
        .release
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("release info not available"))?;

    let asset = upgrade::find_asset_for_platform(release)?;
    printer.key_value("Binary", &asset.name);
    if asset.size > 0 {
        printer.key_value("Size", &format_bytes(asset.size));
    }
    printer.newline();

    printer.info("Downloading...");
    let installed_path = upgrade::download_and_install(release, asset)?;
    printer.success(&format!("Installed to {}", installed_path.display()));

    // Invalidate version cache since we just upgraded
    upgrade::invalidate_cache();

    // Restart daemon if running
    if upgrade::restart_daemon_if_running() {
        printer.info("Daemon restarted with new version");
    }

    printer.newline();
    printer.success(&format!("cfgd upgraded to {}", check.latest));

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn cmd_sync(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Sync");

    let (cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    printer.newline();
    printer.info("Syncing local repo with remote...");

    // Pull local config repo
    match cfgd_core::daemon::git_pull_sync(&config_dir) {
        Ok(true) => printer.success("Pulled new changes from remote"),
        Ok(false) => printer.success("Already up to date"),
        Err(e) => printer.warning(&format!("Pull failed: {}", e)),
    }

    // Sync all configured sources
    if !cfg.spec.sources.is_empty() {
        printer.newline();
        printer.subheader("Sources");

        let cache_dir = source_cache_dir()?;
        let mut mgr = SourceManager::new(&cache_dir);
        let mut changes_detected = false;

        for source_spec in &cfg.spec.sources {
            printer.info(&format!("Syncing source '{}'...", source_spec.name));
            match mgr.load_source(source_spec, printer) {
                Ok(()) => {
                    if let Some(cached) = mgr.get(&source_spec.name) {
                        let commit_short = cached
                            .last_commit
                            .as_deref()
                            .map(|c| &c[..c.len().min(12)])
                            .unwrap_or("unknown");
                        printer.success(&format!(
                            "'{}' synced (commit: {})",
                            source_spec.name, commit_short
                        ));
                        changes_detected = true;
                    }
                }
                Err(e) => {
                    printer.warning(&format!("Failed to sync '{}': {}", source_spec.name, e));
                }
            }
        }

        if changes_detected {
            printer.newline();
            printer.info(
                "Sources updated. Run 'cfgd plan' to see changes, then 'cfgd apply' to reconcile.",
            );
        }
    }

    Ok(())
}

fn cmd_pull(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Pull");

    let (_cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    printer.newline();

    match cfgd_core::daemon::git_pull_sync(&config_dir) {
        Ok(true) => printer.success("Pulled new changes from remote"),
        Ok(false) => printer.success("Already up to date"),
        Err(e) => printer.warning(&format!("Pull failed: {}", e)),
    }

    Ok(())
}

fn default_device_id() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// `cfgd init --server <url> --token <bootstrap-token>`
///
/// Enrolls this device with cfgd-server:
/// 1. Validates arguments
/// 2. Exchanges bootstrap token for permanent device credential
/// 3. Saves credential locally
/// 4. Saves any desired config pushed by server
/// 5. Prints next steps
fn cmd_init_server(
    printer: &Printer,
    server_url: Option<&str>,
    token: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Server Enrollment");
    printer.newline();

    let server_url = match server_url {
        Some(url) => url,
        None => {
            printer.error("--server is required for server enrollment");
            printer.info("Usage: cfgd init --server <url> --token <bootstrap-token>");
            return Ok(());
        }
    };

    let token = match token {
        Some(t) => t,
        None => {
            printer.error("--token is required for server enrollment");
            printer.info("Get a bootstrap token from your team admin");
            return Ok(());
        }
    };

    let device_id = default_device_id();
    printer.key_value("Server", server_url);
    printer.key_value("Device ID", &device_id);
    printer.newline();

    // Create a client with no auth (enrollment doesn't need pre-auth)
    let client = cfgd_core::server_client::ServerClient::new(server_url, None, &device_id);

    // Enroll
    printer.info("Exchanging bootstrap token for device credential...");
    let resp = client
        .enroll(token, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    printer.newline();
    printer.success(&format!("Enrolled as user '{}'", resp.username));
    if let Some(ref team) = resp.team {
        printer.key_value("Team", team);
    }
    printer.key_value("Device", &resp.device_id);

    // Save credential
    let credential = cfgd_core::server_client::DeviceCredential {
        server_url: server_url.to_string(),
        device_id: resp.device_id.clone(),
        api_key: resp.api_key.clone(),
        username: resp.username.clone(),
        team: resp.team.clone(),
        enrolled_at: cfgd_core::utc_now_iso8601(),
    };

    match cfgd_core::server_client::save_credential(&credential) {
        Ok(path) => {
            printer.success(&format!("Credential saved to {}", path.display()));
        }
        Err(e) => {
            printer.error(&format!("Failed to save credential: {}", e));
            printer.warning("You will need to manually provide --api-key for future commands");
        }
    }

    // Save desired config if server pushed one
    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                printer.newline();
                printer.info(&format!(
                    "Server pushed desired config — saved to {}",
                    path.display()
                ));
                printer.info("Run `cfgd plan` to review, then `cfgd apply` to apply");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
            }
        }
    }

    printer.newline();
    printer.header("Next Steps");
    printer.newline();
    printer.info("  cfgd checkin --server-url <url>   — report status to server");
    printer.info("  cfgd plan                         — preview configuration");
    printer.info("  cfgd apply                        — apply configuration");
    printer.info("  cfgd daemon --install              — start background sync");

    Ok(())
}

fn cmd_init_module(printer: &Printer, url: &str, module_name: &str) -> anyhow::Result<()> {
    printer.header("Module Bootstrap");
    printer.newline();

    if !check_prerequisites(printer) {
        return Ok(());
    }

    // Clone the repo
    let cloned_dir = init_from_remote(printer, url)?;
    let config_dir = match cloned_dir {
        Some(dir) => dir,
        None => return Ok(()),
    };

    // Verify the module exists in the cloned repo
    let all_modules = modules::load_modules(&config_dir)?;
    if !all_modules.contains_key(module_name) {
        printer.error(&format!("Module '{}' not found in repository", module_name));
        if !all_modules.is_empty() {
            let mut available: Vec<&str> = all_modules.keys().map(|s| s.as_str()).collect();
            available.sort();
            printer.info(&format!("Available modules: {}", available.join(", ")));
        }
        return Ok(());
    }

    // Create minimal cfgd.yaml with just this module
    let config_path = config_dir.join("cfgd.yaml");
    if !config_path.exists() {
        let profiles_dir = config_dir.join("profiles");
        std::fs::create_dir_all(&profiles_dir)?;

        let profile_content = format!(
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - {}
  variables: {{}}
  packages: {{}}
"#,
            module_name
        );
        std::fs::write(profiles_dir.join("default.yaml"), &profile_content)?;

        let config_name = config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cfgd-config");
        let config_content = format!(
            r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: {}
spec:
  profile: default
"#,
            config_name.trim_start_matches('.')
        );
        std::fs::write(&config_path, &config_content)?;
        printer.success("Created minimal cfgd.yaml with module profile");
    }

    // Resolve module and its dependencies
    let platform = Platform::detect();
    printer.key_value(
        "Platform",
        &format!("{}/{}/{}", platform.os, platform.distro, platform.arch),
    );

    let registry = build_registry();
    let mgr_map = managers_map(&registry);
    let cache_base = modules::default_module_cache_dir()?;

    let module_names = vec![module_name.to_string()];
    let resolved_modules =
        modules::resolve_modules(&module_names, &config_dir, &cache_base, &platform, &mgr_map)?;

    if resolved_modules.is_empty() {
        printer.warning("No actions resolved for this module");
        return Ok(());
    }

    // Show plan
    printer.newline();
    printer.subheader("Module Plan");
    for rm in &resolved_modules {
        printer.info(&format!(
            "  {} ({} packages, {} files)",
            rm.name,
            rm.packages.len(),
            rm.files.len()
        ));
        for pkg in &rm.packages {
            let ver = pkg.version.as_deref().unwrap_or("-");
            printer.info(&format!(
                "    + {} install {} ({})",
                pkg.manager, pkg.resolved_name, ver
            ));
        }
        for file in &rm.files {
            printer.info(&format!("    -> {}", file.target.display()));
        }
    }

    // Confirm
    printer.newline();
    let confirmed = printer
        .prompt_confirm("Apply this module?")
        .unwrap_or(false);
    if !confirmed {
        printer.info("Aborted");
        return Ok(());
    }

    // Apply via reconciler
    let resolved = config::resolve_profile("default", &config_dir.join("profiles"))?;
    let state = open_state_store()?;
    let reconciler = Reconciler::new(&registry, &state);
    let plan = reconciler.plan(&resolved, Vec::new(), Vec::new(), resolved_modules.clone())?;
    let result = reconciler.apply(
        &plan,
        &resolved,
        &config_dir,
        printer,
        None,
        &resolved_modules,
    )?;

    printer.newline();
    print_apply_result(&result, printer);

    printer.newline();
    printer.info("Useful commands:");
    printer.info("  cfgd module show <name>  — view module details");
    printer.info("  cfgd plan               — preview all changes");
    printer.info("  cfgd apply              — apply changes");

    Ok(())
}

fn which(command: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

// --- Source management commands (Phase 9) ---

fn source_cache_dir() -> anyhow::Result<std::path::PathBuf> {
    SourceManager::default_cache_dir().map_err(|e| anyhow::anyhow!(e))
}

fn cmd_source_add(
    cli: &Cli,
    printer: &Printer,
    url: &str,
    name: Option<&str>,
    profile: Option<&str>,
    accept_recommended: bool,
    priority: Option<u32>,
) -> anyhow::Result<()> {
    printer.header("Add Config Source");

    // Infer name from URL if not provided
    let source_name = name
        .map(|s| s.to_string())
        .unwrap_or_else(|| infer_source_name(url));

    // Check if source already exists in config
    let config_path = cli.config.clone();
    if config_path.exists() {
        let cfg = config::load_config(&config_path)?;
        if cfg.spec.sources.iter().any(|s| s.name == source_name) {
            anyhow::bail!(
                "Source '{}' already exists. Use 'cfgd source update' to refresh.",
                source_name
            );
        }
    }

    // Clone and parse the source
    let cache_dir = source_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir);
    let spec = SourceManager::build_source_spec(&source_name, url, profile);
    mgr.load_source(&spec, printer)?;

    let cached = mgr
        .get(&source_name)
        .ok_or_else(|| anyhow::anyhow!("Failed to load source '{}'", source_name))?;

    // Display source manifest info
    let manifest = &cached.manifest;
    printer.subheader("Source Manifest");
    printer.key_value("Name", &manifest.metadata.name);
    if let Some(ref version) = manifest.metadata.version {
        printer.key_value("Version", version);
    }
    if let Some(ref desc) = manifest.metadata.description {
        printer.key_value("Description", desc);
    }

    if !manifest.spec.provides.profiles.is_empty() {
        printer.key_value("Profiles", &manifest.spec.provides.profiles.join(", "));
    }

    // Show policy summary
    let policy = &manifest.spec.policy;
    let required_count = count_policy_items(&policy.required);
    let recommended_count = count_policy_items(&policy.recommended);
    let locked_count = count_policy_items(&policy.locked);

    printer.newline();
    printer.subheader("Policy");
    if locked_count > 0 {
        printer.warning(&format!(
            "{} locked item(s) (cannot override)",
            locked_count
        ));
    }
    if required_count > 0 {
        printer.info(&format!(
            "{} required item(s) (team requirement)",
            required_count
        ));
    }
    if recommended_count > 0 {
        printer.info(&format!("{} recommended item(s)", recommended_count));
    }

    // Show constraints
    let constraints = &manifest.spec.policy.constraints;
    if constraints.no_scripts {
        printer.info("Scripts: blocked");
    }
    if constraints.no_secrets_read {
        printer.info("Secret access: blocked");
    }
    if !constraints.allowed_target_paths.is_empty() {
        printer.info(&format!(
            "Allowed paths: {}",
            constraints.allowed_target_paths.join(", ")
        ));
    }

    // Interactive profile selection if not provided
    let selected_profile = if profile.is_some() {
        profile.map(|s| s.to_string())
    } else if manifest.spec.provides.profiles.len() == 1 {
        Some(manifest.spec.provides.profiles[0].clone())
    } else if !manifest.spec.provides.profiles.is_empty() {
        printer.newline();
        let selection = printer.prompt_select(
            "Select a profile to subscribe to:",
            &manifest.spec.provides.profiles,
        )?;
        Some(selection.clone())
    } else {
        None
    };

    // Interactive priority prompt (when --priority not specified on command line)
    let resolved_priority = if let Some(p) = priority {
        p
    } else {
        printer.newline();
        let input = printer.prompt_text("Set priority", "500")?;
        input
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid priority: '{}' (must be a number)", input))?
    };

    // Conflict preview: check for conflicts with current config before subscribing
    if config_path.exists()
        && let Ok(cfg) = config::load_config(&config_path)
    {
        let pdir = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("profiles");
        let profile_name = cli.profile.as_deref().unwrap_or(&cfg.spec.profile);

        if let Ok(local_resolved) = config::resolve_profile(profile_name, &pdir) {
            // Build a CompositionInput for the prospective source
            let mut preview_layers = Vec::new();
            if let Some(ref pn) = selected_profile
                && let Ok(src_profiles_dir) = mgr.source_profiles_dir(&source_name)
                && src_profiles_dir.exists()
                && let Ok(r) = config::resolve_profile(pn, &src_profiles_dir)
            {
                preview_layers = r.layers;
            }

            let preview_sub = config::SubscriptionSpec {
                profile: selected_profile.clone(),
                priority: resolved_priority,
                accept_recommended,
                ..Default::default()
            };

            let preview_input = CompositionInput {
                source_name: source_name.clone(),
                priority: resolved_priority,
                policy: manifest.spec.policy.clone(),
                constraints: manifest.spec.policy.constraints.clone(),
                layers: preview_layers,
                subscription: SubscriptionConfig {
                    accept_recommended: preview_sub.accept_recommended,
                    opt_in: preview_sub.opt_in.clone(),
                    overrides: preview_sub.overrides.clone(),
                    reject: preview_sub.reject.clone(),
                },
            };

            match composition::compose(&local_resolved, &[preview_input]) {
                Ok(result) => {
                    if result.conflicts.is_empty() {
                        printer.newline();
                        printer.success("No conflicts with current config");
                    } else {
                        printer.newline();
                        printer.subheader("Conflicts with Current Config");
                        for conflict in &result.conflicts {
                            let label = conflict.resolution_type.label();
                            printer.warning(&format!(
                                "  {} {} <- {} ({})",
                                label,
                                conflict.resource_id,
                                conflict.winning_source,
                                conflict.details
                            ));
                        }
                    }
                }
                Err(e) => {
                    printer.warning(&format!("Could not preview conflicts: {}", e));
                }
            }
        }
    }

    // Confirm subscription
    printer.newline();
    if !printer.prompt_confirm("Subscribe to this source?")? {
        printer.info("Cancelled");
        return Ok(());
    }

    // Build the source spec with user choices
    let mut source_spec =
        SourceManager::build_source_spec(&source_name, url, selected_profile.as_deref());
    source_spec.subscription.accept_recommended = accept_recommended;
    source_spec.subscription.priority = resolved_priority;

    // Update cfgd.yaml
    add_source_to_config(&config_path, &source_spec)?;

    // Update state store
    let state = open_state_store()?;
    state.upsert_config_source(
        &source_name,
        url,
        &spec.origin.branch,
        cached.last_commit.as_deref(),
        manifest.metadata.version.as_deref(),
        None,
    )?;

    printer.success(&format!("Subscribed to source '{}'", source_name));
    if let Some(ref profile) = selected_profile {
        printer.key_value("Profile", profile);
    }
    printer.info("Run 'cfgd plan' to see changes from this source");

    Ok(())
}

fn cmd_source_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Config Sources");

    let config_path = cli.config.clone();
    if !config_path.exists() {
        printer.info("No config file found");
        return Ok(());
    }

    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        printer.info("No sources configured");
        return Ok(());
    }

    let state = open_state_store()?;

    let mut rows = Vec::new();
    for source in &cfg.spec.sources {
        let state_info = state.config_source_by_name(&source.name)?;
        let status = state_info
            .as_ref()
            .map(|s| s.status.clone())
            .unwrap_or_else(|| "unknown".into());
        let last_fetched = state_info
            .as_ref()
            .and_then(|s| s.last_fetched.clone())
            .unwrap_or_else(|| "never".into());
        let version = state_info
            .as_ref()
            .and_then(|s| s.source_version.clone())
            .unwrap_or_else(|| "-".into());

        rows.push(vec![
            source.name.clone(),
            source.origin.url.clone(),
            source.subscription.priority.to_string(),
            version,
            status,
            last_fetched,
        ]);
    }

    printer.table(
        &[
            "Name",
            "URL",
            "Priority",
            "Version",
            "Status",
            "Last Fetched",
        ],
        &rows,
    );

    Ok(())
}

fn cmd_source_show(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source_spec = cfg
        .spec
        .sources
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("Source '{}' not found", name))?;

    printer.header(&format!("Source: {}", name));
    printer.key_value("URL", &source_spec.origin.url);
    printer.key_value("Branch", &source_spec.origin.branch);
    printer.key_value("Priority", &source_spec.subscription.priority.to_string());
    printer.key_value(
        "Accept Recommended",
        &source_spec.subscription.accept_recommended.to_string(),
    );
    if let Some(ref profile) = source_spec.subscription.profile {
        printer.key_value("Profile", profile);
    }
    printer.key_value("Sync Interval", &source_spec.sync.interval);
    printer.key_value("Auto Apply", &source_spec.sync.auto_apply.to_string());
    if let Some(ref pin) = source_spec.sync.pin_version {
        printer.key_value("Version Pin", pin);
    }

    // Show state info
    let state = open_state_store()?;
    if let Some(state_info) = state.config_source_by_name(name)? {
        printer.newline();
        printer.subheader("State");
        printer.key_value("Status", &state_info.status);
        if let Some(ref fetched) = state_info.last_fetched {
            printer.key_value("Last Fetched", fetched);
        }
        if let Some(ref commit) = state_info.last_commit {
            printer.key_value("Last Commit", &commit[..commit.len().min(12)]);
        }
        if let Some(ref version) = state_info.source_version {
            printer.key_value("Version", version);
        }
    }

    // Show managed resources from this source
    let resources = state.managed_resources_by_source(name)?;
    if !resources.is_empty() {
        printer.newline();
        printer.subheader("Managed Resources");
        let rows: Vec<Vec<String>> = resources
            .iter()
            .map(|r| vec![r.resource_type.clone(), r.resource_id.clone()])
            .collect();
        printer.table(&["Type", "Resource"], &rows);
    }

    // Load and show manifest from cache
    let cache_dir = source_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir);
    // Populate the manager from the cached source on disk
    if let Err(e) = mgr.load_source(source_spec, printer) {
        printer.warning(&format!("Could not load source manifest: {}", e));
    }
    if let Some(cached) = mgr.get(name) {
        printer.newline();
        printer.subheader("Manifest");
        printer.key_value("Name", &cached.manifest.metadata.name);
        if let Some(ref desc) = cached.manifest.metadata.description {
            printer.key_value("Description", desc);
        }

        let policy = &cached.manifest.spec.policy;
        let locked_count = count_policy_items(&policy.locked);
        let required_count = count_policy_items(&policy.required);
        let recommended_count = count_policy_items(&policy.recommended);

        if locked_count + required_count + recommended_count > 0 {
            printer.newline();
            printer.subheader("Policy Summary");

            if locked_count > 0 {
                printer.key_value("Locked", &locked_count.to_string());
                display_policy_items(printer, &policy.locked, "  ");
            }
            if required_count > 0 {
                printer.key_value("Required", &required_count.to_string());
                display_policy_items(printer, &policy.required, "  ");
            }
            if recommended_count > 0 {
                printer.key_value("Recommended", &recommended_count.to_string());
                display_policy_items(printer, &policy.recommended, "  ");
            }
        }
    }

    Ok(())
}

fn cmd_source_remove(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    keep_all: bool,
    remove_all: bool,
) -> anyhow::Result<()> {
    printer.header(&format!("Remove Source: {}", name));

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if !cfg.spec.sources.iter().any(|s| s.name == name) {
        anyhow::bail!("Source '{}' not found in config", name);
    }

    let state = open_state_store()?;
    let resources = state.managed_resources_by_source(name)?;

    if !resources.is_empty() && !keep_all && !remove_all {
        // Interactive: ask for each resource or batch
        printer.info(&format!(
            "This source manages {} resource(s):",
            resources.len()
        ));
        let rows: Vec<Vec<String>> = resources
            .iter()
            .map(|r| vec![r.resource_type.clone(), r.resource_id.clone()])
            .collect();
        printer.table(&["Type", "Resource"], &rows);
        printer.newline();

        let options = vec![
            "Keep all (resources become locally managed)".to_string(),
            "Remove all".to_string(),
        ];
        let choice = printer.prompt_select("What to do with these resources?", &options)?;

        if choice.starts_with("Keep") {
            // Re-assign resources to local
            for r in &resources {
                state.upsert_managed_resource(
                    &r.resource_type,
                    &r.resource_id,
                    "local",
                    r.last_hash.as_deref(),
                    r.last_applied,
                )?;
            }
            printer.info("Resources transferred to local management");
        }
        // If "Remove all", they'll be cleaned up when state is updated
    } else if keep_all {
        for r in &resources {
            state.upsert_managed_resource(
                &r.resource_type,
                &r.resource_id,
                "local",
                r.last_hash.as_deref(),
                r.last_applied,
            )?;
        }
    }

    // Remove from config
    remove_source_from_config(&config_path, name)?;

    // Remove from state
    state.remove_config_source(name)?;

    // Remove cached data
    let cache_dir = source_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir);
    let _ = mgr.remove_source(name);

    printer.success(&format!("Source '{}' removed", name));
    Ok(())
}

fn cmd_source_update(cli: &Cli, printer: &Printer, name: Option<&str>) -> anyhow::Result<()> {
    printer.header("Update Sources");

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        printer.info("No sources configured");
        return Ok(());
    }

    let cache_dir = source_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir);
    let state = open_state_store()?;

    let sources_to_update: Vec<&config::SourceSpec> = if let Some(name) = name {
        cfg.spec.sources.iter().filter(|s| s.name == name).collect()
    } else {
        cfg.spec.sources.iter().collect()
    };

    if sources_to_update.is_empty()
        && let Some(name) = name
    {
        anyhow::bail!("Source '{}' not found", name);
    }

    for source in &sources_to_update {
        match mgr.load_source(source, printer) {
            Ok(()) => {
                if let Some(cached) = mgr.get(&source.name) {
                    state.upsert_config_source(
                        &source.name,
                        &source.origin.url,
                        &source.origin.branch,
                        cached.last_commit.as_deref(),
                        cached.manifest.metadata.version.as_deref(),
                        source.sync.pin_version.as_deref(),
                    )?;
                    printer.success(&format!("Updated source '{}'", source.name));
                }
            }
            Err(e) => {
                printer.error(&format!("Failed to update source '{}': {}", source.name, e));
            }
        }
    }

    Ok(())
}

fn cmd_source_override(
    cli: &Cli,
    printer: &Printer,
    source_name: &str,
    action: &str,
    path: &str,
    value: Option<&str>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    // Verify source exists in config
    if !cfg.spec.sources.iter().any(|s| s.name == source_name) {
        anyhow::bail!("Source '{}' not found", source_name);
    }

    match action {
        "reject" => {
            printer.info(&format!(
                "Rejecting '{}' from source '{}'",
                path, source_name
            ));
            update_source_rejection(&config_path, source_name, path)?;
            printer.success(&format!("Rejected '{}' from '{}'", path, source_name));
        }
        "set" => {
            let val = value.ok_or_else(|| anyhow::anyhow!("'set' action requires a value"))?;
            printer.info(&format!(
                "Overriding '{}' = '{}' for source '{}'",
                path, val, source_name
            ));
            update_source_override(&config_path, source_name, path, val)?;
            printer.success(&format!(
                "Override set: {} = {} for '{}'",
                path, val, source_name
            ));
        }
        other => {
            anyhow::bail!("Unknown action '{}'. Use 'set' or 'reject'.", other);
        }
    }

    Ok(())
}

fn cmd_source_replace(
    cli: &Cli,
    printer: &Printer,
    old_name: &str,
    new_url: &str,
) -> anyhow::Result<()> {
    printer.header(&format!("Replace Source: {}", old_name));

    // Remove old source (keeping resources)
    cmd_source_remove(cli, printer, old_name, true, false)?;

    // Add new source with same name
    cmd_source_add(
        cli,
        printer,
        new_url,
        Some(old_name),
        None,
        false,
        Some(500),
    )?;

    printer.success(&format!("Source '{}' replaced with {}", old_name, new_url));
    Ok(())
}

fn cmd_source_priority(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    value: Option<u32>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source = cfg
        .spec
        .sources
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("source '{}' not found", name))?;

    match value {
        Some(new_priority) => {
            // Update priority in cfgd.yaml
            let raw_content = std::fs::read_to_string(&config_path)?;
            let mut raw: serde_yaml::Value = serde_yaml::from_str(&raw_content)?;

            let source_entry = find_source_in_config(&mut raw, name)
                .ok_or_else(|| anyhow::anyhow!("source '{}' not found in config file", name))?;

            let subscription = source_entry
                .get_mut("subscription")
                .ok_or_else(|| anyhow::anyhow!("source '{}' has no subscription block", name))?;

            if let Some(mapping) = subscription.as_mapping_mut() {
                mapping.insert(
                    serde_yaml::Value::String("priority".into()),
                    serde_yaml::Value::Number(serde_yaml::Number::from(new_priority)),
                );
            }

            let output = serde_yaml::to_string(&raw)?;
            std::fs::write(&config_path, output)?;

            printer.success(&format!(
                "Source '{}' priority updated: {} -> {}",
                name, source.subscription.priority, new_priority
            ));
        }
        None => {
            printer.key_value("Source", name);
            printer.key_value("Priority", &source.subscription.priority.to_string());
            printer.info("Local config priority is 1000");
        }
    }

    Ok(())
}

// --- Source config helpers ---

fn infer_source_name(url: &str) -> String {
    // Extract name from URL: git@github.com:acme/dev-config.git -> acme-dev-config
    let cleaned = url
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .or_else(|| url.rsplit(':').next())
        .unwrap_or(url);

    // If the path component includes org/repo, use org-repo
    if let Some(rest) = url.strip_prefix("git@")
        && let Some(path) = rest.split(':').nth(1)
    {
        return path.trim_end_matches(".git").replace('/', "-");
    }

    cleaned.to_string()
}

fn count_policy_items(items: &config::PolicyItems) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len() + brew.taps.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        count += pkgs.pipx.len() + pkgs.dnf.len();
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
    }
    count += items.files.len();
    count += items.variables.len();
    count += items.system.len();
    count
}

fn display_policy_items(printer: &Printer, items: &config::PolicyItems, indent: &str) {
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            for f in &brew.formulae {
                printer.info(&format!("{indent}brew formula: {f}"));
            }
            for c in &brew.casks {
                printer.info(&format!("{indent}brew cask: {c}"));
            }
        }
        if let Some(ref apt) = pkgs.apt {
            for p in &apt.packages {
                printer.info(&format!("{indent}apt: {p}"));
            }
        }
        if let Some(ref cargo) = pkgs.cargo {
            for p in &cargo.packages {
                printer.info(&format!("{indent}cargo: {p}"));
            }
        }
        for p in &pkgs.pipx {
            printer.info(&format!("{indent}pipx: {p}"));
        }
        for p in &pkgs.dnf {
            printer.info(&format!("{indent}dnf: {p}"));
        }
        if let Some(ref npm) = pkgs.npm {
            for p in &npm.global {
                printer.info(&format!("{indent}npm: {p}"));
            }
        }
    }
    for f in &items.files {
        printer.info(&format!("{indent}file: {}", f.target.display()));
    }
    for k in items.variables.keys() {
        printer.info(&format!("{indent}variable: {k}"));
    }
    for k in items.system.keys() {
        printer.info(&format!("{indent}system: {k}"));
    }
}

fn display_pending_decisions(printer: &Printer, decisions: &[cfgd_core::state::PendingDecision]) {
    let mut by_source: std::collections::BTreeMap<&str, Vec<&cfgd_core::state::PendingDecision>> =
        std::collections::BTreeMap::new();
    for d in decisions {
        by_source.entry(&d.source).or_default().push(d);
    }
    for (source_name, items) in &by_source {
        printer.info(&format!(
            "{}: {} pending item{}",
            source_name,
            items.len(),
            if items.len() == 1 { "" } else { "s" }
        ));
        for item in items {
            printer.info(&format!(
                "  {} {} — {} ({})",
                item.tier, item.resource, item.summary, item.action
            ));
        }
    }
}

fn add_source_to_config(config_path: &Path, source: &config::SourceSpec) -> anyhow::Result<()> {
    if !config_path.exists() {
        anyhow::bail!("Config file not found: {}", config_path.display());
    }

    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    // Get or create spec.sources array
    let spec = raw
        .get_mut("spec")
        .ok_or_else(|| anyhow::anyhow!("config missing 'spec'"))?;

    let sources = spec
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("spec is not a mapping"))?
        .entry(serde_yaml::Value::String("sources".into()))
        .or_insert(serde_yaml::Value::Sequence(vec![]));

    let seq = sources
        .as_sequence_mut()
        .ok_or_else(|| anyhow::anyhow!("sources is not a sequence"))?;

    let source_value = serde_yaml::to_value(source)?;
    seq.push(source_value);

    let output = serde_yaml::to_string(&raw)?;
    std::fs::write(config_path, output)?;

    Ok(())
}

fn remove_source_from_config(config_path: &Path, name: &str) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }

    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    if let Some(spec) = raw.get_mut("spec")
        && let Some(sources) = spec.get_mut("sources")
        && let Some(seq) = sources.as_sequence_mut()
    {
        seq.retain(|item| {
            item.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n != name)
                .unwrap_or(true)
        });
    }

    let output = serde_yaml::to_string(&raw)?;
    std::fs::write(config_path, output)?;

    Ok(())
}

fn update_source_rejection(
    config_path: &Path,
    source_name: &str,
    path: &str,
) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    if let Some(source) = find_source_in_config(&mut raw, source_name) {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let reject = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?
            .entry(serde_yaml::Value::String("reject".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

        // Parse path like "packages.brew.formulae" and add rejection
        set_nested_yaml_value(reject, path, &serde_yaml::Value::Null)?;
    }

    let output = serde_yaml::to_string(&raw)?;
    std::fs::write(config_path, output)?;

    Ok(())
}

fn update_source_override(
    config_path: &Path,
    source_name: &str,
    path: &str,
    value: &str,
) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    if let Some(source) = find_source_in_config(&mut raw, source_name) {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let overrides = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?
            .entry(serde_yaml::Value::String("overrides".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

        set_nested_yaml_value(
            overrides,
            path,
            &serde_yaml::Value::String(value.to_string()),
        )?;
    }

    let output = serde_yaml::to_string(&raw)?;
    std::fs::write(config_path, output)?;

    Ok(())
}

fn find_source_in_config<'a>(
    raw: &'a mut serde_yaml::Value,
    source_name: &str,
) -> Option<&'a mut serde_yaml::Value> {
    raw.get_mut("spec")?
        .get_mut("sources")?
        .as_sequence_mut()?
        .iter_mut()
        .find(|item| {
            item.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == source_name)
                .unwrap_or(false)
        })
}

// --- Plan filtering for --skip and --only ---

/// Compute the dot-notation resource path for an action.
/// Returns the phase-level prefix and the action-specific path components.
///
/// Examples:
///   PackageAction::Install { manager: "brew", packages: ["ripgrep"] } → "packages.brew"
///   SystemAction::SetValue { configurator: "sysctl", key: "net.ipv4.ip_forward" } → "system.sysctl.net.ipv4.ip_forward"
///   FileAction::Create { target: "/etc/foo" } → "files./etc/foo"
///   SecretAction::Resolve { provider: "1password" } → "secrets.1password"
///   ScriptAction::Run { path: "scripts/setup.sh" } → "scripts.scripts/setup.sh"
fn action_path(phase: &PhaseName, action: &reconciler::Action) -> String {
    let prefix = phase.as_str();
    match action {
        reconciler::Action::Package(pa) => {
            let manager = match pa {
                PackageAction::Bootstrap { manager, .. } => manager,
                PackageAction::Install { manager, .. } => manager,
                PackageAction::Uninstall { manager, .. } => manager,
                PackageAction::Skip { manager, .. } => manager,
            };
            format!("{}.{}", prefix, manager)
        }
        reconciler::Action::System(sa) => match sa {
            reconciler::SystemAction::SetValue {
                configurator, key, ..
            } => format!("{}.{}.{}", prefix, configurator, key),
            reconciler::SystemAction::Skip { configurator, .. } => {
                format!("{}.{}", prefix, configurator)
            }
        },
        reconciler::Action::File(fa) => {
            let target = match fa {
                FileAction::Create { target, .. } => target,
                FileAction::Update { target, .. } => target,
                FileAction::Delete { target, .. } => target,
                FileAction::SetPermissions { target, .. } => target,
                FileAction::Skip { target, .. } => target,
            };
            format!("{}:{}", prefix, target.display())
        }
        reconciler::Action::Secret(sa) => match sa {
            SecretAction::Decrypt { target, .. } => {
                format!("{}:{}", prefix, target.display())
            }
            SecretAction::Resolve {
                provider,
                reference,
                ..
            } => format!("{}.{}.{}", prefix, provider, reference),
            SecretAction::Skip { source, .. } => {
                format!("{}.{}", prefix, source)
            }
        },
        reconciler::Action::Script(sa) => match sa {
            reconciler::ScriptAction::Run { path, .. } => {
                format!("{}:{}", prefix, path.display())
            }
        },
        reconciler::Action::Module(ma) => {
            format!("{}.{}", prefix, ma.module_name)
        }
    }
}

/// Check if a pattern matches an action path.
/// A pattern is a prefix match: "packages.brew" matches "packages.brew.ripgrep".
/// For file/script paths using `:`, "files:" matches all files.
fn pattern_matches(pattern: &str, action_path: &str) -> bool {
    if action_path == pattern {
        return true;
    }
    // "packages" matches "packages.brew.ripgrep"
    // "packages.brew" matches "packages.brew.ripgrep"
    if action_path.starts_with(pattern) && action_path[pattern.len()..].starts_with(['.', ':']) {
        return true;
    }
    // "packages" should also match "packages:..." (colon-separated paths)
    false
}

/// Apply --skip and --only filters to a plan, modifying it in place.
/// For package actions, individual packages can be filtered from install/uninstall lists.
fn filter_plan(plan: &mut reconciler::Plan, skip: &[String], only: &[String]) {
    if skip.is_empty() && only.is_empty() {
        return;
    }

    for phase in &mut plan.phases {
        let mut filtered_actions = Vec::new();

        for action in std::mem::take(&mut phase.actions) {
            // Package install/uninstall actions need per-package granularity
            if let reconciler::Action::Package(ref pa) = action {
                match pa {
                    PackageAction::Install {
                        manager,
                        packages,
                        origin,
                    } => {
                        let kept =
                            filter_package_list(phase.name.as_str(), manager, packages, skip, only);
                        if !kept.is_empty() {
                            filtered_actions.push(reconciler::Action::Package(
                                PackageAction::Install {
                                    manager: manager.clone(),
                                    packages: kept,
                                    origin: origin.clone(),
                                },
                            ));
                        }
                        continue;
                    }
                    PackageAction::Uninstall {
                        manager,
                        packages,
                        origin,
                    } => {
                        let kept =
                            filter_package_list(phase.name.as_str(), manager, packages, skip, only);
                        if !kept.is_empty() {
                            filtered_actions.push(reconciler::Action::Package(
                                PackageAction::Uninstall {
                                    manager: manager.clone(),
                                    packages: kept,
                                    origin: origin.clone(),
                                },
                            ));
                        }
                        continue;
                    }
                    _ => {}
                }
            }

            // Non-package actions: action-level filtering
            let path = action_path(&phase.name, &action);
            let should_skip = skip.iter().any(|s| pattern_matches(s, &path));
            let passes_only = only.is_empty()
                || only
                    .iter()
                    .any(|o| pattern_matches(o, &path) || pattern_matches(&path, o));

            if !should_skip && passes_only {
                filtered_actions.push(action);
            }
        }

        phase.actions = filtered_actions;
    }
}

/// Filter individual packages from an install/uninstall list based on skip/only patterns.
fn filter_package_list(
    phase: &str,
    manager: &str,
    packages: &[String],
    skip: &[String],
    only: &[String],
) -> Vec<String> {
    packages
        .iter()
        .filter(|pkg| {
            let pkg_path = format!("{}.{}.{}", phase, manager, pkg);

            // Check skip: pattern can target the specific package, manager, or phase
            let pkg_skip = skip.iter().any(|s| pattern_matches(s, &pkg_path));

            // Check only: the pattern must cover this package.
            // "packages" covers "packages.brew.ripgrep" (broad → specific)
            // "packages.brew.ripgrep" covers "packages.brew.ripgrep" (exact)
            // But "packages.brew.ripgrep" does NOT cover "packages.brew.fd"
            let pkg_only = only.is_empty()
                || only
                    .iter()
                    .any(|o| pattern_matches(o, &pkg_path) || pattern_matches(&pkg_path, o));

            !pkg_skip && pkg_only
        })
        .cloned()
        .collect()
}

fn set_nested_yaml_value(
    root: &mut serde_yaml::Value,
    path: &str,
    value: &serde_yaml::Value,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = root;

    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Last part: set the value
            if let Some(mapping) = current.as_mapping_mut() {
                mapping.insert(serde_yaml::Value::String(part.to_string()), value.clone());
            }
        } else {
            // Intermediate part: navigate or create
            let mapping = current
                .as_mapping_mut()
                .ok_or_else(|| anyhow::anyhow!("expected mapping at '{}'", part))?;
            current = mapping
                .entry(serde_yaml::Value::String(part.to_string()))
                .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
    }

    Ok(())
}

// --- Plan integration with sources (Phase 9) ---

/// Compose sources with local profile for plan generation.
fn compose_with_sources(
    cli: &Cli,
    local_resolved: &ResolvedProfile,
    printer: &Printer,
) -> anyhow::Result<composition::CompositionResult> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        // No sources, return local profile as-is
        return Ok(composition::CompositionResult {
            resolved: local_resolved.clone(),
            conflicts: Vec::new(),
            source_variables: std::collections::HashMap::new(),
        });
    }

    let cache_dir = source_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.load_sources(&cfg.spec.sources, printer)?;

    let mut inputs = Vec::new();
    for source_spec in &cfg.spec.sources {
        if let Some(cached) = mgr.get(&source_spec.name) {
            // Load source profile layers
            let mut layers = Vec::new();
            if let Some(ref profile_name) = source_spec.subscription.profile {
                let profiles_dir = mgr.source_profiles_dir(&source_spec.name)?;
                if profiles_dir.exists() {
                    match config::resolve_profile(profile_name, &profiles_dir) {
                        Ok(resolved) => {
                            layers = resolved.layers;
                        }
                        Err(e) => {
                            printer.warning(&format!(
                                "Failed to resolve profile '{}' from source '{}': {}",
                                profile_name, source_spec.name, e
                            ));
                        }
                    }
                }
            }

            // Validate security constraints
            for layer in &layers {
                if let Err(e) = composition::validate_constraints(
                    &source_spec.name,
                    &cached.manifest.spec.policy.constraints,
                    &layer.spec,
                ) {
                    printer.error(&format!(
                        "Security violation in source '{}': {}",
                        source_spec.name, e
                    ));
                    continue;
                }
            }

            inputs.push(CompositionInput {
                source_name: source_spec.name.clone(),
                priority: source_spec.subscription.priority,
                policy: cached.manifest.spec.policy.clone(),
                constraints: cached.manifest.spec.policy.constraints.clone(),
                layers,
                subscription: SubscriptionConfig::from_spec(source_spec),
            });
        }
    }

    let result = composition::compose(local_resolved, &inputs)?;

    // Display conflicts
    if !result.conflicts.is_empty() {
        printer.newline();
        printer.subheader("Source Conflicts");
        for conflict in &result.conflicts {
            match conflict.resolution_type {
                composition::ResolutionType::Locked => {
                    printer.warning(&conflict.details);
                }
                composition::ResolutionType::Required => {
                    printer.info(&conflict.details);
                }
                composition::ResolutionType::Rejected => {
                    printer.info(&conflict.details);
                }
                composition::ResolutionType::Override => {
                    printer.info(&conflict.details);
                }
                composition::ResolutionType::Default => {}
            }
        }
    }

    Ok(result)
}

fn cmd_checkin(
    cli: &Cli,
    printer: &Printer,
    server_url: &str,
    api_key: Option<&str>,
    device_id: Option<&str>,
) -> anyhow::Result<()> {
    let (_cfg, resolved) = load_config_and_profile(cli, printer)?;
    let registry = build_registry_with_profile(&resolved.merged.packages);

    // Try stored device credential first, fall back to explicit args
    let stored_cred = cfgd_core::server_client::load_credential().ok().flatten();
    let client = if api_key.is_none() {
        if let Some(ref cred) = stored_cred {
            if cred.server_url.trim_end_matches('/') == server_url.trim_end_matches('/') {
                cfgd_core::server_client::ServerClient::from_credential(cred)
            } else {
                let did = device_id
                    .map(|s| s.to_string())
                    .unwrap_or_else(default_device_id);
                cfgd_core::server_client::ServerClient::new(server_url, None, &did)
            }
        } else {
            let did = device_id
                .map(|s| s.to_string())
                .unwrap_or_else(default_device_id);
            cfgd_core::server_client::ServerClient::new(server_url, None, &did)
        }
    } else {
        let did = device_id
            .map(|s| s.to_string())
            .unwrap_or_else(default_device_id);
        cfgd_core::server_client::ServerClient::new(server_url, api_key, &did)
    };

    // Compute config hash
    let config_yaml = serde_yaml::to_string(&resolved.merged.system).unwrap_or_default();
    let config_hash = {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(config_yaml.as_bytes());
        format!("{:x}", hash)
    };

    // Check in
    let resp = client
        .checkin(&config_hash, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    printer.key_value("Server status", &resp.status);
    printer.key_value("Config changed", &resp.config_changed.to_string());

    // Save desired config from server for next reconcile
    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                printer.warning(&format!(
                    "Server pushed a new desired config — saved to {}",
                    path.display()
                ));
                printer.info("Run `cfgd plan` to review or `cfgd apply` to reconcile");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
                printer.warning("Server sent desired config but failed to save it locally");
            }
        }
    }

    // Collect and report drift
    let mut all_drifts = Vec::new();
    let available = registry.available_system_configurators();

    for configurator in &available {
        let key = configurator.name();
        let desired = match resolved.merged.system.get(key) {
            Some(v) => v,
            None => continue,
        };
        if let Ok(drifts) = configurator.diff(desired) {
            all_drifts.extend(drifts);
        }
    }

    if !all_drifts.is_empty() {
        client
            .report_drift(&all_drifts, printer)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        printer.warning(&format!("{} drift items reported", all_drifts.len()));
    } else {
        printer.success("No drift to report");
    }

    Ok(())
}

fn cmd_decide(
    printer: &Printer,
    action: &str,
    resource: Option<&str>,
    source: Option<&str>,
    all: bool,
) -> anyhow::Result<()> {
    let resolution = match action {
        "accept" => "accepted",
        "reject" => "rejected",
        other => {
            printer.error(&format!(
                "Unknown action '{}'. Use 'accept' or 'reject'.",
                other
            ));
            return Ok(());
        }
    };

    let state = open_state_store()?;

    if all {
        let count = state.resolve_all_decisions(resolution)?;
        if count == 0 {
            printer.info("No pending decisions");
        } else {
            printer.success(&format!(
                "{} {} item{}",
                resolution.to_uppercase(),
                count,
                if count == 1 { "" } else { "s" }
            ));
            printer.info("Changes will take effect on next reconcile");
        }
        return Ok(());
    }

    if let Some(source_name) = source {
        let count = state.resolve_decisions_for_source(source_name, resolution)?;
        if count == 0 {
            printer.info(&format!(
                "No pending decisions for source '{}'",
                source_name
            ));
        } else {
            printer.success(&format!(
                "{} {} item{} from {}",
                resolution.to_uppercase(),
                count,
                if count == 1 { "" } else { "s" },
                source_name,
            ));
            printer.info("Changes will take effect on next reconcile");
        }
        return Ok(());
    }

    if let Some(resource_path) = resource {
        let resolved = state.resolve_decision(resource_path, resolution)?;
        if resolved {
            printer.success(&format!(
                "{}: {} will {} on next reconcile",
                resolution.to_uppercase(),
                resource_path,
                if resolution == "accepted" {
                    "be applied"
                } else {
                    "not be applied"
                }
            ));
        } else {
            printer.warning(&format!(
                "No pending decision found for '{}'",
                resource_path
            ));
        }
        return Ok(());
    }

    // No resource, source, or --all — show pending decisions
    let decisions = state.pending_decisions()?;
    if decisions.is_empty() {
        printer.info("No pending decisions");
        return Ok(());
    }

    printer.subheader("Pending Decisions");
    display_pending_decisions(printer, &decisions);
    printer.newline();
    printer
        .info("Use `cfgd decide accept <resource>` or `cfgd decide reject <resource>` to resolve");
    printer.info("Use `cfgd decide accept --all` or `cfgd decide accept --source <name>` for bulk operations");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        // Create profiles directory with a test profile
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();

        std::fs::write(
            profiles_dir.join("default.yaml"),
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables:
    editor: vim
  packages:
    cargo:
      - bat
"#,
        )
        .unwrap();

        std::fs::write(
            profiles_dir.join("work.yaml"),
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - default
  variables:
    editor: code
  packages:
    cargo:
      - exa
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn bootstrap_state_serialization_roundtrip() {
        let dir = tempfile::tempdir().unwrap();

        let state = BootstrapState {
            repo_url: Some("https://github.com/test/bootstrap.git".to_string()),
            config_dir: "/home/user/.config/cfgd".to_string(),
            profile: Some("work".to_string()),
            phase: BootstrapPhase::Apply,
        };

        save_bootstrap_state(dir.path(), &state).unwrap();
        let loaded = load_bootstrap_state(dir.path()).unwrap();

        assert_eq!(loaded.repo_url, state.repo_url);
        assert_eq!(loaded.config_dir, state.config_dir);
        assert_eq!(loaded.profile, state.profile);
        assert_eq!(loaded.phase, BootstrapPhase::Apply);
    }

    #[test]
    fn bootstrap_state_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_bootstrap_state(dir.path()).is_none());
    }

    #[test]
    fn clear_bootstrap_state_removes_file() {
        let dir = tempfile::tempdir().unwrap();

        let state = BootstrapState {
            repo_url: None,
            config_dir: ".".to_string(),
            profile: None,
            phase: BootstrapPhase::Clone,
        };

        save_bootstrap_state(dir.path(), &state).unwrap();
        assert!(dir.path().join(BOOTSTRAP_STATE_FILE).exists());

        clear_bootstrap_state(dir.path());
        assert!(!dir.path().join(BOOTSTRAP_STATE_FILE).exists());
    }

    #[test]
    fn ensure_config_file_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        ensure_config_file(
            dir.path(),
            &config_path,
            "work",
            Some("https://github.com/test/init-cfg.git"),
        )
        .unwrap();

        assert!(config_path.exists());
        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("profile: work"));
        assert!(contents.contains("https://github.com/test/init-cfg.git"));
    }

    #[test]
    fn ensure_config_file_updates_existing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        std::fs::write(
            &config_path,
            r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#,
        )
        .unwrap();

        ensure_config_file(dir.path(), &config_path, "work", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile, "work");
    }

    #[test]
    fn ensure_config_file_no_update_if_same_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        let original = r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#;
        std::fs::write(&config_path, original).unwrap();

        ensure_config_file(dir.path(), &config_path, "default", None).unwrap();

        // Should not be rewritten
        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(contents, original);
    }

    #[test]
    fn count_packages_empty() {
        let spec = config::ProfileSpec::default();
        assert_eq!(count_packages(&spec), 0);
    }

    #[test]
    fn count_packages_with_various() {
        let spec = config::ProfileSpec {
            packages: Some(config::PackagesSpec {
                brew: Some(config::BrewSpec {
                    formulae: vec!["rg".into(), "fd".into()],
                    casks: vec!["firefox".into()],
                    ..Default::default()
                }),
                cargo: Some(config::CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(count_packages(&spec), 4);
    }

    #[test]
    fn init_local_returns_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let original_dir = std::env::current_dir().unwrap();

        // Pre-create cfgd.yaml so init_local takes the fast path (no prompts)
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd/v1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        std::env::set_current_dir(dir.path()).unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = init_local(&printer).unwrap();

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_some());
        let config_dir = result.unwrap();
        assert!(config_dir.join("cfgd.yaml").exists());
    }

    #[test]
    fn bootstrap_profile_select_single_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();

        std::fs::write(
            profiles_dir.join("default.yaml"),
            r#"apiVersion: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
"#,
        )
        .unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = bootstrap_profile_select(dir.path(), &printer).unwrap();
        assert_eq!(result, Some("default".to_string()));
    }

    #[test]
    fn bootstrap_phase_display_names() {
        assert_eq!(BootstrapPhase::Clone.display_name(), "Clone repository");
        assert_eq!(BootstrapPhase::Apply.display_name(), "Apply configuration");
        assert_eq!(BootstrapPhase::Complete.display_name(), "Complete");
    }

    #[test]
    fn profile_switch_via_config_update() {
        let dir = create_test_config_dir();

        // Create cfgd.yaml
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#,
        )
        .unwrap();

        // Simulate what cmd_profile_switch does: update profile in cfgd.yaml
        ensure_config_file(dir.path(), &config_path, "work", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile, "work");
    }

    #[test]
    fn cli_init_has_source_flag() {
        // Verify the --source flag is accepted (Phase 9 prep)
        use clap::CommandFactory;
        let cmd = Cli::command();
        let init_cmd = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "init")
            .unwrap();
        let source_arg = init_cmd.get_arguments().find(|a| a.get_id() == "source");
        assert!(source_arg.is_some(), "--source flag should be reserved");
        assert!(
            source_arg.unwrap().is_hide_set(),
            "--source should be hidden"
        );
    }

    #[test]
    fn cli_has_source_subcommand() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let source_cmd = cmd.get_subcommands().find(|c| c.get_name() == "source");
        assert!(source_cmd.is_some(), "source subcommand should exist");

        let source_cmd = source_cmd.unwrap();
        let subcommands: Vec<&str> = source_cmd.get_subcommands().map(|c| c.get_name()).collect();
        assert!(subcommands.contains(&"add"));
        assert!(subcommands.contains(&"list"));
        assert!(subcommands.contains(&"show"));
        assert!(subcommands.contains(&"remove"));
        assert!(subcommands.contains(&"update"));
        assert!(subcommands.contains(&"override"));
        assert!(subcommands.contains(&"priority"));
        assert!(subcommands.contains(&"replace"));
    }

    #[test]
    fn infer_source_name_from_ssh_url() {
        assert_eq!(
            super::infer_source_name("git@github.com:acme-corp/dev-config.git"),
            "acme-corp-dev-config"
        );
    }

    #[test]
    fn infer_source_name_from_https_url() {
        assert_eq!(
            super::infer_source_name("https://github.com/acme/config.git"),
            "config"
        );
    }

    #[test]
    fn count_policy_items_empty() {
        let items = cfgd_core::config::PolicyItems::default();
        assert_eq!(super::count_policy_items(&items), 0);
    }

    #[test]
    fn add_and_remove_source_in_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            r#"apiVersion: cfgd/v1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#,
        )
        .unwrap();

        let source = cfgd_core::sources::SourceManager::build_source_spec(
            "acme",
            "git@github.com:acme/config.git",
            Some("backend"),
        );
        super::add_source_to_config(&config_path, &source).unwrap();

        let cfg = cfgd_core::config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.sources.len(), 1);
        assert_eq!(cfg.spec.sources[0].name, "acme");

        super::remove_source_from_config(&config_path, "acme").unwrap();
        let cfg = cfgd_core::config::load_config(&config_path).unwrap();
        assert!(cfg.spec.sources.is_empty());
    }

    #[test]
    fn set_nested_yaml_value_creates_path() {
        let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        super::set_nested_yaml_value(
            &mut root,
            "variables.EDITOR",
            &serde_yaml::Value::String("nvim".into()),
        )
        .unwrap();

        let editor = root
            .get("variables")
            .and_then(|v| v.get("EDITOR"))
            .and_then(|v| v.as_str());
        assert_eq!(editor, Some("nvim"));
    }

    #[test]
    fn pattern_matches_exact() {
        assert!(super::pattern_matches("packages.brew", "packages.brew"));
    }

    #[test]
    fn pattern_matches_prefix() {
        assert!(super::pattern_matches("packages", "packages.brew.ripgrep"));
        assert!(super::pattern_matches(
            "packages.brew",
            "packages.brew.ripgrep"
        ));
    }

    #[test]
    fn pattern_no_partial_match() {
        // "packages.br" should NOT match "packages.brew"
        assert!(!super::pattern_matches("packages.br", "packages.brew"));
    }

    #[test]
    fn pattern_matches_file_colon_paths() {
        assert!(super::pattern_matches("files", "files:/etc/foo"));
    }

    #[test]
    fn filter_plan_skip_entire_phase() {
        use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

        let mut plan = Plan {
            phases: vec![
                Phase {
                    name: PhaseName::Packages,
                    actions: vec![Action::Package(PackageAction::Install {
                        manager: "brew".into(),
                        packages: vec!["ripgrep".into(), "fd".into()],
                        origin: "local".into(),
                    })],
                },
                Phase {
                    name: PhaseName::Files,
                    actions: vec![Action::File(FileAction::Create {
                        source: "/tmp/a".into(),
                        target: "/tmp/b".into(),
                        origin: "local".into(),
                    })],
                },
            ],
        };

        super::filter_plan(&mut plan, &["packages".into()], &[]);
        assert!(plan.phases[0].actions.is_empty());
        assert_eq!(plan.phases[1].actions.len(), 1);
    }

    #[test]
    fn filter_plan_skip_single_package() {
        use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

        let mut plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["ripgrep".into(), "fd".into(), "bat".into()],
                    origin: "local".into(),
                })],
            }],
        };

        super::filter_plan(&mut plan, &["packages.brew.fd".into()], &[]);

        // Should keep ripgrep and bat, skip fd
        match &plan.phases[0].actions[0] {
            Action::Package(PackageAction::Install { packages, .. }) => {
                assert_eq!(packages, &["ripgrep".to_string(), "bat".to_string()]);
            }
            _ => panic!("expected Install action"),
        }
    }

    #[test]
    fn filter_plan_only_specific_manager() {
        use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

        let mut plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![
                    Action::Package(PackageAction::Install {
                        manager: "brew".into(),
                        packages: vec!["ripgrep".into()],
                        origin: "local".into(),
                    }),
                    Action::Package(PackageAction::Install {
                        manager: "cargo".into(),
                        packages: vec!["bat".into()],
                        origin: "local".into(),
                    }),
                ],
            }],
        };

        super::filter_plan(&mut plan, &[], &["packages.brew".into()]);

        // Only brew should remain
        assert_eq!(plan.phases[0].actions.len(), 1);
        match &plan.phases[0].actions[0] {
            Action::Package(PackageAction::Install { manager, .. }) => {
                assert_eq!(manager, "brew");
            }
            _ => panic!("expected Install action"),
        }
    }

    #[test]
    fn filter_plan_only_specific_package() {
        use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

        let mut plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["ripgrep".into(), "fd".into(), "bat".into()],
                    origin: "local".into(),
                })],
            }],
        };

        super::filter_plan(&mut plan, &[], &["packages.brew.ripgrep".into()]);

        match &plan.phases[0].actions[0] {
            Action::Package(PackageAction::Install { packages, .. }) => {
                assert_eq!(packages, &["ripgrep".to_string()]);
            }
            _ => panic!("expected Install action"),
        }
    }
}
