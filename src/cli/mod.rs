use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::config::{self, CfgdConfig, ResolvedProfile};
use crate::files::CfgdFileManager;
use crate::output::Printer;
use crate::packages;
use crate::providers::{FileAction, ProviderRegistry};
use crate::reconciler::{self, PhaseName, Reconciler};
use crate::secrets;
use crate::state::StateStore;

const BOOTSTRAP_STATE_FILE: &str = ".cfgd-bootstrap-state";

#[derive(Parser)]
#[command(
    name = "cfgd",
    version,
    about = "Declarative, GitOps-style machine configuration"
)]
pub struct Cli {
    /// Path to config file
    #[arg(long, global = true, default_value = "cfgd.yaml")]
    pub config: PathBuf,

    /// Profile to use (overrides config file)
    #[arg(long, global = true)]
    pub profile: Option<String>,

    /// Verbose output
    #[arg(long, short, global = true)]
    pub verbose: bool,

    /// Suppress all non-error output
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
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
    },

    /// Show the execution plan
    Plan,

    /// Apply the configuration
    Apply {
        /// Apply only a specific phase
        #[arg(long)]
        phase: Option<String>,

        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
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

/// Execute the given CLI command. Returns Ok(()) on success.
pub fn execute(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    match &cli.command {
        Command::Plan => cmd_plan(cli, printer),
        Command::Apply { phase, yes } => cmd_apply(cli, printer, phase.as_deref(), *yes),
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
                printer.info("remove (file): not yet implemented");
                Ok(())
            }
        }
        Command::Init { from, source: _ } => cmd_init(printer, from.as_deref()),
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
    }
}

// --- Bootstrap State (for resumable init) ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BootstrapState {
    repo_url: Option<String>,
    config_dir: String,
    profile: Option<String>,
    phase: BootstrapPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        // Clone from remote
        init_from_remote(printer, url)?
    } else {
        // Initialize in current directory
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

        let available_managers = registry.available_package_managers();
        let pkg_actions = packages::plan_packages(&resolved.merged, &available_managers)?;

        let mut fm = CfgdFileManager::new(&config_dir, &resolved)?;
        let file_actions = fm.plan(&resolved.merged)?;

        let plan = reconciler.plan(&resolved, file_actions, pkg_actions)?;

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
        let registry = build_registry_with_config(Some(&cfg));
        let store = open_state_store()?;
        let reconciler = Reconciler::new(&registry, &store);

        let available_managers = registry.available_package_managers();
        let pkg_actions = packages::plan_packages(&resolved.merged, &available_managers)?;

        let mut fm = CfgdFileManager::new(&config_dir, &resolved)?;
        let file_actions = fm.plan(&resolved.merged)?;

        let plan = reconciler.plan(&resolved, file_actions, pkg_actions)?;

        let result = reconciler.apply(&plan, &resolved, &config_dir, printer, None)?;

        printer.newline();
        match result.status {
            crate::state::ApplyStatus::Success => {
                printer.success(&format!(
                    "Apply complete — {} action(s) succeeded",
                    result.succeeded()
                ));
            }
            crate::state::ApplyStatus::Partial => {
                printer.warning(&format!(
                    "Apply partially complete — {} succeeded, {} failed",
                    result.succeeded(),
                    result.failed()
                ));
                printer.info("Failed actions can be retried with 'cfgd apply'");
            }
            crate::state::ApplyStatus::Failed => {
                printer.error(&format!(
                    "Apply failed — {} action(s) failed",
                    result.failed()
                ));
                printer.info("Review errors above and run 'cfgd init' to retry");
                return Ok(());
            }
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
        let registry = build_registry();
        let store = open_state_store()?;

        let results = reconciler::verify(&resolved, &registry, &store, printer)?;

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
            match crate::daemon::install_service(&config_path, Some(&profile_name)) {
                Ok(()) => {
                    if cfg!(target_os = "macos") {
                        printer.success("Installed launchd service: com.cfgd.daemon");
                        printer.info("Load with: launchctl load ~/Library/LaunchAgents/com.cfgd.daemon.plist");
                    } else {
                        printer.success("Installed systemd user service: cfgd.service");
                        printer.info("Enable with: systemctl --user enable --now cfgd.service");
                    }
                }
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

            match crate::daemon::git_pull_sync(&target_dir) {
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

    match git2::Repository::clone(url, &target_dir) {
        Ok(_) => {
            printer.success(&format!("Cloned to {}", target_dir.display()));
        }
        Err(e) => {
            // Fall back to git CLI for SSH URLs (git2 SSH support can be limited)
            printer.info("Trying git CLI...");
            let status = std::process::Command::new("git")
                .args(["clone", url, &target_dir.display().to_string()])
                .status();

            match status {
                Ok(s) if s.success() => {
                    printer.success(&format!("Cloned to {}", target_dir.display()));
                }
                Ok(_) => {
                    printer.error(&format!("Failed to clone {}: {}", url, e));
                    return Ok(None);
                }
                Err(clone_err) => {
                    printer.error(&format!(
                        "Failed to clone {}: {} (git: {})",
                        url, e, clone_err
                    ));
                    return Ok(None);
                }
            }
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

    // Create a minimal config structure
    printer.info("Initializing new cfgd configuration...");

    let profiles_dir = config_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir)?;

    // Create a default profile
    let default_profile = r#"api-version: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
  packages: {}
"#;
    let profile_path = profiles_dir.join("default.yaml");
    if !profile_path.exists() {
        std::fs::write(&profile_path, default_profile)?;
        printer.success("Created profiles/default.yaml");
    }

    // Create cfgd.yaml
    let config_content = format!(
        r#"api-version: cfgd/v1
kind: Config
metadata:
  name: {}
spec:
  profile: default
"#,
        config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("my-config")
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

    Ok(Some(config_dir))
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
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                profiles.push(stem.to_string());
            }
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
            count += apt.install.len();
        }
        count += pkgs.cargo.len();
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
        r#"api-version: cfgd/v1
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

fn build_registry_with_config(cfg: Option<&CfgdConfig>) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.package_managers = packages::all_package_managers();

    // Register system configurators based on OS
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

    // Register secret backend and providers
    let (backend_name, age_key_path) = if let Some(cfg) = cfg {
        if let Some(ref secrets_cfg) = cfg.spec.secrets {
            let name = secrets_cfg.backend.as_str();
            let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
            (name.to_string(), key)
        } else {
            ("sops".to_string(), None)
        }
    } else {
        ("sops".to_string(), None)
    };

    registry.secret_backend = Some(secrets::build_secret_backend(&backend_name, age_key_path));
    registry.secret_providers = secrets::build_secret_providers();

    registry
}

fn open_state_store() -> anyhow::Result<StateStore> {
    Ok(StateStore::open_default()?)
}

fn cmd_plan(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Plan");

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let registry = build_registry_with_config(Some(&cfg));
    let state = open_state_store()?;
    let reconciler = Reconciler::new(&registry, &state);

    // Generate component plans
    let available_managers = registry.available_package_managers();
    let pkg_actions = packages::plan_packages(&resolved.merged, &available_managers)?;

    let mut fm = CfgdFileManager::new(&config_dir, &resolved)?;
    let file_actions = fm.plan(&resolved.merged)?;

    let plan = reconciler.plan(&resolved, file_actions, pkg_actions)?;

    printer.newline();

    for phase in &plan.phases {
        let items = reconciler::format_plan_items(phase);
        printer.plan_phase(phase.name.display_name(), &items);
    }

    // Show diffs for file updates
    for phase in &plan.phases {
        if phase.name != PhaseName::Files {
            continue;
        }
        for action in &phase.actions {
            if let reconciler::Action::File(FileAction::Update { source, target, .. }) = action {
                if let Ok(target_content) = std::fs::read_to_string(target) {
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

fn cmd_apply(cli: &Cli, printer: &Printer, phase: Option<&str>, yes: bool) -> anyhow::Result<()> {
    printer.header("Apply");

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let registry = build_registry_with_config(Some(&cfg));
    let state = open_state_store()?;
    let reconciler = Reconciler::new(&registry, &state);

    // Validate phase name if provided
    let phase_filter = if let Some(p) = phase {
        match PhaseName::from_str(p) {
            Some(pn) => Some(pn),
            None => {
                printer.error(&format!(
                    "Unknown phase '{}'. Valid phases: system, packages, files, secrets, scripts",
                    p
                ));
                return Ok(());
            }
        }
    } else {
        None
    };

    // Generate the plan
    let available_managers = registry.available_package_managers();
    let pkg_actions = packages::plan_packages(&resolved.merged, &available_managers)?;

    let mut fm = CfgdFileManager::new(&config_dir, &resolved)?;
    let file_actions = fm.plan(&resolved.merged)?;

    let plan = reconciler.plan(&resolved, file_actions, pkg_actions)?;

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
        if let Some(ref pf) = phase_filter {
            if &phase_item.name != pf {
                continue;
            }
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
        &resolved,
        &config_dir,
        printer,
        phase_filter.as_ref(),
    )?;

    printer.newline();
    match result.status {
        crate::state::ApplyStatus::Success => {
            printer.success(&format!(
                "Apply complete — {} action(s) succeeded",
                result.succeeded()
            ));
        }
        crate::state::ApplyStatus::Partial => {
            printer.warning(&format!(
                "Apply partially complete — {} succeeded, {} failed",
                result.succeeded(),
                result.failed()
            ));
        }
        crate::state::ApplyStatus::Failed => {
            printer.error(&format!(
                "Apply failed — {} action(s) failed",
                result.failed()
            ));
        }
    }

    Ok(())
}

fn cmd_status(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Status");

    let (_cfg, _resolved) = load_config_and_profile(cli, printer)?;
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
                crate::state::ApplyStatus::Success => "success",
                crate::state::ApplyStatus::Partial => "partial",
                crate::state::ApplyStatus::Failed => "failed",
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
            printer.warning(&format!(
                "{} {} — want: {}, have: {}",
                event.resource_type,
                event.resource_id,
                event.expected.as_deref().unwrap_or("?"),
                event.actual.as_deref().unwrap_or("?"),
            ));
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
                        crate::state::ApplyStatus::Success => "success".to_string(),
                        crate::state::ApplyStatus::Partial => "partial".to_string(),
                        crate::state::ApplyStatus::Failed => "failed".to_string(),
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

    let (_cfg, resolved) = load_config_and_profile(cli, printer)?;
    let registry = build_registry();
    let state = open_state_store()?;

    printer.newline();

    let results = reconciler::verify(&resolved, &registry, &state, printer)?;

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

    let (_cfg, resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    printer.newline();
    let mut fm = CfgdFileManager::new(&config_dir, &resolved)?;
    fm.diff(&resolved.merged, printer)?;

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
    if let Some(ref apt) = pkgs.apt {
        if !apt.install.is_empty() {
            printer.key_value("apt", &apt.install.join(", "));
            has_packages = true;
        }
    }
    if !pkgs.cargo.is_empty() {
        printer.key_value("cargo", &pkgs.cargo.join(", "));
        has_packages = true;
    }
    if let Some(ref npm) = pkgs.npm {
        if !npm.global.is_empty() {
            printer.key_value("npm", &npm.global.join(", "));
            has_packages = true;
        }
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
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                profiles.push(stem.to_string());
            }
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
                if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        available.push(stem.to_string());
                    }
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
    let printer = std::sync::Arc::new(crate::output::Printer::new(if cli.quiet {
        crate::output::Verbosity::Quiet
    } else if cli.verbose {
        crate::output::Verbosity::Verbose
    } else {
        crate::output::Verbosity::Normal
    }));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async { crate::daemon::run_daemon(config_path, profile_override, printer).await })?;

    Ok(())
}

fn cmd_daemon_status(printer: &Printer) -> anyhow::Result<()> {
    printer.header("Daemon Status");
    printer.newline();

    match crate::daemon::query_daemon_status()? {
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

    crate::daemon::install_service(&cli.config, cli.profile.as_deref())?;

    if cfg!(target_os = "macos") {
        printer.success("Installed launchd service: com.cfgd.daemon");
        printer.info("Load with: launchctl load ~/Library/LaunchAgents/com.cfgd.daemon.plist");
    } else {
        printer.success("Installed systemd user service: cfgd.service");
        printer.info("Enable with: systemctl --user enable --now cfgd.service");
    }

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

    crate::daemon::uninstall_service()?;
    printer.success("Daemon service removed");

    Ok(())
}

fn cmd_sync(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Sync");

    let (_cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    printer.newline();
    printer.info("Syncing with remote...");

    // Pull
    match crate::daemon::git_pull_sync(&config_dir) {
        Ok(true) => printer.success("Pulled new changes from remote"),
        Ok(false) => printer.success("Already up to date"),
        Err(e) => printer.warning(&format!("Pull failed: {}", e)),
    }

    Ok(())
}

fn cmd_pull(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Pull");

    let (_cfg, _resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    printer.newline();

    match crate::daemon::git_pull_sync(&config_dir) {
        Ok(true) => printer.success("Pulled new changes from remote"),
        Ok(false) => printer.success("Already up to date"),
        Err(e) => printer.warning(&format!("Pull failed: {}", e)),
    }

    Ok(())
}

fn which(command: &str) -> bool {
    std::process::Command::new("which")
        .arg(command)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
            r#"api-version: cfgd/v1
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
            r#"api-version: cfgd/v1
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
            config_dir: "/home/user/.dotfiles".to_string(),
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
            r#"api-version: cfgd/v1
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

        let original = r#"api-version: cfgd/v1
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
                cargo: vec!["bat".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(count_packages(&spec), 4);
    }

    #[test]
    fn init_local_creates_structure() {
        let dir = tempfile::tempdir().unwrap();
        let original_dir = std::env::current_dir().unwrap();

        std::env::set_current_dir(dir.path()).unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = init_local(&printer).unwrap();

        std::env::set_current_dir(&original_dir).unwrap();

        assert!(result.is_some());
        let config_dir = result.unwrap();
        assert!(config_dir.join("cfgd.yaml").exists());
        assert!(config_dir.join("profiles").exists());
        assert!(config_dir.join("profiles").join("default.yaml").exists());
    }

    #[test]
    fn bootstrap_profile_select_single_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();

        std::fs::write(
            profiles_dir.join("default.yaml"),
            r#"api-version: cfgd/v1
kind: Profile
metadata:
  name: default
spec:
  variables: {}
"#,
        )
        .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
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
            r#"api-version: cfgd/v1
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
}
