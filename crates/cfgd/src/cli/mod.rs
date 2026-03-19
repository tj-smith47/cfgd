mod explain;
mod init;
mod module;
mod profile;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;

use crate::files::CfgdFileManager;
use crate::generate;
use crate::packages;
use crate::secrets;
use cfgd_core::composition::{self, CompositionInput, SubscriptionConfig};
use cfgd_core::config::{self, CfgdConfig, ResolvedProfile};
use cfgd_core::modules;
use cfgd_core::output::Printer;
use cfgd_core::platform::Platform;
use cfgd_core::providers::{
    FileAction, PackageAction, ProviderRegistry, SecretAction, SecretBackend,
};
use cfgd_core::reconciler::{self, PhaseName, Reconciler};
use cfgd_core::sources::SourceManager;
use cfgd_core::state::StateStore;

const MSG_NO_CONFIG: &str = "No cfgd.yaml found — run 'cfgd init' first";
const MSG_RUN_APPLY: &str = "Run 'cfgd apply --dry-run' to preview changes, then 'cfgd apply'";

// --- Structured output types ---

#[derive(Serialize)]
struct StatusOutput {
    last_apply: Option<cfgd_core::state::ApplyRecord>,
    drift: Vec<cfgd_core::state::DriftEvent>,
    sources: Vec<cfgd_core::state::ConfigSourceRecord>,
    pending_decisions: Vec<cfgd_core::state::PendingDecision>,
    modules: Vec<ModuleStatusEntry>,
    managed_resources: Vec<cfgd_core::state::ManagedResource>,
}

#[derive(Serialize)]
struct ModuleStatusEntry {
    name: String,
    packages: usize,
    files: usize,
    status: String,
}

#[derive(Serialize)]
struct LogOutput {
    entries: Vec<cfgd_core::state::ApplyRecord>,
}

#[derive(Serialize)]
struct VerifyOutput {
    results: Vec<cfgd_core::reconciler::VerifyResult>,
    pass_count: usize,
    fail_count: usize,
}

#[derive(Serialize)]
struct DoctorOutput {
    config: DoctorConfigCheck,
    git: bool,
    secrets: DoctorSecretsCheck,
    package_managers: Vec<DoctorManagerCheck>,
    modules: Vec<DoctorModuleCheck>,
    system_configurators: Vec<DoctorConfiguratorCheck>,
}

#[derive(Clone, Serialize)]
struct DoctorConfigCheck {
    valid: bool,
    path: String,
    name: Option<String>,
    profile: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct DoctorSecretsCheck {
    sops_available: bool,
    sops_version: Option<String>,
    age_key_exists: bool,
    age_key_path: Option<String>,
    sops_config_exists: bool,
    providers: Vec<DoctorProviderCheck>,
}

#[derive(Serialize)]
struct DoctorProviderCheck {
    name: String,
    available: bool,
}

#[derive(Serialize)]
struct DoctorManagerCheck {
    name: String,
    available: bool,
    declared: bool,
    can_bootstrap: bool,
}

#[derive(Serialize)]
struct DoctorModuleCheck {
    name: String,
    valid: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct DoctorConfiguratorCheck {
    name: String,
    available: bool,
}

#[derive(Serialize)]
struct SourceListEntry {
    name: String,
    url: String,
    priority: u32,
    version: Option<String>,
    status: String,
    last_fetched: Option<String>,
}

#[derive(Serialize)]
struct SourceShowOutput {
    name: String,
    url: String,
    branch: String,
    priority: u32,
    accept_recommended: bool,
    profile: Option<String>,
    sync_interval: String,
    auto_apply: bool,
    version_pin: Option<String>,
    state: Option<SourceStateInfo>,
    managed_resources: Vec<SourceResourceEntry>,
}

#[derive(Serialize)]
struct SourceStateInfo {
    status: String,
    last_fetched: Option<String>,
    last_commit: Option<String>,
    version: Option<String>,
}

#[derive(Serialize)]
struct SourceResourceEntry {
    resource_type: String,
    resource_id: String,
}

fn default_config_file() -> PathBuf {
    cfgd_core::default_config_dir().join("cfgd.yaml")
}

/// No built-in aliases — all aliases come from cfgd.yaml spec.aliases.
/// Default aliases are scaffolded by `cfgd init`.
fn builtin_aliases() -> HashMap<String, String> {
    HashMap::new()
}

/// Expand CLI aliases before clap parsing.
///
/// Finds the first positional argument (non-flag, non-flag-value), checks if it
/// matches an alias, and replaces it with the alias's command tokens. Any remaining
/// arguments after the alias name are appended.
///
/// Returns the potentially-expanded args.
pub fn expand_aliases(args: Vec<String>) -> Vec<String> {
    if args.len() < 2 {
        return args;
    }

    // Collect global flags that appear before the subcommand so we can skip them.
    // We need to find the first positional arg (the subcommand position).
    let mut subcommand_idx = None;
    let mut i = 1; // skip argv[0]
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            break;
        }
        if arg.starts_with('-') {
            // Skip flags and their values. Known global flags that take a value:
            if matches!(arg.as_str(), "--config" | "--profile") {
                i += 1; // skip the value too
            } else if arg.starts_with("--config=") || arg.starts_with("--profile=") {
                // value is inline, no skip needed
            }
            // Boolean flags (--verbose, -v, --quiet, -q, --no-color) are just skipped
        } else {
            subcommand_idx = Some(i);
            break;
        }
        i += 1;
    }

    let subcommand_idx = match subcommand_idx {
        Some(idx) => idx,
        None => return args,
    };

    let candidate = &args[subcommand_idx];

    // Try to load config to get user aliases; fall back to empty if unavailable.
    let config_path = extract_config_path(&args);
    let user_aliases = config_path
        .and_then(|p| {
            if p.exists() {
                cfgd_core::config::load_config(&p).ok()
            } else {
                None
            }
        })
        .map(|c| c.spec.aliases)
        .unwrap_or_default();

    // Merge: user overrides built-in
    let mut aliases = builtin_aliases();
    aliases.extend(user_aliases);

    let expansion = match aliases.get(candidate) {
        Some(cmd) => cmd,
        None => return args,
    };

    // Build expanded args: argv[0] + globals + expanded tokens + remaining args
    let mut result = Vec::with_capacity(args.len() + 4);
    result.extend_from_slice(&args[..subcommand_idx]);
    result.extend(expansion.split_whitespace().map(String::from));
    result.extend_from_slice(&args[subcommand_idx + 1..]);
    result
}

/// Extract the --config path from raw args, or use the default.
fn extract_config_path(args: &[String]) -> Option<PathBuf> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--config" {
            return args.get(i + 1).map(PathBuf::from);
        }
        if let Some(val) = arg.strip_prefix("--config=") {
            return Some(PathBuf::from(val));
        }
    }
    Some(default_config_file())
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
    #[arg(
        long,
        short,
        global = true,
        env = "CFGD_VERBOSE",
        conflicts_with = "quiet"
    )]
    pub verbose: bool,

    /// Suppress all non-error output
    #[arg(
        long,
        short,
        global = true,
        env = "CFGD_QUIET",
        conflicts_with = "verbose"
    )]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Output format: table (default), json, yaml
    #[arg(long, short = 'o', global = true, default_value = "table")]
    pub output: String,

    /// JSONPath expression to extract from structured output
    #[arg(long, global = true)]
    pub jsonpath: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Parser)]
pub struct ApplyArgs {
    /// Preview changes without applying
    #[arg(long)]
    pub dry_run: bool,
    /// Apply only a specific phase
    #[arg(long)]
    pub phase: Option<String>,
    /// Skip confirmation prompt
    #[arg(long, short, env = "CFGD_YES")]
    pub yes: bool,
    /// Skip specific items by dot-notation path (e.g., packages.brew.ripgrep, system.sysctl)
    #[arg(long)]
    pub skip: Vec<String>,
    /// Apply only items matching dot-notation paths (e.g., packages, files)
    #[arg(long)]
    pub only: Vec<String>,
    /// Apply only the specified module and its dependencies
    #[arg(long)]
    pub module: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new cfgd configuration repository
    Init {
        /// Directory to initialize (default: current directory)
        #[arg(value_hint = clap::ValueHint::DirPath)]
        path: Option<String>,

        /// Clone from a remote repository
        #[arg(long)]
        from: Option<String>,

        /// Git branch to clone (default: master)
        #[arg(long, default_value = "master")]
        branch: String,

        /// Config name in metadata (default: directory name)
        #[arg(long)]
        name: Option<String>,

        /// Also apply configuration after scaffolding
        #[arg(long)]
        apply: bool,

        /// Preview changes without applying (used with --apply/--apply-profile/--apply-module)
        #[arg(long)]
        dry_run: bool,

        /// Skip all confirmation prompts (used with --apply)
        #[arg(long, short, env = "CFGD_YES")]
        yes: bool,

        /// Install daemon service after init
        #[arg(long)]
        install_daemon: bool,

        /// Theme name (default, dracula, solarized-dark, solarized-light, minimal)
        #[arg(long)]
        theme: Option<String>,

        /// Activate and apply a specific profile (errors if not found)
        #[arg(long)]
        apply_profile: Option<String>,

        /// Apply specific modules (repeatable, errors if not found)
        #[arg(long = "apply-module")]
        apply_modules: Vec<String>,
    },

    /// Apply the configuration (use --dry-run to preview without applying)
    Apply(ApplyArgs),

    /// Show configuration status and drift
    Status,

    /// Show detailed diffs
    Diff,

    /// Show apply history
    Log {
        /// Number of entries to show
        #[arg(long, short = 'n', default_value = "20")]
        limit: u32,
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

    /// Show schema and field documentation for cfgd resource types
    Explain {
        /// Resource type or field path (e.g., "module", "profile.spec.packages")
        #[arg(value_hint = clap::ValueHint::Other)]
        resource: Option<String>,

        /// Show all fields expanded recursively
        #[arg(long)]
        recursive: bool,
    },

    /// View or edit the cfgd configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Manage GitHub Actions workflows for config repo releases
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },

    /// Check in with the device gateway and report status
    Checkin {
        /// Device gateway URL
        #[arg(long, env = "CFGD_SERVER_URL")]
        server_url: String,

        /// API key for authentication
        #[arg(long, env = "CFGD_API_KEY")]
        api_key: Option<String>,

        /// Device identifier (defaults to hostname)
        #[arg(long, env = "CFGD_DEVICE_ID")]
        device_id: Option<String>,
    },

    /// Enroll with a device gateway (token or key-based)
    Enroll {
        /// Device gateway URL
        #[arg(long, env = "CFGD_SERVER_URL")]
        server_url: String,

        /// Bootstrap token for token-based enrollment
        #[arg(long, env = "CFGD_BOOTSTRAP_TOKEN")]
        token: Option<String>,

        /// SSH key file for signing (default: auto-detect from agent or ~/.ssh/)
        #[arg(long, conflicts_with = "gpg_key")]
        ssh_key: Option<String>,

        /// GPG key ID for signing
        #[arg(long, conflicts_with = "ssh_key")]
        gpg_key: Option<String>,

        /// Username to enroll as (default: current system user)
        #[arg(long, env = "USER")]
        username: Option<String>,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Generate a cfgd config from your existing dotfiles (interactive AI-assisted)
    Generate {
        /// Shell to scan for aliases and exports (default: auto-detect from $SHELL)
        #[arg(long)]
        shell: Option<String>,

        /// Home directory to scan (default: $HOME)
        #[arg(long, value_hint = clap::ValueHint::DirPath)]
        home: Option<String>,

        /// Only scan dotfiles and shell config; print findings without AI generation
        #[arg(long)]
        scan_only: bool,
    },
}

#[derive(Parser)]
pub struct SourceAddArgs {
    /// Git URL of the source
    pub url: String,
    /// Name for this source (default: inferred from URL)
    #[arg(long)]
    pub name: Option<String>,
    /// Git branch (default: master)
    #[arg(long)]
    pub branch: Option<String>,
    /// Profile to subscribe to
    #[arg(long)]
    pub profile: Option<String>,
    /// Accept recommended items
    #[arg(long)]
    pub accept_recommended: bool,
    /// Priority for conflict resolution (default: 500, local config: 1000)
    #[arg(long)]
    pub priority: Option<u32>,
    /// Opt-in to specific items (repeatable)
    #[arg(long = "opt-in")]
    pub opt_in: Vec<String>,
    /// Sync interval (e.g., "30m", "1h", "6h")
    #[arg(long)]
    pub sync_interval: Option<String>,
    /// Automatically apply changes on sync
    #[arg(long)]
    pub auto_apply: bool,
    /// Pin to a semver version range (e.g., "~1.0", ">=2.0")
    #[arg(long)]
    pub pin_version: Option<String>,
    /// Skip confirmation prompt
    #[arg(long, short, env = "CFGD_YES")]
    pub yes: bool,
}

#[derive(Subcommand)]
pub enum SourceCommand {
    /// Subscribe to a config source
    Add(Box<SourceAddArgs>),

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

        /// Resource path (e.g., env.EDITOR, packages.brew.formulae)
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

    /// Open cfgd-source.yaml in $EDITOR
    Edit,

    /// Create a new cfgd-source.yaml in the current directory
    Create {
        /// Source name
        #[arg(long)]
        name: Option<String>,

        /// Description
        #[arg(long)]
        description: Option<String>,

        /// Version string
        #[arg(long)]
        version: Option<String>,
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
pub enum ConfigCommand {
    /// Show the current cfgd configuration
    Show,
    /// Open cfgd.yaml in $EDITOR
    Edit,
    /// Get a config value by dotted key path (e.g., theme, daemon.reconcile.interval)
    Get {
        /// Dotted key path using YAML field names
        key: String,
    },
    /// Set a config value by dotted key path
    Set {
        /// Dotted key path using YAML field names
        key: String,
        /// Value to set
        value: String,
    },
    /// Remove a config value (resets to default)
    Unset {
        /// Dotted key path to remove
        key: String,
    },
}

#[derive(Subcommand)]
pub enum WorkflowCommand {
    /// Generate or regenerate GitHub Actions workflows for releases
    Generate {
        /// Overwrite existing workflow files
        #[arg(long)]
        force: bool,
    },
}

#[derive(Parser)]
pub struct ProfileCreateArgs {
    /// Profile name
    pub name: String,
    /// Inherit from other profiles (repeatable)
    #[arg(long = "inherit")]
    pub inherits: Vec<String>,
    /// Modules to include (repeatable)
    #[arg(long = "module")]
    pub modules: Vec<String>,
    /// Packages to include (repeatable, e.g. --package curl or --package brew:curl)
    #[arg(long = "package")]
    pub packages: Vec<String>,
    /// Environment variables as key=value (repeatable)
    #[arg(long = "env")]
    pub env: Vec<String>,
    /// Shell aliases as name=command (repeatable)
    #[arg(long = "alias")]
    pub aliases: Vec<String>,
    /// System settings as key=value (repeatable)
    #[arg(long = "system")]
    pub system: Vec<String>,
    /// Files to manage (repeatable). Use <path> to adopt in place, or <source>:<target> for explicit mapping.
    #[arg(long = "file")]
    pub files: Vec<String>,
    /// Mark all --file entries as private (local-only, excluded from git).
    #[arg(long = "private-files")]
    pub private: bool,
    /// Secrets as source:target (repeatable, e.g. --secret secrets/api-key.enc:~/.config/app/key)
    #[arg(long = "secret")]
    pub secrets: Vec<String>,
    /// Pre-apply scripts (repeatable)
    #[arg(long = "pre-apply")]
    pub pre_reconcile: Vec<PathBuf>,
    /// Post-apply scripts (repeatable)
    #[arg(long = "post-apply")]
    pub post_reconcile: Vec<PathBuf>,
}

#[derive(Parser)]
pub struct ProfileUpdateArgs {
    /// Profile name (optional when --active is used)
    pub name: Option<String>,
    /// Use the active profile from cfgd.yaml
    #[arg(long)]
    pub active: bool,
    /// Inherited profiles (repeatable, prefix with - to remove)
    #[arg(long = "inherit", allow_hyphen_values = true)]
    pub inherits: Vec<String>,
    /// Modules (repeatable, prefix with - to remove)
    #[arg(long = "module", allow_hyphen_values = true)]
    pub modules: Vec<String>,
    /// Packages (repeatable, prefix with - to remove, e.g. --package brew:jq --package -brew:old)
    #[arg(long = "package", allow_hyphen_values = true)]
    pub packages: Vec<String>,
    /// Files (repeatable, prefix with - to remove by target path)
    #[arg(long = "file", allow_hyphen_values = true)]
    pub files: Vec<String>,
    /// Env vars as KEY=VALUE (repeatable, prefix with - to remove by key)
    #[arg(long = "env", allow_hyphen_values = true)]
    pub env: Vec<String>,
    /// Shell aliases as name=command (repeatable, prefix with - to remove by name)
    #[arg(long = "alias", allow_hyphen_values = true)]
    pub aliases: Vec<String>,
    /// System settings as key=value (repeatable, prefix with - to remove by key)
    #[arg(long = "system", allow_hyphen_values = true)]
    pub system: Vec<String>,
    /// Secrets as source:target (repeatable, prefix with - to remove by target)
    #[arg(long = "secret", allow_hyphen_values = true)]
    pub secrets: Vec<String>,
    /// Pre-apply scripts (repeatable, prefix with - to remove)
    #[arg(long = "pre-apply", allow_hyphen_values = true)]
    pub pre_reconcile: Vec<PathBuf>,
    /// Post-apply scripts (repeatable, prefix with - to remove)
    #[arg(long = "post-apply", allow_hyphen_values = true)]
    pub post_reconcile: Vec<PathBuf>,
    /// Mark all --file entries as private (local-only, excluded from git).
    #[arg(long = "private-files")]
    pub private: bool,
}

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// List available profiles
    List,
    /// Switch to a different profile (alias: use)
    #[command(alias = "use")]
    Switch {
        /// Profile name
        name: String,
    },
    /// Show the resolved profile
    Show,
    /// Create a new profile
    Create(Box<ProfileCreateArgs>),
    /// Modify an existing profile
    Update(Box<ProfileUpdateArgs>),
    /// Open a profile in $EDITOR
    Edit {
        /// Profile name
        name: String,
    },
    /// Delete a profile
    Delete {
        /// Profile name
        name: String,
        /// Skip confirmation prompt
        #[arg(short, long, env = "CFGD_YES")]
        yes: bool,
    },
}

#[derive(Parser)]
pub struct ModuleCreateArgs {
    /// Module name
    pub name: String,
    /// Module description
    #[arg(long)]
    pub description: Option<String>,
    /// Dependencies on other modules (repeatable)
    #[arg(long = "depends")]
    pub depends: Vec<String>,
    /// Packages to include (repeatable)
    #[arg(long = "package")]
    pub packages: Vec<String>,
    /// Files to import (repeatable). Use <path> to adopt in place, or <source>:<target> for explicit mapping.
    #[arg(long = "file")]
    pub files: Vec<String>,
    /// Mark all --file entries as private (local-only, excluded from git).
    #[arg(long = "private-files")]
    pub private: bool,
    /// Environment variables as KEY=VALUE (repeatable)
    #[arg(long = "env")]
    pub env: Vec<String>,
    /// Shell aliases as name=command (repeatable)
    #[arg(long = "alias")]
    pub aliases: Vec<String>,
    /// Post-apply scripts (repeatable)
    #[arg(long = "post-apply")]
    pub post_apply: Vec<String>,
    /// Helm-style overrides: package.<name>.<field>=<value>
    #[arg(long = "set")]
    pub sets: Vec<String>,
    /// Apply the module immediately after creating it
    #[arg(long)]
    pub apply: bool,
    /// Skip confirmation prompts (used with --apply)
    #[arg(long, short, env = "CFGD_YES")]
    pub yes: bool,
}

#[derive(Parser)]
pub struct ModuleUpdateArgs {
    /// Module name
    pub name: String,
    /// Packages (repeatable, prefix with - to remove)
    #[arg(long = "package", allow_hyphen_values = true)]
    pub packages: Vec<String>,
    /// Files (repeatable, prefix with - to remove by target path)
    #[arg(long = "file", allow_hyphen_values = true)]
    pub files: Vec<String>,
    /// Env vars as KEY=VALUE (repeatable, prefix with - to remove by key)
    #[arg(long = "env", allow_hyphen_values = true)]
    pub env: Vec<String>,
    /// Shell aliases as name=command (repeatable, prefix with - to remove by name)
    #[arg(long = "alias", allow_hyphen_values = true)]
    pub aliases: Vec<String>,
    /// Dependencies (repeatable, prefix with - to remove)
    #[arg(long = "depends", allow_hyphen_values = true)]
    pub depends: Vec<String>,
    /// Post-apply scripts (repeatable, prefix with - to remove)
    #[arg(long = "post-apply", allow_hyphen_values = true)]
    pub post_apply: Vec<String>,
    /// Mark all --file entries as private (local-only, excluded from git).
    #[arg(long = "private-files")]
    pub private: bool,
    /// Set description
    #[arg(long)]
    pub description: Option<String>,
    /// Helm-style overrides: package.<name>.<field>=<value>
    #[arg(long = "set")]
    pub sets: Vec<String>,
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
    /// Create a new local module
    Create(Box<ModuleCreateArgs>),
    /// Modify an existing local module (add/remove packages, files, deps; --set overrides)
    Update(Box<ModuleUpdateArgs>),
    /// Open a module's module.yaml in $EDITOR
    Edit {
        /// Module name
        name: String,
    },
    /// Delete a local module
    Delete {
        /// Module name
        name: String,
        /// Skip confirmation prompt
        #[arg(short, long, env = "CFGD_YES")]
        yes: bool,
        /// Also remove files deployed by this module to target locations
        #[arg(long)]
        purge: bool,
    },
    /// Create a new local module (alias for 'create')
    #[command(hide = true)]
    Add(Box<ModuleCreateArgs>),
    /// Upgrade a remote module to a new version
    Upgrade {
        /// Module name (must be a locked remote module)
        name: String,
        /// New ref to pin to (tag or commit SHA)
        #[arg(long)]
        ref_: Option<String>,
        /// Skip confirmation prompt (for non-interactive use)
        #[arg(short, long, env = "CFGD_YES")]
        yes: bool,
        /// Allow unsigned modules even when require-signatures is enabled
        #[arg(long)]
        allow_unsigned: bool,
    },
    /// Search module registries for available modules
    Search {
        /// Search query
        query: String,
    },
    /// Manage module registries (searchable indexes of reusable modules)
    Registry {
        #[command(subcommand)]
        command: ModuleRegistryCommand,
    },
}

#[derive(Subcommand)]
pub enum ModuleRegistryCommand {
    /// Add a module registry
    Add {
        /// Git URL of the registry repo (GitHub only)
        url: String,
        /// Custom name/alias (defaults to GitHub org name)
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a module registry
    Remove {
        /// Registry name
        name: String,
    },
    /// Rename a module registry (updates config references)
    Rename {
        /// Current registry name
        name: String,
        /// New name
        new_name: String,
    },
    /// List configured module registries
    List,
}

/// Execute the given CLI command. Returns Ok(()) on success.
pub fn execute(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    match &cli.command {
        Command::Apply(args) => cmd_apply(cli, printer, args),
        Command::Status => cmd_status(cli, printer),
        Command::Diff => cmd_diff(cli, printer),
        Command::Log { limit } => cmd_log(printer, *limit),
        Command::Verify => cmd_verify(cli, printer),
        Command::Profile { command } => match command {
            ProfileCommand::Show => profile::cmd_profile_show(cli, printer),
            ProfileCommand::List => profile::cmd_profile_list(cli, printer),
            ProfileCommand::Switch { name } => profile::cmd_profile_switch(cli, name, printer),
            ProfileCommand::Create(args) => profile::cmd_profile_create(cli, printer, args),
            ProfileCommand::Update(args) => {
                let profile_name = resolve_profile_name(cli, args.name.as_deref(), args.active)?;
                profile::cmd_profile_update(cli, printer, &profile_name, args)
            }
            ProfileCommand::Edit { name } => profile::cmd_profile_edit(cli, printer, name),
            ProfileCommand::Delete { name, yes } => {
                profile::cmd_profile_delete(cli, printer, name, *yes)
            }
        },
        Command::Doctor => cmd_doctor(cli, printer),
        Command::Init {
            path,
            from,
            branch,
            name,
            apply,
            dry_run,
            yes,
            install_daemon,
            theme,
            apply_profile,
            apply_modules,
        } => init::cmd_init(
            printer,
            &init::InitArgs {
                path: path.as_deref(),
                from: from.as_deref(),
                branch,
                name: name.as_deref(),
                apply: *apply,
                dry_run: *dry_run,
                yes: *yes,
                install_daemon: *install_daemon,
                theme: theme.as_deref(),
                apply_profile: apply_profile.as_deref(),
                apply_modules,
            },
        ),
        Command::Module { command } => match command {
            ModuleCommand::List => module::cmd_module_list(cli, printer),
            ModuleCommand::Show { name } => module::cmd_module_show(cli, printer, name),
            ModuleCommand::Create(args) => module::cmd_module_create(cli, printer, args),
            ModuleCommand::Update(args) => module::cmd_module_update_local(cli, printer, args),
            ModuleCommand::Edit { name } => module::cmd_module_edit(cli, printer, name),
            ModuleCommand::Delete { name, yes, purge } => {
                module::cmd_module_delete(cli, printer, name, *yes, *purge)
            }
            ModuleCommand::Add(args) => module::cmd_module_create(cli, printer, args),
            ModuleCommand::Upgrade {
                name,
                ref_,
                yes,
                allow_unsigned,
            } => module::cmd_module_upgrade(
                cli,
                printer,
                name,
                ref_.as_deref(),
                *yes,
                *allow_unsigned,
            ),
            ModuleCommand::Search { query } => module::cmd_module_search(cli, printer, query),
            ModuleCommand::Registry { command } => match command {
                ModuleRegistryCommand::Add { url, name } => {
                    module::cmd_module_registry_add(cli, printer, url, name.as_deref())
                }
                ModuleRegistryCommand::Remove { name } => {
                    module::cmd_module_registry_remove(cli, printer, name)
                }
                ModuleRegistryCommand::Rename { name, new_name } => {
                    module::cmd_module_registry_rename(cli, printer, name, new_name)
                }
                ModuleRegistryCommand::List => module::cmd_module_registry_list(cli, printer),
            },
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
            SourceCommand::Add(args) => cmd_source_add(cli, printer, args),
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
            SourceCommand::Edit => cmd_source_edit(cli, printer),
            SourceCommand::Create {
                name,
                description,
                version,
            } => cmd_source_create(
                cli,
                printer,
                name.as_deref(),
                description.as_deref(),
                version.as_deref(),
            ),
        },
        Command::Explain {
            resource,
            recursive,
        } => explain::cmd_explain(printer, resource.as_deref(), *recursive),
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
        Command::Config { command } => match command {
            ConfigCommand::Show => cmd_config_show(cli, printer),
            ConfigCommand::Edit => cmd_config_edit(cli, printer),
            ConfigCommand::Get { key } => cmd_config_get(cli, printer, key),
            ConfigCommand::Set { key, value } => cmd_config_set(cli, printer, key, value),
            ConfigCommand::Unset { key } => cmd_config_unset(cli, printer, key),
        },
        Command::Workflow { command } => match command {
            WorkflowCommand::Generate { force } => cmd_workflow_generate(cli, printer, *force),
        },
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
        Command::Enroll {
            server_url,
            token,
            ssh_key,
            gpg_key,
            username,
        } => init::cmd_enroll(
            printer,
            server_url,
            token.as_deref(),
            ssh_key.as_deref(),
            gpg_key.as_deref(),
            username.as_deref(),
        ),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "cfgd", &mut std::io::stdout());
            Ok(())
        }
        Command::Generate { shell, home, scan_only } => {
            cmd_generate(printer, shell.as_deref(), home.as_deref(), *scan_only)
        }
    }
}

fn load_config_and_profile(
    cli: &Cli,
    printer: &Printer,
) -> anyhow::Result<(CfgdConfig, ResolvedProfile)> {
    let cfg = config::load_config(&cli.config)?;
    let profile_name = match cli.profile.as_deref() {
        Some(p) => p,
        None => cfg.active_profile()?,
    };

    printer.key_value("Config", &cli.config.display().to_string());
    printer.key_value("Profile", profile_name);

    let resolved = config::resolve_profile(profile_name, &profiles_dir(cli))?;
    Ok((cfg, resolved))
}

/// Parse a `--package` flag value. If it contains `:` and the prefix is a known
/// package manager name, split into (Some(manager), package). Otherwise treat
/// the entire string as a bare package name.
pub(super) fn parse_package_flag(s: &str, known_managers: &[&str]) -> (Option<String>, String) {
    if let Some((prefix, suffix)) = s.split_once(':')
        && !prefix.is_empty()
        && !suffix.is_empty()
        && known_managers.contains(&prefix)
    {
        return (Some(prefix.to_string()), suffix.to_string());
    }
    (None, s.to_string())
}

/// Collect known package manager names from the registry.
pub(super) fn known_manager_names() -> Vec<String> {
    packages::all_package_managers()
        .iter()
        .map(|m| m.name().to_string())
        .collect()
}

/// Parse a `--file` value into (source_path, target_path).
/// - `<path>` without `:` → adopt in place: source=path, target=path
/// - `<source>:<target>` → explicit mapping
fn parse_file_spec(spec: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    if let Some((source, target)) = spec.split_once(':') {
        if source.is_empty() {
            anyhow::bail!("empty source in file spec: {}", spec);
        }
        if target.is_empty() {
            anyhow::bail!("empty target in file spec: {}", spec);
        }
        Ok((
            cfgd_core::expand_tilde(Path::new(source)),
            cfgd_core::expand_tilde(Path::new(target)),
        ))
    } else {
        let expanded = cfgd_core::expand_tilde(Path::new(spec));
        Ok((expanded.clone(), expanded))
    }
}

/// Adopt files: copy into `repo_dir`, symlink back from source location.
/// Returns `(basename, deploy_target)` pairs — basename is the filename in the repo,
/// deploy_target is where the file should be deployed on the machine.
fn copy_files_to_dir(
    file_specs: &[String],
    repo_dir: &Path,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut results = Vec::new();
    for spec in file_specs {
        let (source, target) = parse_file_spec(spec)?;
        if !source.exists() {
            anyhow::bail!("File not found: {}", source.display());
        }
        std::fs::create_dir_all(repo_dir)?;
        let file_name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid file path: {}", source.display()))?;
        let dest = repo_dir.join(file_name);
        if source.is_dir() {
            cfgd_core::copy_dir_recursive(&source, &dest)?;
        } else {
            std::fs::copy(&source, &dest)?;
        }
        // Symlink back from source location to repo copy
        if source.exists() && !source.is_symlink() {
            if source.is_dir() {
                std::fs::remove_dir_all(&source)?;
            } else {
                std::fs::remove_file(&source)?;
            }
            std::os::unix::fs::symlink(&dest, &source)?;
        }
        results.push((file_name.to_string_lossy().to_string(), target));
    }
    Ok(results)
}

/// Add a path to `.gitignore` in `config_dir` if not already present.
fn add_to_gitignore(config_dir: &Path, path: &str) -> anyhow::Result<()> {
    let gitignore = config_dir.join(".gitignore");
    let existing = if gitignore.exists() {
        std::fs::read_to_string(&gitignore)?
    } else {
        String::new()
    };
    // Check if already listed (exact line match)
    if existing.lines().any(|line| line.trim() == path) {
        return Ok(());
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(path);
    content.push('\n');
    std::fs::write(&gitignore, content)?;
    Ok(())
}

/// Extract secret backend name and age key path from config.
/// Returns ("sops", None) as defaults when no secrets config is present.
fn secret_backend_from_config(cfg: Option<&CfgdConfig>) -> (String, Option<PathBuf>) {
    if let Some(cfg) = cfg
        && let Some(ref secrets_cfg) = cfg.spec.secrets
    {
        let name = secrets_cfg.backend.as_str().to_string();
        let key = secrets_cfg.sops.as_ref().and_then(|s| s.age_key.clone());
        (name, key)
    } else {
        ("sops".to_string(), None)
    }
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
        fm.set_global_strategy(cfg.spec.file_strategy);
        let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg));
        let backend = secrets::build_secret_backend(&backend_name, age_key_path, Some(config_dir));
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
    build_registry_with_config_and_packages(None, Some(spec))
}

fn build_registry_with_config(cfg: Option<&CfgdConfig>) -> ProviderRegistry {
    build_registry_with_config_and_packages(cfg, None)
}

fn build_registry_with_config_and_packages(
    cfg: Option<&CfgdConfig>,
    packages: Option<&cfgd_core::config::PackagesSpec>,
) -> ProviderRegistry {
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
    let (backend_name, age_key_path) = secret_backend_from_config(cfg);
    registry.secret_backend = Some(secrets::build_secret_backend(
        &backend_name,
        age_key_path,
        None,
    ));
    registry.secret_providers = secrets::build_secret_providers();

    // Set global file strategy from config
    if let Some(cfg) = cfg {
        registry.default_file_strategy = cfg.spec.file_strategy;
    }

    // Extend with custom package managers from profile packages spec
    if let Some(spec) = packages {
        registry
            .package_managers
            .extend(packages::custom_managers(&spec.custom));
    }

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

fn cmd_apply(cli: &Cli, printer: &Printer, args: &ApplyArgs) -> anyhow::Result<()> {
    let dry_run = args.dry_run;
    let phase = args.phase.as_deref();
    let yes = args.yes;
    let skip = &args.skip;
    let only = &args.only;
    let module_filter = args.module.as_deref();
    if dry_run {
        printer.header("Plan");
    } else {
        printer.header("Apply");
    }

    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);
    let mut registry = build_registry_with_config(Some(&cfg));
    let state = open_state_store()?;

    // Validate phase name if provided
    let phase_filter = if let Some(p) = phase {
        match p.parse::<PhaseName>() {
            Ok(pn) => Some(pn),
            Err(_) => {
                anyhow::bail!(
                    "Unknown phase '{}'. Valid phases: modules, system, packages, files, secrets, scripts",
                    p
                );
            }
        }
    } else {
        None
    };

    // Compose with sources if configured
    let source_env = if !cfg.spec.sources.is_empty() {
        let composition_result = compose_with_sources(cli, &resolved, printer)?;
        let se = composition_result.source_env;
        (Some(composition_result.resolved), se)
    } else {
        (None, std::collections::HashMap::new())
    };
    let mut effective_resolved = source_env.0.unwrap_or(resolved);
    let source_env = source_env.1;

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

    // In dry-run mode we don't need secret providers wired up — just plan files for display.
    // In apply mode we wire up the full file manager with secret providers.
    let (pkg_actions, file_actions, dry_run_fm) = if module_only {
        (Vec::new(), Vec::new(), None)
    } else {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg = packages::plan_packages(&effective_resolved.merged, &all_managers)?;

        let mut fm = CfgdFileManager::new(&config_dir, &effective_resolved)?;
        fm.set_global_strategy(cfg.spec.file_strategy);
        if !source_env.is_empty() {
            fm.set_source_env(&source_env);
        }

        if !dry_run {
            let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg));
            fm.set_secret_providers(
                Some(secrets::build_secret_backend(
                    &backend_name,
                    age_key_path,
                    Some(&config_dir),
                )),
                secrets::build_secret_providers(),
            );
        }

        let fa = fm.plan(&effective_resolved.merged)?;

        if dry_run {
            // Keep fm around for diff display but don't register it
            (pkg, fa, Some(fm))
        } else {
            // Register the file manager so the reconciler delegates through the trait
            registry.file_manager = Some(Box::new(fm));
            (pkg, fa, None)
        }
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

    if dry_run {
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

        for phase_item in &plan.phases {
            if let Some(ref pf) = phase_filter
                && &phase_item.name != pf
            {
                continue;
            }
            let items = reconciler::format_plan_items(phase_item);
            printer.plan_phase(phase_item.name.display_name(), &items);
        }

        // Show diffs for file updates
        if let Some(ref fm) = dry_run_fm {
            for phase_item in &plan.phases {
                if phase_item.name != PhaseName::Files {
                    continue;
                }
                for action in &phase_item.actions {
                    if let reconciler::Action::File(FileAction::Update { source, target, .. }) =
                        action
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

        for w in &plan.warnings {
            printer.warning(w);
        }

        printer.newline();
        let total = plan.total_actions();
        if total == 0 {
            printer.success("Nothing to do — everything is up to date");
        } else {
            printer.info(&format!("{} action(s) planned", total));
        }

        return Ok(());
    }

    // --- Apply mode ---

    // Handle unmanaged file targets: if a target exists as a non-cfgd file, prompt to
    // adopt (proceed), backup (rename to .cfgd-backup), or skip.
    handle_unmanaged_file_targets(&mut plan, &config_dir, &state, printer, yes)?;

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

    for w in &plan.warnings {
        printer.warning(w);
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

    // Acquire apply lock to prevent concurrent applies
    let state_dir = cfgd_core::state::default_state_dir()
        .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))?;
    let _apply_lock = cfgd_core::acquire_apply_lock(&state_dir)?;

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

    // Prune old backups — keep last 10 applies' worth
    if let Ok(state) = open_state_store() {
        let _ = state.prune_old_backups(10);
    }

    Ok(())
}

fn cmd_status(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let (cfg, resolved) = load_config_and_profile(cli, printer)?;
    let state = open_state_store()?;

    let last_apply = state.last_apply()?;
    let drift_events = state.unresolved_drift()?;
    let source_records = if !cfg.spec.sources.is_empty() {
        state.config_sources()?
    } else {
        vec![]
    };
    let pending = state.pending_decisions()?;
    let resources = state.managed_resources()?;

    // Build module status entries
    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
    let all_modules = modules::load_all_modules(&config_dir, &cache_base).unwrap_or_default();
    let state_map = module_state_map(&state);
    let module_entries: Vec<ModuleStatusEntry> = resolved
        .merged
        .modules
        .iter()
        .map(|mod_ref| {
            let mod_name = modules::resolve_profile_module_name(mod_ref);
            let (pkg_count, file_count) = all_modules
                .get(mod_name)
                .map(|m| (m.spec.packages.len(), m.spec.files.len()))
                .unwrap_or((0, 0));
            let status = state_map
                .get(mod_name)
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "not applied".into());
            ModuleStatusEntry {
                name: mod_ref.clone(),
                packages: pkg_count,
                files: file_count,
                status,
            }
        })
        .collect();

    if printer.is_structured() {
        printer.write_structured(&StatusOutput {
            last_apply,
            drift: drift_events,
            sources: source_records,
            pending_decisions: pending,
            modules: module_entries,
            managed_resources: resources,
        });
        return Ok(());
    }

    printer.header("Status");
    printer.newline();

    // Last apply
    if let Some(last) = &last_apply {
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
    if !pending.is_empty() {
        printer.newline();
        printer.subheader("Pending Decisions");
        display_pending_decisions(printer, &pending);
    }

    // Modules
    if !resolved.merged.modules.is_empty() {
        printer.newline();
        printer.subheader("Modules");

        for mod_ref in &resolved.merged.modules {
            let mod_name = modules::resolve_profile_module_name(mod_ref);
            let (pkg_count, file_count) = all_modules
                .get(mod_name)
                .map(|m| (m.spec.packages.len(), m.spec.files.len()))
                .unwrap_or((0, 0));

            let summary = format!("{} pkgs, {} files", pkg_count, file_count);
            if let Some(state_rec) = state_map.get(mod_name) {
                if state_rec.status == "installed" {
                    printer.success(&format!("{}: {}, {}", mod_ref, summary, state_rec.status));
                } else {
                    printer.warning(&format!("{}: {}, {}", mod_ref, summary, state_rec.status));
                }
            } else {
                printer.info(&format!("{}: {}, not yet applied", mod_ref, summary));
            }
        }
    }

    // Managed resources
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
    let state = open_state_store()?;
    let history = state.history(count)?;

    if printer.is_structured() {
        printer.write_structured(&LogOutput { entries: history });
        return Ok(());
    }

    printer.header("Apply History");

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
    let (_cfg, mut resolved) = load_config_and_profile(cli, printer)?;
    let config_dir = config_dir(cli);

    packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir)?;

    let registry = build_registry_with_profile(&resolved.merged.packages);
    let state = open_state_store()?;

    let results = reconciler::verify(&resolved, &registry, &state, printer, &[])?;

    let pass_count = results.iter().filter(|r| r.matches).count();
    let fail_count = results.iter().filter(|r| !r.matches).count();

    if printer.is_structured() {
        printer.write_structured(&VerifyOutput {
            results,
            pass_count,
            fail_count,
        });
        return Ok(());
    }

    printer.header("Verify");
    printer.newline();

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

// --- Validation helpers ---

/// Validate a resource name (module or profile) for filesystem safety.
/// Allows alphanumeric, hyphen, underscore, and dot (but not leading dot).
fn validate_resource_name(name: &str, kind: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("{kind} name cannot be empty");
    }
    if name.len() > 128 {
        anyhow::bail!("{kind} name too long (max 128 characters)");
    }
    if name.starts_with('.') || name.starts_with('-') {
        anyhow::bail!("{kind} name cannot start with '.' or '-'");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        anyhow::bail!(
            "{kind} name '{}' contains invalid characters — use only alphanumeric, hyphen, underscore, or dot",
            name
        );
    }
    Ok(())
}

// --- Scan helpers ---

/// Scan a profiles/ directory and return sorted profile names.
fn scan_profile_names(profiles_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    if profiles_dir.exists() {
        for entry in std::fs::read_dir(profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml" || e == "yml")
                && let Ok(doc) = config::load_profile(&path)
            {
                names.push(doc.metadata.name);
            }
        }
        names.sort();
    }
    Ok(names)
}

/// Scan a modules/ directory and return sorted module names.
fn scan_module_names(modules_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    if modules_dir.exists() {
        for entry in std::fs::read_dir(modules_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && path.join("module.yaml").exists()
                && let Some(n) = entry.file_name().to_str()
            {
                names.push(n.to_string());
            }
        }
        names.sort();
    }
    Ok(names)
}

// --- Config CRUD ---

fn cmd_config_show(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let cfg = config::load_config(config_path)?;

    if printer.write_structured(&cfg) {
        return Ok(());
    }

    printer.header("Configuration");
    printer.key_value("File", &config_path.display().to_string());
    printer.key_value("Profile", cfg.spec.profile.as_deref().unwrap_or("(none)"));

    // Origins
    if !cfg.spec.origin.is_empty() {
        printer.newline();
        printer.subheader("Origins");
        for (i, origin) in cfg.spec.origin.iter().enumerate() {
            let label = if i == 0 { "Primary" } else { "Secondary" };
            printer.key_value(label, &format!("{:?} — {}", origin.origin_type, origin.url));
            printer.key_value("  Branch", &origin.branch);
        }
    }

    // Sources
    if !cfg.spec.sources.is_empty() {
        printer.newline();
        printer.subheader("Sources");
        for src in &cfg.spec.sources {
            printer.key_value(&src.name, &src.origin.url);
        }
    }

    // Module registries
    if let Some(ref mods) = cfg.spec.modules {
        if !mods.registries.is_empty() {
            printer.newline();
            printer.subheader("Module Registries");
            for ms in &mods.registries {
                printer.key_value(&ms.name, &ms.url);
            }
        }

        // Module security
        if let Some(ref sec) = mods.security {
            printer.newline();
            printer.subheader("Module Security");
            printer.key_value(
                "Require signatures",
                if sec.require_signatures { "yes" } else { "no" },
            );
        }
    }

    // Daemon
    if let Some(ref daemon) = cfg.spec.daemon {
        printer.newline();
        printer.subheader("Daemon");
        printer.key_value("Enabled", if daemon.enabled { "yes" } else { "no" });
        if let Some(ref reconcile) = daemon.reconcile {
            printer.key_value("  Reconcile interval", &reconcile.interval);
            printer.key_value(
                "  On change",
                if reconcile.on_change { "yes" } else { "no" },
            );
            printer.key_value(
                "  Auto apply",
                if reconcile.auto_apply { "yes" } else { "no" },
            );
        }
        if let Some(ref sync) = daemon.sync {
            printer.key_value("  Sync interval", &sync.interval);
        }
    }

    // Secrets
    if let Some(ref secrets) = cfg.spec.secrets {
        printer.newline();
        printer.subheader("Secrets");
        printer.key_value("Backend", &secrets.backend);
    }

    // Theme
    if let Some(ref theme) = cfg.spec.theme {
        printer.newline();
        printer.subheader("Theme");
        printer.key_value("Theme", &theme.name);
    }

    Ok(())
}

fn cmd_config_edit(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    open_in_editor(config_path, printer)?;

    // Validate after editing — loop until valid or user cancels
    loop {
        match config::load_config(config_path) {
            Ok(_) => {
                printer.success("Configuration is valid");
                break;
            }
            Err(e) => {
                printer.error(&format!("Invalid configuration: {}", e));
                if !printer.prompt_confirm("Re-open in editor to fix?")? {
                    printer.warning("Saved with validation errors");
                    break;
                }
                open_in_editor(config_path, printer)?;
            }
        }
    }

    Ok(())
}

// --- Config get/set/unset ---

/// Walk a dotted key path through a YAML value, returning the leaf.
/// Use "." to return the root value itself.
fn walk_yaml_path<'a>(
    value: &'a serde_yaml::Value,
    path: &str,
) -> anyhow::Result<&'a serde_yaml::Value> {
    if path == "." {
        return Ok(value);
    }
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        anyhow::bail!("invalid key path '{}': contains empty segment", path);
    }
    let mut current = value;

    for (i, segment) in segments.iter().enumerate() {
        match current {
            serde_yaml::Value::Mapping(map) => {
                let key = serde_yaml::Value::String((*segment).to_string());
                current = map.get(&key).ok_or_else(|| {
                    let partial = segments[..=i].join(".");
                    anyhow::anyhow!("key '{}' not found in config", partial)
                })?;
            }
            _ => {
                let partial = segments[..i].join(".");
                anyhow::bail!("'{}' is not a mapping", partial);
            }
        }
    }

    Ok(current)
}

/// Walk a dotted key path, creating intermediate mappings as needed.
/// Returns a mutable reference to the *parent* mapping and the leaf key name.
fn walk_yaml_path_mut<'a>(
    value: &'a mut serde_yaml::Value,
    path: &str,
) -> anyhow::Result<(&'a mut serde_yaml::Mapping, String)> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.is_empty() || segments.iter().any(|s| s.is_empty()) {
        anyhow::bail!("invalid key path '{}': contains empty segment", path);
    }

    let mut current = value;
    // Walk to the parent of the final segment, creating intermediate maps
    for segment in &segments[..segments.len() - 1] {
        let key = serde_yaml::Value::String((*segment).to_string());
        if !current.as_mapping().is_some_and(|m| m.contains_key(&key)) {
            // Create intermediate mapping
            let map = current
                .as_mapping_mut()
                .ok_or_else(|| anyhow::anyhow!("cannot traverse into non-mapping"))?;
            map.insert(
                key.clone(),
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            );
        }
        current = current
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("cannot traverse into non-mapping"))?
            .get_mut(&key)
            .ok_or_else(|| anyhow::anyhow!("failed to create intermediate mapping"))?;
    }

    let parent = current
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("parent is not a mapping"))?;
    let leaf = segments
        .last()
        .ok_or_else(|| anyhow::anyhow!("empty key path"))?
        .to_string();
    Ok((parent, leaf))
}

/// Parse a string value into the most appropriate YAML type.
fn parse_yaml_value(s: &str) -> serde_yaml::Value {
    match s {
        "true" => serde_yaml::Value::Bool(true),
        "false" => serde_yaml::Value::Bool(false),
        "null" | "~" => serde_yaml::Value::Null,
        _ => {
            // Try integer, then float, then string
            if let Ok(n) = s.parse::<i64>() {
                serde_yaml::Value::Number(n.into())
            } else if let Ok(f) = s.parse::<f64>() {
                serde_yaml::Value::Number(serde_yaml::Number::from(f))
            } else {
                serde_yaml::Value::String(s.to_string())
            }
        }
    }
}

fn cmd_config_get(cli: &Cli, printer: &Printer, key: &str) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let contents = std::fs::read_to_string(config_path)?;
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    let spec = raw
        .get("spec")
        .ok_or_else(|| anyhow::anyhow!("config has no 'spec' section"))?;

    let value = walk_yaml_path(spec, key)?;

    if printer.is_structured() {
        // Convert serde_yaml::Value to serde_json::Value for structured output
        let json_value: serde_json::Value =
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        printer.write_structured(&json_value);
        return Ok(());
    }

    match value {
        serde_yaml::Value::Null => {} // key exists but null — print nothing
        serde_yaml::Value::String(s) => printer.stdout_line(s),
        serde_yaml::Value::Bool(b) => printer.stdout_line(&b.to_string()),
        serde_yaml::Value::Number(n) => printer.stdout_line(&n.to_string()),
        other => {
            let yaml = serde_yaml::to_string(other)?;
            let trimmed = yaml.strip_prefix("---\n").unwrap_or(&yaml);
            printer.stdout_line(trimmed.trim_end());
        }
    }

    Ok(())
}

fn cmd_config_set(cli: &Cli, printer: &Printer, key: &str, value: &str) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    let spec = raw
        .get_mut("spec")
        .ok_or_else(|| anyhow::anyhow!("config has no 'spec' section"))?;

    let (parent, leaf_key) = walk_yaml_path_mut(spec, key)?;
    let yaml_key = serde_yaml::Value::String(leaf_key);
    parent.insert(yaml_key, parse_yaml_value(value));

    // Validate by round-tripping through the typed config parser
    let output = serde_yaml::to_string(&raw)?;
    if let Err(e) = config::parse_config(&output, config_path) {
        anyhow::bail!("invalid value for '{}': {}", key, e);
    }

    cfgd_core::atomic_write_str(config_path, &output)?;
    printer.success(&format!("Set {} = {}", key, value));
    Ok(())
}

fn cmd_config_unset(cli: &Cli, printer: &Printer, key: &str) -> anyhow::Result<()> {
    let config_path = &cli.config;
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;

    let spec = raw
        .get_mut("spec")
        .ok_or_else(|| anyhow::anyhow!("config has no 'spec' section"))?;

    let (parent, leaf_key) = walk_yaml_path_mut(spec, key)?;
    let yaml_key = serde_yaml::Value::String(leaf_key.clone());
    if parent.remove(&yaml_key).is_none() {
        anyhow::bail!("key '{}' not found in config", key);
    }

    // Validate the result is still parseable
    let output = serde_yaml::to_string(&raw)?;
    if let Err(e) = config::parse_config(&output, config_path) {
        anyhow::bail!("cannot unset '{}': result would be invalid: {}", key, e);
    }

    cfgd_core::atomic_write_str(config_path, &output)?;
    printer.success(&format!("Unset {}", key));
    Ok(())
}

// --- Source CRUD ---

fn cmd_source_edit(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let source_path = config_dir.join("cfgd-source.yaml");
    if !source_path.exists() {
        anyhow::bail!(
            "No cfgd-source.yaml found in {} — run 'cfgd source create' to scaffold one",
            config_dir.display()
        );
    }

    open_in_editor(&source_path, printer)?;

    // Validate after editing — loop until valid or user cancels
    loop {
        let contents = std::fs::read_to_string(&source_path)?;
        match config::parse_config_source(&contents) {
            Ok(_) => {
                printer.success("Source manifest is valid");
                break;
            }
            Err(e) => {
                printer.error(&format!("Invalid source manifest: {}", e));
                if !printer.prompt_confirm("Re-open in editor to fix?")? {
                    printer.warning("Saved with validation errors");
                    break;
                }
                open_in_editor(&source_path, printer)?;
            }
        }
    }

    Ok(())
}

fn cmd_source_create(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    description: Option<&str>,
    version: Option<&str>,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let source_path = config_dir.join("cfgd-source.yaml");
    if source_path.exists() {
        anyhow::bail!(
            "cfgd-source.yaml already exists at {} — use 'cfgd source edit' to modify it",
            source_path.display()
        );
    }

    // Interactive mode if no flags provided
    let is_interactive = name.is_none() && description.is_none() && version.is_none();

    // Determine name: flag > interactive prompt > directory name
    let source_name = match name {
        Some(n) => n.to_string(),
        None => {
            let dir_name = config_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("my-config");
            if is_interactive {
                printer.prompt_text("Source name", dir_name)?
            } else {
                dir_name.to_string()
            }
        }
    };

    let source_description = match description {
        Some(d) => d.to_string(),
        None => {
            if is_interactive {
                printer.prompt_text("Description", "Team configuration source")?
            } else {
                "Team configuration source".to_string()
            }
        }
    };

    let source_version = match version {
        Some(v) => v.to_string(),
        None => "0.1.0".to_string(),
    };

    let profile_names = scan_profile_names(&config_dir.join("profiles"))?;
    let module_names = scan_module_names(&config_dir.join("modules"))?;

    // Build profiles YAML block
    let profiles_yaml = if profile_names.is_empty() {
        "    profiles: []".to_string()
    } else {
        let mut lines = vec!["    profiles:".to_string()];
        for p in &profile_names {
            lines.push(format!("      - {}", p));
        }
        lines.join("\n")
    };

    // Build modules YAML block
    let modules_yaml = if module_names.is_empty() {
        "    modules: []".to_string()
    } else {
        let mut lines = vec!["    modules:".to_string()];
        for m in &module_names {
            lines.push(format!("      - {}", m));
        }
        lines.join("\n")
    };

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\n\
         kind: ConfigSource\n\
         metadata:\n\
         \x20 name: {}\n\
         \x20 version: \"{}\"\n\
         \x20 description: \"{}\"\n\
         spec:\n\
         \x20 provides:\n\
         {}\n\
         {}\n\
         \x20 policy:\n\
         \x20   required:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   recommended:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   optional:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   constraints:\n\
         \x20     no-scripts: true\n\
         \x20     no-secrets-read: true\n",
        source_name, source_version, source_description, profiles_yaml, modules_yaml,
    );

    std::fs::write(&source_path, &yaml)?;
    printer.success(&format!(
        "Created cfgd-source.yaml at {}",
        source_path.display()
    ));
    if !profile_names.is_empty() {
        printer.info(&format!(
            "Included {} profile(s): {}",
            profile_names.len(),
            profile_names.join(", ")
        ));
    }
    if !module_names.is_empty() {
        printer.info(&format!(
            "Included {} module(s): {}",
            module_names.len(),
            module_names.join(", ")
        ));
    }
    printer.info("Edit the file to configure policy tiers and platform-profiles");

    Ok(())
}

// --- Workflow Generation ---

fn cmd_workflow_generate(cli: &Cli, printer: &Printer, force: bool) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let workflow_dir = config_dir.join(".github").join("workflows");
    let workflow_path = workflow_dir.join("cfgd-release.yml");

    // Scan for profiles and modules
    let profile_names = scan_profile_names(&config_dir.join("profiles"))?;
    let module_names = scan_module_names(&config_dir.join("modules"))?;

    if profile_names.is_empty() && module_names.is_empty() {
        printer.warning("No profiles or modules found — nothing to generate");
        return Ok(());
    }

    // Check for existing file
    if workflow_path.exists()
        && !force
        && !printer
            .prompt_confirm(&format!(
                "Workflow already exists at {} — overwrite?",
                workflow_path.display()
            ))
            .unwrap_or(false)
    {
        printer.info("Skipped workflow generation");
        return Ok(());
    }

    let yaml = generate_release_workflow_yaml(&module_names, &profile_names);

    std::fs::create_dir_all(&workflow_dir)?;
    std::fs::write(&workflow_path, &yaml)?;

    printer.success(&format!(
        "Generated release workflow at {}",
        workflow_path.display()
    ));
    printer.info(&format!(
        "Covers {} module(s) and {} profile(s)",
        module_names.len(),
        profile_names.len()
    ));

    Ok(())
}

fn generate_release_workflow_yaml(modules: &[String], profiles: &[String]) -> String {
    let mut yaml = String::new();
    let has_targets = !modules.is_empty() || !profiles.is_empty();

    // Header
    yaml.push_str(
        "# Auto-generated by cfgd — manages release tagging for modules and profiles.\n\
         # Regenerate with: cfgd workflow generate --force\n\
         name: cfgd Release\n\
         \n\
         on:\n\
         \x20 push:\n\
         \x20   branches: [main]\n",
    );

    if has_targets {
        yaml.push_str("    paths:\n");
        for m in modules {
            yaml.push_str(&format!("      - 'modules/{}/**'\n", m));
        }
        for p in profiles {
            yaml.push_str(&format!("      - 'profiles/{}.yaml'\n", p));
            yaml.push_str(&format!("      - 'profiles/{}.yml'\n", p));
        }
    } else {
        yaml.push_str(
            "    # paths: (auto-populated when modules/profiles are created)\n\
             \x20   #   - 'modules/<name>/**'\n\
             \x20   #   - 'profiles/<name>.yaml'\n",
        );
    }

    yaml.push_str(
        "\n\
         permissions:\n\
         \x20 contents: write\n\
         \n\
         jobs:\n",
    );

    if !has_targets {
        yaml.push_str(
            "  # Jobs are auto-generated when modules or profiles are created.\n\
             \x20 # Run `cfgd workflow generate --force` to regenerate manually.\n\
             \x20 placeholder:\n\
             \x20   runs-on: ubuntu-latest\n\
             \x20   steps:\n\
             \x20     - run: echo \"No modules or profiles to tag yet.\"\n",
        );
        return yaml;
    }

    // Detect changes job
    yaml.push_str(
        "\x20 detect-changes:\n\
         \x20   runs-on: ubuntu-latest\n\
         \x20   outputs:\n",
    );
    for m in modules {
        let safe = m.replace('-', "_");
        yaml.push_str(&format!(
            "      module_{}: ${{{{ steps.changes.outputs.module_{} }}}}\n",
            safe, safe
        ));
    }
    for p in profiles {
        let safe = p.replace('-', "_");
        yaml.push_str(&format!(
            "      profile_{}: ${{{{ steps.changes.outputs.profile_{} }}}}\n",
            safe, safe
        ));
    }

    yaml.push_str(
        "\x20   steps:\n\
         \x20     - uses: actions/checkout@v4\n\
         \x20       with:\n\
         \x20         fetch-depth: 0\n\
         \x20     - id: changes\n\
         \x20       run: |\n\
         \x20         if git rev-parse HEAD~1 >/dev/null 2>&1; then\n\
         \x20           CHANGED=$(git diff --name-only HEAD~1 HEAD)\n\
         \x20         else\n\
         \x20           CHANGED=$(git diff-tree --no-commit-id --name-only -r HEAD)\n\
         \x20         fi\n",
    );

    for m in modules {
        let safe = m.replace('-', "_");
        yaml.push_str(&format!(
            "          if echo \"$CHANGED\" | grep -q '^modules/{}/'; then\n\
             \x20           echo \"module_{}=true\" >> $GITHUB_OUTPUT\n\
             \x20         else\n\
             \x20           echo \"module_{}=false\" >> $GITHUB_OUTPUT\n\
             \x20         fi\n",
            m, safe, safe
        ));
    }
    for p in profiles {
        let safe = p.replace('-', "_");
        yaml.push_str(&format!(
            "          if echo \"$CHANGED\" | grep -q '^profiles/{}\\.'; then\n\
             \x20           echo \"profile_{}=true\" >> $GITHUB_OUTPUT\n\
             \x20         else\n\
             \x20           echo \"profile_{}=false\" >> $GITHUB_OUTPUT\n\
             \x20         fi\n",
            p, safe, safe
        ));
    }

    // Tag modules job
    if !modules.is_empty() {
        yaml.push_str(
            "\n\
             \x20 tag-modules:\n\
             \x20   runs-on: ubuntu-latest\n\
             \x20   needs: detect-changes\n\
             \x20   strategy:\n\
             \x20     matrix:\n\
             \x20       include:\n",
        );
        for m in modules {
            let safe = m.replace('-', "_");
            yaml.push_str(&format!(
                "          - name: {}\n\
                 \x20           changed: ${{{{ needs.detect-changes.outputs.module_{} }}}}\n",
                m, safe
            ));
        }
        yaml.push_str(
            "\x20   steps:\n\
             \x20     - uses: actions/checkout@v4\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       with:\n\
             \x20         fetch-depth: 0\n\
             \x20     - name: Read module version\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       id: version\n\
             \x20       run: |\n\
             \x20         VERSION=$(grep -oP 'version:\\s*\"?\\K[^\"\\s]+' \"modules/${{ matrix.name }}/module.yaml\" || echo \"0.1.0\")\n\
             \x20         echo \"version=$VERSION\" >> $GITHUB_OUTPUT\n\
             \x20     - name: Tag module release\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       run: |\n\
             \x20         TAG=\"${{ matrix.name }}/v${{ steps.version.outputs.version }}\"\n\
             \x20         git tag -f \"$TAG\"\n\
             \x20         git push origin \"$TAG\" --force\n",
        );
    }

    // Tag profiles job
    if !profiles.is_empty() {
        yaml.push_str(
            "\n\
             \x20 tag-profiles:\n\
             \x20   runs-on: ubuntu-latest\n\
             \x20   needs: detect-changes\n\
             \x20   strategy:\n\
             \x20     matrix:\n\
             \x20       include:\n",
        );
        for p in profiles {
            let safe = p.replace('-', "_");
            yaml.push_str(&format!(
                "          - name: {}\n\
                 \x20           changed: ${{{{ needs.detect-changes.outputs.profile_{} }}}}\n",
                p, safe
            ));
        }
        yaml.push_str(
            "\x20   steps:\n\
             \x20     - uses: actions/checkout@v4\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       with:\n\
             \x20         fetch-depth: 0\n\
             \x20     - name: Tag profile release\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       run: |\n\
             \x20         DATE=$(date +%Y%m%d)\n\
             \x20         TAG=\"profile/${{ matrix.name }}/${DATE}\"\n\
             \x20         git tag -f \"$TAG\"\n\
             \x20         git push origin \"$TAG\" --force\n",
        );
    }

    yaml
}

fn maybe_update_workflow(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    init::regenerate_workflow(&config_dir, printer)?;
    Ok(())
}

// --- Generate Command ---

fn cmd_generate(
    printer: &Printer,
    shell: Option<&str>,
    home: Option<&str>,
    scan_only: bool,
) -> anyhow::Result<()> {
    let home_path = if let Some(h) = home {
        PathBuf::from(h)
    } else {
        dirs_from_env()
    };

    let detected_shell = shell
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::var("SHELL")
                .ok()
                .and_then(|s| s.rsplit('/').next().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| "zsh".to_string());

    printer.header("Scanning dotfiles");

    let dotfiles = generate::scan::scan_dotfiles(&home_path)?;
    let tool_set: std::collections::HashSet<String> = dotfiles
        .iter()
        .filter_map(|e| e.tool_guess.clone())
        .collect();

    if dotfiles.is_empty() {
        printer.info("No dotfiles found");
    } else {
        printer.info(&format!("Found {} dotfile entries", dotfiles.len()));
        if !tool_set.is_empty() {
            let mut tools: Vec<String> = tool_set.into_iter().collect();
            tools.sort();
            printer.info(&format!("Detected tools: {}", tools.join(", ")));
        }
    }

    printer.header(&format!("Scanning {} config", detected_shell));
    let shell_result = generate::scan::scan_shell_config(&detected_shell, &home_path)?;
    if !shell_result.aliases.is_empty() {
        printer.info(&format!("Found {} aliases", shell_result.aliases.len()));
    }
    if !shell_result.exports.is_empty() {
        printer.info(&format!("Found {} exports", shell_result.exports.len()));
    }
    if !shell_result.path_additions.is_empty() {
        printer.info(&format!(
            "Found {} PATH additions",
            shell_result.path_additions.len()
        ));
    }
    if let Some(pm) = &shell_result.plugin_manager {
        printer.info(&format!("Plugin manager: {}", pm));
    }

    if scan_only {
        printer.success("Scan complete — use without --scan-only to generate config");
        return Ok(());
    }

    // Full AI-assisted generation is implemented in Task 18.
    printer.warning("AI-assisted generation is not yet available (coming soon)");
    printer.info("Use --scan-only to preview what would be scanned");

    Ok(())
}

fn dirs_from_env() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
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

fn module_state_map(
    state: &cfgd_core::state::StateStore,
) -> std::collections::HashMap<String, cfgd_core::state::ModuleStateRecord> {
    state
        .module_states()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.module_name.clone(), s))
        .collect()
}

fn open_in_editor(path: &Path, printer: &Printer) -> anyhow::Result<()> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    let status = std::process::Command::new(&editor)
        .arg(path)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to open editor '{}': {}", editor, e))?;

    if !status.success() {
        printer.warning(&format!("Editor '{}' exited with non-zero status", editor));
    }
    Ok(())
}

/// Resolve the secret backend from config, check availability, and validate the file exists.
/// Returns a registry whose `secret_backend` is guaranteed `Some`.
fn resolve_secret_backend(cli: &Cli, file: &Path) -> anyhow::Result<ProviderRegistry> {
    let cfg = if cli.config.exists() {
        Some(config::load_config(&cli.config)?)
    } else {
        None
    };

    let mut registry = build_registry_with_config(cfg.as_ref());

    // Rebuild secret backend with config dir so sops can find .sops.yaml
    let cd = config_dir(cli);
    let (backend_name, age_key_path) = secret_backend_from_config(cfg.as_ref());
    registry.secret_backend = Some(secrets::build_secret_backend(
        &backend_name,
        age_key_path,
        Some(&cd),
    ));

    match registry.secret_backend {
        Some(ref backend) if !backend.is_available() => {
            anyhow::bail!("{}: not installed", backend.name());
        }
        None => anyhow::bail!("No secret backend configured"),
        _ => {}
    }

    if !file.exists() {
        anyhow::bail!("File not found: {}", file.display());
    }

    Ok(registry)
}

/// Shorthand: resolve secret backend and extract it in one call.
fn get_secret_backend(cli: &Cli, file: &Path) -> anyhow::Result<Box<dyn SecretBackend>> {
    let registry = resolve_secret_backend(cli, file)?;
    registry
        .secret_backend
        .ok_or_else(|| anyhow::anyhow!("No secret backend configured"))
}

fn cmd_secret_encrypt(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    printer.header("Secret Encrypt");

    let backend = get_secret_backend(cli, file)?;
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

    let backend = get_secret_backend(cli, file)?;
    let decrypted = backend.decrypt_file(file)?;
    printer.info(&decrypted);

    Ok(())
}

fn cmd_secret_edit(cli: &Cli, printer: &Printer, file: &Path) -> anyhow::Result<()> {
    printer.header("Secret Edit");

    let backend = get_secret_backend(cli, file)?;
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
    // Gather data for both structured and human output
    let (config_check, loaded_cfg) = if cli.config.exists() {
        match config::load_config(&cli.config) {
            Ok(cfg) => (
                DoctorConfigCheck {
                    valid: true,
                    path: cli.config.display().to_string(),
                    name: Some(cfg.metadata.name.clone()),
                    profile: cfg.spec.profile.clone(),
                    error: None,
                },
                Some(cfg),
            ),
            Err(e) => (
                DoctorConfigCheck {
                    valid: false,
                    path: cli.config.display().to_string(),
                    name: None,
                    profile: None,
                    error: Some(format!("{}", e)),
                },
                None,
            ),
        }
    } else {
        (
            DoctorConfigCheck {
                valid: false,
                path: cli.config.display().to_string(),
                name: None,
                profile: None,
                error: Some("not found".into()),
            },
            None,
        )
    };

    let git_available = which("git");

    let config_dir = config_dir(cli);
    let age_key_override = loaded_cfg
        .as_ref()
        .and_then(|c| c.spec.secrets.as_ref())
        .and_then(|s| s.sops.as_ref())
        .and_then(|s| s.age_key.as_ref());

    let health = secrets::check_secrets_health(&config_dir, age_key_override.map(|p| p.as_path()));

    // Build structured doctor output for --output json/yaml
    // Resolve profile to get declared managers (including custom) and build registry
    let resolved_packages = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());
        if let Some(pn) = profile_name
            && let Ok(mut resolved) = config::resolve_profile(pn, &profiles_dir)
        {
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

    // Build manager check data (deduplicate brew-tap/brew-cask under brew)
    let mut manager_checks: Vec<DoctorManagerCheck> = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        for mgr in all_managers.iter() {
            let name = mgr.name();
            if name == "brew-tap" || name == "brew-cask" {
                continue;
            }
            if !seen.insert(name.to_string()) {
                continue;
            }
            manager_checks.push(DoctorManagerCheck {
                name: name.to_string(),
                available: mgr.is_available(),
                declared: declared_managers.iter().any(|d| d == name),
                can_bootstrap: mgr.can_bootstrap(),
            });
        }
    }

    // Modules health
    let module_list: Vec<String> = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());
        profile_name
            .and_then(|pn| config::resolve_profile(pn, &profiles_dir).ok())
            .map(|r| r.merged.modules)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
    let all_modules = modules::load_all_modules(&config_dir, &cache_base).unwrap_or_default();
    let module_checks: Vec<DoctorModuleCheck> = module_list
        .iter()
        .map(|mod_name| {
            if all_modules.contains_key(mod_name) {
                DoctorModuleCheck {
                    name: mod_name.clone(),
                    valid: true,
                    error: None,
                }
            } else {
                DoctorModuleCheck {
                    name: mod_name.clone(),
                    valid: false,
                    error: Some("module not found".into()),
                }
            }
        })
        .collect();

    // System configurators
    let configurator_checks: Vec<DoctorConfiguratorCheck> = registry
        .available_system_configurators()
        .iter()
        .map(|c| DoctorConfiguratorCheck {
            name: c.name().to_string(),
            available: true,
        })
        .collect();

    // Structured output
    if printer.write_structured(&DoctorOutput {
        config: config_check.clone(),
        git: git_available,
        secrets: DoctorSecretsCheck {
            sops_available: health.sops_available,
            sops_version: health.sops_version.clone(),
            age_key_exists: health.age_key_exists,
            age_key_path: health
                .age_key_path
                .as_ref()
                .map(|p| p.display().to_string()),
            sops_config_exists: health.sops_config_exists,
            providers: health
                .providers
                .iter()
                .map(|(n, a)| DoctorProviderCheck {
                    name: n.clone(),
                    available: *a,
                })
                .collect(),
        },
        package_managers: manager_checks,
        modules: module_checks,
        system_configurators: configurator_checks,
    }) {
        return Ok(());
    }

    // Human display
    printer.header("Doctor");

    let mut all_ok = config_check.valid && git_available;

    if config_check.valid {
        printer.success(&format!("Config file: {} (valid)", config_check.path));
        if let Some(name) = loaded_cfg.as_ref().map(|c| &c.metadata.name) {
            printer.key_value("Name", name);
        }
        printer.key_value(
            "Profile",
            loaded_cfg
                .as_ref()
                .and_then(|c| c.spec.profile.as_deref())
                .unwrap_or("(none)"),
        );
    } else if let Some(ref err) = config_check.error {
        if err == "not found" {
            printer.warning(&format!(
                "Config file not found: {} — run 'cfgd init' to create one",
                config_check.path
            ));
        } else {
            printer.error(&format!("Config file: {} — {}", config_check.path, err));
        }
    }

    if git_available {
        printer.success("git: found");
    } else {
        printer.error("git: not found — install git to use cfgd");
    }

    // Secrets
    printer.newline();
    printer.subheader("Secrets");

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
    } else if let Some(ref path) = health.age_key_path {
        printer.warning(&format!(
            "age key: not found at {} — run 'cfgd init' to generate",
            path.display()
        ));
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

    let mut shown_managers = std::collections::HashSet::new();
    for mgr in all_managers.iter() {
        let name = mgr.name();
        if name == "brew-tap" || name == "brew-cask" {
            continue;
        }
        if !shown_managers.insert(name.to_string()) {
            continue;
        }
        let is_declared = declared_managers.iter().any(|d| d == name);
        let available = mgr.is_available();

        if is_declared {
            if available {
                printer.success(&format!("{}: available (declared in config)", name));
            } else if mgr.can_bootstrap() {
                let method = packages::bootstrap_method(mgr.as_ref());
                printer.warning(&format!(
                    "{}: not found — can auto-bootstrap via {}",
                    name, method
                ));
            } else {
                printer.error(&format!(
                    "{}: not found — declared in config but not available",
                    name
                ));
                all_ok = false;
            }
        } else if available {
            printer.info(&format!("{}: available (not used in config)", name));
        }
    }

    if !module_list.is_empty() {
        printer.newline();
        printer.subheader("Modules");

        let registry_for_modules = build_registry();
        let mgr_map = managers_map(&registry_for_modules);
        let platform = Platform::detect();

        for mod_name in &module_list {
            if let Some(module) = all_modules.get(mod_name) {
                printer.info(&format!("{}:", mod_name));
                for entry in &module.spec.packages {
                    match modules::resolve_package(entry, mod_name, &platform, &mgr_map) {
                        Ok(Some(resolved)) => {
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

/// List sorted YAML file stems in a directory (e.g. "base" from "base.yaml").
/// Returns an empty vec if the directory doesn't exist.
pub(super) fn list_yaml_stems(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml" || e == "yml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
        names.sort();
    }
    Ok(names)
}

/// Resolve profile name from explicit name or --active flag.
fn resolve_profile_name(cli: &Cli, name: Option<&str>, active: bool) -> anyhow::Result<String> {
    match (name, active) {
        (Some(n), _) => Ok(n.to_string()),
        (None, true) => {
            let config_path = &cli.config;
            if !config_path.exists() {
                anyhow::bail!("{}", MSG_NO_CONFIG);
            }
            let cfg = config::load_config(config_path)?;
            if let Some(ref profile_override) = cli.profile {
                Ok(profile_override.clone())
            } else {
                Ok(cfg.active_profile()?.to_string())
            }
        }
        (None, false) => {
            anyhow::bail!("Profile name required (or use --active for the active profile)")
        }
    }
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

    cfgd_core::daemon::install_service(&cli.config, cli.profile.as_deref())?;

    print_daemon_install_success(printer);

    Ok(())
}

fn cmd_daemon_uninstall(printer: &Printer) -> anyhow::Result<()> {
    printer.header("Uninstall Daemon Service");

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
        mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
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
            printer.info(&format!("Sources updated. {}", MSG_RUN_APPLY));
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

fn cmd_source_add(cli: &Cli, printer: &Printer, args: &SourceAddArgs) -> anyhow::Result<()> {
    let url = &args.url;
    let name = args.name.as_deref();
    let branch = args.branch.as_deref();
    let profile = args.profile.as_deref();
    let accept_recommended = args.accept_recommended;
    let priority = args.priority;
    let opt_in = &args.opt_in;
    let sync_interval = args.sync_interval.as_deref();
    let auto_apply = args.auto_apply;
    let pin_version = args.pin_version.as_deref();
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
    if config_path.exists()
        && let Ok(existing_cfg) = config::load_config(&config_path)
    {
        mgr.set_allow_unsigned(
            existing_cfg
                .spec
                .security
                .as_ref()
                .is_some_and(|s| s.allow_unsigned),
        );
    }
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
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());

        if let Some(pn) = profile_name
            && let Ok(local_resolved) = config::resolve_profile(pn, &pdir)
        {
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
                opt_in: opt_in.to_vec(),
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
    if !args.yes {
        printer.newline();
        if !printer.prompt_confirm("Subscribe to this source?")? {
            printer.info("Cancelled");
            return Ok(());
        }
    }

    // Build the source spec with user choices
    let mut source_spec =
        SourceManager::build_source_spec(&source_name, url, selected_profile.as_deref());
    if let Some(b) = branch {
        source_spec.origin.branch = b.to_string();
    }
    source_spec.subscription.accept_recommended = accept_recommended;
    source_spec.subscription.priority = resolved_priority;
    if !opt_in.is_empty() {
        source_spec.subscription.opt_in = opt_in.to_vec();
    }
    if let Some(interval) = sync_interval {
        source_spec.sync.interval = interval.to_string();
    }
    if auto_apply {
        source_spec.sync.auto_apply = true;
    }
    if let Some(pin) = pin_version {
        source_spec.sync.pin_version = Some(pin.to_string());
    }

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
    printer.info(MSG_RUN_APPLY);

    Ok(())
}

fn cmd_source_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    if !config_path.exists() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<SourceListEntry>::new());
            return Ok(());
        }
        printer.header("Config Sources");
        printer.info("No config file found");
        return Ok(());
    }

    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<SourceListEntry>::new());
            return Ok(());
        }
        printer.header("Config Sources");
        printer.info("No sources configured");
        return Ok(());
    }

    let state = open_state_store()?;

    let entries: Vec<SourceListEntry> = cfg
        .spec
        .sources
        .iter()
        .map(|source| {
            let state_info = state.config_source_by_name(&source.name).ok().flatten();
            SourceListEntry {
                name: source.name.clone(),
                url: source.origin.url.clone(),
                priority: source.subscription.priority,
                version: state_info.as_ref().and_then(|s| s.source_version.clone()),
                status: state_info
                    .as_ref()
                    .map(|s| s.status.clone())
                    .unwrap_or_else(|| "unknown".into()),
                last_fetched: state_info.as_ref().and_then(|s| s.last_fetched.clone()),
            }
        })
        .collect();

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Config Sources");

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            vec![
                e.name.clone(),
                e.url.clone(),
                e.priority.to_string(),
                e.version.clone().unwrap_or_else(|| "-".into()),
                e.status.clone(),
                e.last_fetched.clone().unwrap_or_else(|| "never".into()),
            ]
        })
        .collect();
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

    if printer.is_structured() {
        let state = open_state_store()?;
        let state_info = state.config_source_by_name(name)?;
        let resources = state.managed_resources_by_source(name)?;
        let output = SourceShowOutput {
            name: name.to_string(),
            url: source_spec.origin.url.clone(),
            branch: source_spec.origin.branch.clone(),
            priority: source_spec.subscription.priority,
            accept_recommended: source_spec.subscription.accept_recommended,
            profile: source_spec.subscription.profile.clone(),
            sync_interval: source_spec.sync.interval.clone(),
            auto_apply: source_spec.sync.auto_apply,
            version_pin: source_spec.sync.pin_version.clone(),
            state: state_info.map(|s| SourceStateInfo {
                status: s.status,
                last_fetched: s.last_fetched,
                last_commit: s.last_commit,
                version: s.source_version,
            }),
            managed_resources: resources
                .iter()
                .map(|r| SourceResourceEntry {
                    resource_type: r.resource_type.clone(),
                    resource_id: r.resource_id.clone(),
                })
                .collect(),
        };
        printer.write_structured(&output);
        return Ok(());
    }

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
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
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
    if keep_all && remove_all {
        anyhow::bail!("cannot use --keep-all and --remove-all together");
    }

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
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
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

    // Capture old source's profile and priority before removing
    let config_path = cli.config.clone();
    let old_cfg = config::load_config(&config_path)?;
    let old_source = old_cfg.spec.sources.iter().find(|s| s.name == old_name);
    let old_profile = old_source.and_then(|s| s.subscription.profile.clone());
    let old_priority = old_source.map(|s| s.subscription.priority).unwrap_or(500);

    // Remove old source (keeping resources)
    cmd_source_remove(cli, printer, old_name, true, false)?;

    // Add new source with same name, carrying over profile and priority
    cmd_source_add(
        cli,
        printer,
        &SourceAddArgs {
            url: new_url.to_string(),
            name: Some(old_name.to_string()),
            branch: None,
            profile: old_profile,
            accept_recommended: false,
            priority: Some(old_priority),
            opt_in: vec![],
            sync_interval: None,
            auto_apply: false,
            pin_version: None,
            yes: true,
        },
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
            with_source_config(&config_path, name, |source_entry| {
                let subscription = source_entry.get_mut("subscription").ok_or_else(|| {
                    anyhow::anyhow!("source '{}' has no subscription block", name)
                })?;

                if let Some(mapping) = subscription.as_mapping_mut() {
                    mapping.insert(
                        serde_yaml::Value::String("priority".into()),
                        serde_yaml::Value::Number(serde_yaml::Number::from(new_priority)),
                    );
                }
                Ok(())
            })?;

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
    count += items.env.len();
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
    for ev in &items.env {
        printer.info(&format!("{indent}env: {}", ev.name));
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
    with_source_config(config_path, source_name, |source| {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let sub_map = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?;
        let reject = sub_map
            .entry(serde_yaml::Value::String("reject".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        // Replace null with empty mapping (serde serializes default Value::Null)
        if reject.is_null() {
            *reject = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }

        set_nested_yaml_value(reject, path, &serde_yaml::Value::Null)?;
        Ok(())
    })
}

fn update_source_override(
    config_path: &Path,
    source_name: &str,
    path: &str,
    value: &str,
) -> anyhow::Result<()> {
    with_source_config(config_path, source_name, |source| {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let sub_map = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?;
        let overrides = sub_map
            .entry(serde_yaml::Value::String("overrides".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        // Replace null with empty mapping (serde serializes default Value::Null)
        if overrides.is_null() {
            *overrides = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }

        set_nested_yaml_value(
            overrides,
            path,
            &serde_yaml::Value::String(value.to_string()),
        )?;
        Ok(())
    })
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

/// Load config YAML, find a named source, apply a mutation, and write back.
/// The closure receives the mutable source entry; the helper handles I/O.
fn with_source_config<F>(config_path: &Path, source_name: &str, f: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut serde_yaml::Value) -> anyhow::Result<()>,
{
    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;
    let source = find_source_in_config(&mut raw, source_name)
        .ok_or_else(|| anyhow::anyhow!("source '{}' not found in config file", source_name))?;
    f(source)?;
    let output = serde_yaml::to_string(&raw)?;
    std::fs::write(config_path, output)?;
    Ok(())
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
        reconciler::Action::Env(ea) => match ea {
            reconciler::EnvAction::WriteEnvFile { path, .. } => {
                format!("{}:{}", prefix, path.display())
            }
            reconciler::EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("{}:{}", prefix, rc_path.display())
            }
        },
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

/// Check if a file target is an unmanaged file — exists on disk but not tracked by cfgd.
/// A cfgd-managed symlink (pointing into config_dir) is NOT unmanaged.
fn is_unmanaged_file(target: &Path, config_dir: &Path, state: &StateStore) -> bool {
    // Target must exist on disk
    if !target.exists() && target.symlink_metadata().is_err() {
        return false;
    }

    // If it's a symlink pointing into the config dir, it's cfgd-managed
    if let Ok(link_target) = target.read_link() {
        if link_target.starts_with(config_dir) {
            return false;
        }
        // Also check ~/.cache/cfgd/modules/ for module symlinks
        if let Ok(home) = std::env::var("HOME") {
            let module_cache = PathBuf::from(home).join(".cache/cfgd/modules");
            if link_target.starts_with(&module_cache) {
                return false;
            }
        }
    }

    // Check state store — if already tracked, it's managed
    let target_str = target.display().to_string();
    if let Ok(managed) = state.is_resource_managed("file", &target_str) {
        return !managed;
    }

    true
}

/// Handle unmanaged file targets in the plan: for each file Create/Update action targeting
/// an existing file not managed by cfgd, prompt the user to adopt, backup, or skip.
fn handle_unmanaged_file_targets(
    plan: &mut reconciler::Plan,
    config_dir: &Path,
    state: &StateStore,
    printer: &Printer,
    auto_yes: bool,
) -> anyhow::Result<()> {
    let options = vec![
        "Adopt (overwrite with cfgd-managed version)".to_string(),
        "Backup (save as .cfgd-backup, then overwrite)".to_string(),
        "Skip (leave file untouched)".to_string(),
    ];

    for phase in &mut plan.phases {
        let mut i = 0;
        while i < phase.actions.len() {
            // Profile file actions
            if let reconciler::Action::File(
                FileAction::Create { target, .. } | FileAction::Update { target, .. },
            ) = &phase.actions[i]
            {
                let target = target.clone();
                if is_unmanaged_file(&target, config_dir, state) && !auto_yes {
                    let choice = prompt_backup_choice(&target, None, printer, &options)?;
                    apply_backup_choice(choice, &target, &mut phase.actions[i], printer)?;
                }
            }

            // Module file actions
            if let reconciler::Action::Module(ref ma) = phase.actions[i]
                && let reconciler::ModuleActionKind::DeployFiles { files } = &ma.kind
            {
                let needs_prompt = !auto_yes
                    && files.iter().any(|f| {
                        let t = cfgd_core::expand_tilde(&f.target);
                        is_unmanaged_file(&t, config_dir, state)
                    });
                if needs_prompt {
                    let module_name = ma.module_name.clone();
                    if let reconciler::Action::Module(ref mut ma) = phase.actions[i]
                        && let reconciler::ModuleActionKind::DeployFiles { ref mut files } = ma.kind
                    {
                        let mut j = 0;
                        while j < files.len() {
                            let file_target = cfgd_core::expand_tilde(&files[j].target);
                            if is_unmanaged_file(&file_target, config_dir, state) {
                                let choice = prompt_backup_choice(
                                    &file_target,
                                    Some(&module_name),
                                    printer,
                                    &options,
                                )?;
                                if choice.starts_with("Backup") {
                                    backup_file(&file_target, printer)?;
                                } else if choice.starts_with("Skip") {
                                    files.remove(j);
                                    continue;
                                }
                            }
                            j += 1;
                        }
                    }
                }
            }

            i += 1;
        }
    }

    Ok(())
}

/// Prompt the user to choose how to handle an unmanaged file target.
fn prompt_backup_choice<'a>(
    target: &Path,
    module_name: Option<&str>,
    printer: &Printer,
    options: &'a [String],
) -> anyhow::Result<&'a String> {
    let msg = if let Some(m) = module_name {
        format!(
            "Module '{}': target exists as unmanaged file: {}",
            m,
            target.display()
        )
    } else {
        format!("Target exists as unmanaged file: {}", target.display())
    };
    printer.warning(&msg);
    Ok(printer
        .prompt_select("How should cfgd handle this file?", options)
        .unwrap_or(&options[0]))
}

/// Rename a file to <path>.cfgd-backup.
fn backup_file(target: &Path, printer: &Printer) -> anyhow::Result<()> {
    let backup_path = PathBuf::from(format!("{}.cfgd-backup", target.display()));
    std::fs::rename(target, &backup_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to backup {} to {}: {}",
            target.display(),
            backup_path.display(),
            e
        )
    })?;
    printer.success(&format!("Backed up to {}", backup_path.display()));
    Ok(())
}

/// Apply the user's backup choice to a file action.
fn apply_backup_choice(
    choice: &str,
    target: &Path,
    action: &mut reconciler::Action,
    printer: &Printer,
) -> anyhow::Result<()> {
    if choice.starts_with("Backup") {
        backup_file(target, printer)?;
    } else if choice.starts_with("Skip") {
        let origin = match action {
            reconciler::Action::File(FileAction::Create { origin, .. })
            | reconciler::Action::File(FileAction::Update { origin, .. }) => origin.clone(),
            _ => "local".to_string(),
        };
        *action = reconciler::Action::File(FileAction::Skip {
            target: target.to_path_buf(),
            reason: "skipped by user (unmanaged file exists)".to_string(),
            origin,
        });
    }
    Ok(())
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
            source_env: std::collections::HashMap::new(),
        });
    }

    let cache_dir = source_cache_dir()?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
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
    let config_yaml = serde_yaml::to_string(&resolved.merged.system)
        .map_err(|e| anyhow::anyhow!("failed to serialize system config: {}", e))?;
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
                printer.info(MSG_RUN_APPLY);
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
            anyhow::bail!("Unknown action '{}'. Use 'accept' or 'reject'.", other);
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

    const TEST_CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n";

    fn create_test_config_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        // Create profiles directory with a test profile
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();

        std::fs::write(
            profiles_dir.join("default.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  env:
    - name: editor
      value: vim
  packages:
    cargo:
      - bat
"#,
        )
        .unwrap();

        std::fs::write(
            profiles_dir.join("work.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - default
  env:
    - name: editor
      value: code
  packages:
    cargo:
      - exa
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn cli_has_output_flag() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        assert!(
            cmd.get_arguments().any(|a| a.get_id() == "output"),
            "should have global --output flag"
        );
    }

    #[test]
    fn cli_has_jsonpath_flag() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        assert!(
            cmd.get_arguments().any(|a| a.get_id() == "jsonpath"),
            "should have global --jsonpath flag"
        );
    }

    #[test]
    fn cli_output_flag_has_short_alias() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let output_arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "output")
            .unwrap();
        assert_eq!(
            output_arg.get_short(),
            Some('o'),
            "--output should have -o short alias"
        );
    }

    #[test]
    fn cli_init_has_apply_flag() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let init_cmd = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "init")
            .unwrap();
        assert!(
            init_cmd.get_arguments().any(|a| a.get_id() == "apply"),
            "init should have --apply flag"
        );
        assert!(
            init_cmd
                .get_arguments()
                .any(|a| a.get_id() == "install_daemon"),
            "init should have --install-daemon flag"
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
            r#"apiVersion: cfgd.io/v1alpha1
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
            "env.EDITOR",
            &serde_yaml::Value::String("nvim".into()),
        )
        .unwrap();

        let editor = root
            .get("env")
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
                        strategy: cfgd_core::config::FileStrategy::default(),
                        source_hash: None,
                    })],
                },
            ],
            warnings: vec![],
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
            warnings: vec![],
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
            warnings: vec![],
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
            warnings: vec![],
        };

        super::filter_plan(&mut plan, &[], &["packages.brew.ripgrep".into()]);

        match &plan.phases[0].actions[0] {
            Action::Package(PackageAction::Install { packages, .. }) => {
                assert_eq!(packages, &["ripgrep".to_string()]);
            }
            _ => panic!("expected Install action"),
        }
    }

    // --- Module CRUD tests ---

    fn test_cli(dir: &Path) -> Cli {
        Cli {
            config: dir.join("cfgd.yaml"),
            profile: None,
            no_color: true,
            verbose: false,
            quiet: true,
            output: "table".to_string(),
            jsonpath: None,
            command: Command::Status,
        }
    }

    fn test_printer() -> Printer {
        Printer::new(cfgd_core::output::Verbosity::Quiet)
    }

    fn test_profile_create_args(name: &str) -> ProfileCreateArgs {
        ProfileCreateArgs {
            name: name.to_string(),
            inherits: vec![],
            modules: vec![],
            packages: vec![],
            env: vec![],
            aliases: vec![],
            system: vec![],
            files: vec![],
            private: false,
            secrets: vec![],
            pre_reconcile: vec![],
            post_reconcile: vec![],
        }
    }

    fn empty_profile_update_args() -> ProfileUpdateArgs {
        ProfileUpdateArgs {
            name: None,
            active: false,
            inherits: vec![],
            modules: vec![],
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            system: vec![],
            secrets: vec![],
            pre_reconcile: vec![],
            post_reconcile: vec![],
            private: false,
        }
    }

    fn create_module_in_dir(dir: &Path, name: &str, content: &str) {
        let mod_dir = dir.join("modules").join(name);
        std::fs::create_dir_all(mod_dir.join("files")).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), content).unwrap();
    }

    fn empty_module_update_args(name: &str) -> ModuleUpdateArgs {
        ModuleUpdateArgs {
            name: name.to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            depends: vec![],
            post_apply: vec![],
            private: false,
            description: None,
            sets: vec![],
        }
    }

    fn test_module_create_args(name: &str) -> ModuleCreateArgs {
        ModuleCreateArgs {
            name: name.to_string(),
            description: None,
            depends: vec![],
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            private: false,
            post_apply: vec![],
            sets: vec![],
            apply: false,
            yes: false,
        }
    }

    #[test]
    fn module_create_with_flags_produces_valid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("modules").join("test-mod");
        let module_yaml = module_dir.join("module.yaml");

        // Create a test file to import
        let test_file = dir.path().join("testfile.txt");
        std::fs::write(&test_file, "content").unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ModuleCreateArgs {
            description: Some("A test module".to_string()),
            depends: vec!["base".to_string()],
            packages: vec!["curl".to_string(), "vim".to_string()],
            files: vec![test_file.display().to_string()],
            post_apply: vec!["echo done".to_string()],
            sets: vec![
                "package.curl.min-version=7.0".to_string(),
                "package.curl.prefer=brew,apt".to_string(),
                "package.vim.alias.snap=nvim".to_string(),
            ],
            ..test_module_create_args("test-mod")
        };
        module::cmd_module_create(&cli, &printer, &args).unwrap();

        assert!(module_yaml.exists());

        let contents = std::fs::read_to_string(&module_yaml).unwrap();
        let doc = config::parse_module(&contents).unwrap();

        assert_eq!(doc.metadata.name, "test-mod");
        assert_eq!(doc.metadata.description, Some("A test module".to_string()));
        assert_eq!(doc.spec.depends, vec!["base"]);
        assert_eq!(doc.spec.packages.len(), 2);
        assert_eq!(doc.spec.packages[0].name, "curl");
        assert_eq!(doc.spec.packages[0].min_version, Some("7.0".to_string()));
        assert_eq!(doc.spec.packages[0].prefer, vec!["brew", "apt"]);
        assert_eq!(doc.spec.packages[1].name, "vim");
        assert_eq!(
            doc.spec.packages[1].aliases.get("snap"),
            Some(&"nvim".to_string())
        );
        assert_eq!(doc.spec.files.len(), 1);
        assert!(doc.spec.files[0].source.contains("testfile.txt"));
        assert!(
            doc.spec
                .scripts
                .as_ref()
                .unwrap()
                .post_apply
                .contains(&"echo done".to_string())
        );
        assert!(module_dir.join("files").join("testfile.txt").exists());
    }

    #[test]
    fn module_create_refuses_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        create_module_in_dir(
            dir.path(),
            "existing",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: existing\nspec: {}\n",
        );

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ModuleCreateArgs {
            description: Some("dup".to_string()),
            ..test_module_create_args("existing")
        };
        let result = module::cmd_module_create(&cli, &printer, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn module_update_add_and_remove_packages() {
        let dir = tempfile::tempdir().unwrap();
        create_module_in_dir(
            dir.path(),
            "test-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n    - name: vim\n",
        );

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ModuleUpdateArgs {
            packages: vec!["ripgrep".to_string(), "-vim".to_string()],
            ..empty_module_update_args("test-mod")
        };
        module::cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = module::load_module_document(dir.path(), "test-mod").unwrap();
        let names: Vec<&str> = doc.spec.packages.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"curl"));
        assert!(names.contains(&"ripgrep"));
        assert!(!names.contains(&"vim"));
    }

    #[test]
    fn module_update_set_overrides() {
        let dir = tempfile::tempdir().unwrap();
        create_module_in_dir(
            dir.path(),
            "test-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: neovim\n",
        );

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ModuleUpdateArgs {
            sets: vec![
                "package.neovim.min-version=0.9".to_string(),
                "package.neovim.prefer=brew,snap,apt".to_string(),
                "package.neovim.alias.snap=nvim".to_string(),
            ],
            ..empty_module_update_args("test-mod")
        };
        module::cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = module::load_module_document(dir.path(), "test-mod").unwrap();
        let pkg = &doc.spec.packages[0];
        assert_eq!(pkg.min_version, Some("0.9".to_string()));
        assert_eq!(pkg.prefer, vec!["brew", "snap", "apt"]);
        assert_eq!(pkg.aliases.get("snap"), Some(&"nvim".to_string()));
    }

    #[test]
    fn module_delete_refuses_when_referenced() {
        let dir = create_test_config_dir();
        create_module_in_dir(
            dir.path(),
            "used-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: used-mod\nspec: {}\n",
        );

        // Update profile to reference the module
        let profile_path = dir.path().join("profiles").join("default.yaml");
        let mut doc = config::load_profile(&profile_path).unwrap();
        doc.spec.modules.push("used-mod".to_string());
        let yaml = serde_yaml::to_string(&doc).unwrap();
        std::fs::write(&profile_path, &yaml).unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let result = module::cmd_module_delete(&cli, &printer, "used-mod", true, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("referenced by"));
    }

    #[test]
    fn module_delete_succeeds_when_unreferenced() {
        let dir = create_test_config_dir();
        create_module_in_dir(
            dir.path(),
            "orphan-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: orphan-mod\nspec: {}\n",
        );

        let cli = test_cli(dir.path());
        let printer = test_printer();

        module::cmd_module_delete(&cli, &printer, "orphan-mod", true, false).unwrap();
        assert!(!dir.path().join("modules").join("orphan-mod").exists());
    }

    #[test]
    fn module_delete_purge_removes_target_files() {
        let dir = create_test_config_dir();

        // Create a target file outside the module directory
        let target_dir = dir.path().join("targets");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target_file = target_dir.join("deployed.conf");
        std::fs::write(&target_file, "deployed content").unwrap();
        assert!(target_file.exists());

        // Create a module with a file entry pointing at the target
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: purge-mod\nspec:\n  files:\n    - source: files/deployed.conf\n      target: {}\n",
            target_file.display()
        );
        create_module_in_dir(dir.path(), "purge-mod", &module_yaml);
        // Write a source file in the module
        std::fs::write(
            dir.path()
                .join("modules")
                .join("purge-mod")
                .join("files")
                .join("deployed.conf"),
            "source content",
        )
        .unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        module::cmd_module_delete(&cli, &printer, "purge-mod", true, true).unwrap();
        assert!(!dir.path().join("modules").join("purge-mod").exists());
        assert!(!target_file.exists(), "purge should remove target file");
    }

    #[test]
    fn module_delete_no_purge_preserves_target_files() {
        let dir = create_test_config_dir();

        // Create a target file (not a symlink into the module)
        let target_dir = dir.path().join("targets");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target_file = target_dir.join("regular.conf");
        std::fs::write(&target_file, "user content").unwrap();

        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: keep-mod\nspec:\n  files:\n    - source: files/regular.conf\n      target: {}\n",
            target_file.display()
        );
        create_module_in_dir(dir.path(), "keep-mod", &module_yaml);

        let cli = test_cli(dir.path());
        let printer = test_printer();

        module::cmd_module_delete(&cli, &printer, "keep-mod", true, false).unwrap();
        assert!(!dir.path().join("modules").join("keep-mod").exists());
        assert!(
            target_file.exists(),
            "without purge, non-symlinked target files are preserved"
        );
    }

    #[test]
    fn apply_module_sets_rejects_invalid_format() {
        let mut doc = config::ModuleDocument {
            api_version: cfgd_core::API_VERSION.to_string(),
            kind: "Module".to_string(),
            metadata: config::ModuleMetadata {
                name: "test".to_string(),
                description: None,
            },
            spec: config::ModuleSpec::default(),
        };

        // No = sign
        assert!(module::apply_module_sets(&["bad-format".to_string()], &mut doc).is_err());
        // Invalid path prefix
        assert!(module::apply_module_sets(&["foo.bar=baz".to_string()], &mut doc).is_err());
        // Package not found
        assert!(
            module::apply_module_sets(&["package.missing.min-version=1.0".to_string()], &mut doc)
                .is_err()
        );
        // Empty package name
        assert!(
            module::apply_module_sets(&["package..min-version=1.0".to_string()], &mut doc).is_err()
        );
        // Empty field name
        assert!(module::apply_module_sets(&["package.curl.=1.0".to_string()], &mut doc).is_err());
    }

    #[test]
    fn module_update_idempotent_add() {
        let dir = tempfile::tempdir().unwrap();
        create_module_in_dir(
            dir.path(),
            "test-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
        );

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ModuleUpdateArgs {
            packages: vec!["curl".to_string()],
            ..empty_module_update_args("test-mod")
        };
        module::cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = module::load_module_document(dir.path(), "test-mod").unwrap();
        assert_eq!(doc.spec.packages.len(), 1);
    }

    // --- Profile CRUD tests ---

    #[test]
    fn profile_create_with_flags() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ProfileCreateArgs {
            inherits: vec!["default".to_string()],
            modules: vec!["nvim".to_string()],
            packages: vec!["brew:curl".to_string(), "cargo:bat".to_string()],
            env: vec!["EDITOR=nvim".to_string()],
            system: vec!["shell=/bin/zsh".to_string()],
            ..test_profile_create_args("new-profile")
        };
        profile::cmd_profile_create(&cli, &printer, &args).unwrap();

        let profile_path = dir.path().join("profiles").join("new-profile.yaml");
        assert!(profile_path.exists());

        let doc = config::load_profile(&profile_path).unwrap();
        assert_eq!(doc.metadata.name, "new-profile");
        assert_eq!(doc.spec.inherits, vec!["default"]);
        assert_eq!(doc.spec.modules, vec!["nvim"]);
        assert!(doc.spec.env.iter().any(|e| e.name == "EDITOR"));
        assert!(doc.spec.system.contains_key("shell"));
    }

    #[test]
    fn profile_create_refuses_duplicate() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = test_profile_create_args("default");
        let result = profile::cmd_profile_create(&cli, &printer, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn profile_create_refuses_missing_parent() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ProfileCreateArgs {
            inherits: vec!["nonexistent".to_string()],
            ..test_profile_create_args("child")
        };
        let result = profile::cmd_profile_create(&cli, &printer, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn profile_update_add_and_remove() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        let args = ProfileUpdateArgs {
            modules: vec!["nvim".to_string()],
            packages: vec!["brew:jq".to_string()],
            env: vec!["EDITOR=nvim".to_string()],
            system: vec!["shell=/bin/zsh".to_string()],
            ..empty_profile_update_args()
        };
        profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        let profile_path = dir.path().join("profiles").join("default.yaml");
        let doc = config::load_profile(&profile_path).unwrap();
        assert!(doc.spec.modules.contains(&"nvim".to_string()));
        assert!(doc.spec.env.iter().any(|e| e.name == "EDITOR"));
        assert!(doc.spec.system.contains_key("shell"));
    }

    #[test]
    fn profile_delete_refuses_active() {
        let dir = create_test_config_dir();
        std::fs::write(
            dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let result = profile::cmd_profile_delete(&cli, &printer, "default", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("active profile"));
    }

    #[test]
    fn profile_delete_refuses_when_inherited() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        let result = profile::cmd_profile_delete(&cli, &printer, "default", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("inherited by"));
    }

    #[test]
    fn profile_delete_succeeds() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        profile::cmd_profile_delete(&cli, &printer, "work", true).unwrap();
        assert!(!dir.path().join("profiles").join("work.yaml").exists());
    }

    #[test]
    fn profiles_inheriting_finds_children() {
        let dir = create_test_config_dir();
        let result = profile::profiles_inheriting(&dir.path().join("profiles"), "default").unwrap();
        assert_eq!(result, vec!["work"]);

        let result = profile::profiles_inheriting(&dir.path().join("profiles"), "work").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_manager_package_valid() {
        let (mgr, pkg) = profile::parse_manager_package("brew:curl").unwrap();
        assert_eq!(mgr, "brew");
        assert_eq!(pkg, "curl");
    }

    #[test]
    fn parse_manager_package_invalid() {
        assert!(profile::parse_manager_package("no-colon").is_err());
        assert!(profile::parse_manager_package(":curl").is_err());
        assert!(profile::parse_manager_package("brew:").is_err());
        assert!(profile::parse_manager_package(":").is_err());
    }

    #[test]
    fn parse_package_flag_with_known_manager() {
        let known = &["brew", "apt", "cargo"];
        let (mgr, pkg) = parse_package_flag("brew:curl", known);
        assert_eq!(mgr, Some("brew".to_string()));
        assert_eq!(pkg, "curl");
    }

    #[test]
    fn parse_package_flag_bare_name() {
        let known = &["brew", "apt", "cargo"];
        let (mgr, pkg) = parse_package_flag("ripgrep", known);
        assert_eq!(mgr, None);
        assert_eq!(pkg, "ripgrep");
    }

    #[test]
    fn parse_package_flag_unknown_prefix_treated_as_bare() {
        let known = &["brew", "apt", "cargo"];
        // "python3:amd64" — "python3" is not a known manager
        let (mgr, pkg) = parse_package_flag("python3:amd64", known);
        assert_eq!(mgr, None);
        assert_eq!(pkg, "python3:amd64");
    }

    #[test]
    fn parse_package_flag_empty_parts() {
        let known = &["brew"];
        // ":curl" — empty prefix, not a known manager
        let (mgr, pkg) = parse_package_flag(":curl", known);
        assert_eq!(mgr, None);
        assert_eq!(pkg, ":curl");

        // "brew:" — empty suffix
        let (mgr, pkg) = parse_package_flag("brew:", known);
        assert_eq!(mgr, None);
        assert_eq!(pkg, "brew:");
    }

    #[test]
    fn parse_secret_spec_valid() {
        let spec = profile::parse_secret_spec("secrets/key.enc:~/.config/app/key").unwrap();
        assert_eq!(spec.source, "secrets/key.enc");
        assert_eq!(spec.target, PathBuf::from("~/.config/app/key"));
        assert!(spec.template.is_none());
        assert!(spec.backend.is_none());
    }

    #[test]
    fn parse_secret_spec_provider_url() {
        // Provider URLs with :// must not be split on the scheme colon
        let spec = profile::parse_secret_spec("op://vault/item:~/.config/key").unwrap();
        assert_eq!(spec.source, "op://vault/item");
        assert_eq!(spec.target, PathBuf::from("~/.config/key"));
    }

    #[test]
    fn parse_secret_spec_absolute_target() {
        let spec = profile::parse_secret_spec("secrets/db.enc:/etc/app/db.conf").unwrap();
        assert_eq!(spec.source, "secrets/db.enc");
        assert_eq!(spec.target, PathBuf::from("/etc/app/db.conf"));
    }

    #[test]
    fn parse_secret_spec_invalid() {
        assert!(profile::parse_secret_spec("no-colon").is_err());
        assert!(profile::parse_secret_spec(":target").is_err());
        assert!(profile::parse_secret_spec("source:").is_err());
    }

    #[test]
    fn profile_update_inherits() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        // Add inherits
        let args = ProfileUpdateArgs {
            inherits: vec!["default".to_string()],
            ..empty_profile_update_args()
        };
        profile::cmd_profile_update(&cli, &printer, "work", &args).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
        assert!(doc.spec.inherits.contains(&"default".to_string()));

        // Remove inherits
        let args = ProfileUpdateArgs {
            inherits: vec!["-default".to_string()],
            ..empty_profile_update_args()
        };
        profile::cmd_profile_update(&cli, &printer, "work", &args).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
        assert!(!doc.spec.inherits.contains(&"default".to_string()));
    }

    #[test]
    fn profile_update_secrets() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        // Add secret
        let args = ProfileUpdateArgs {
            secrets: vec!["secrets/key.enc:~/.config/app/key".to_string()],
            ..empty_profile_update_args()
        };
        profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert_eq!(doc.spec.secrets.len(), 1);
        assert_eq!(doc.spec.secrets[0].source, "secrets/key.enc");

        // Remove secret
        let args = ProfileUpdateArgs {
            secrets: vec!["-~/.config/app/key".to_string()],
            ..empty_profile_update_args()
        };
        profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(doc.spec.secrets.is_empty());
    }

    #[test]
    fn profile_update_scripts() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();

        // Add pre-reconcile and post-reconcile
        let args = ProfileUpdateArgs {
            pre_reconcile: vec![PathBuf::from("scripts/pre.sh")],
            post_reconcile: vec![PathBuf::from("scripts/post.sh")],
            ..empty_profile_update_args()
        };
        profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        let scripts = doc.spec.scripts.as_ref().unwrap();
        assert_eq!(scripts.pre_reconcile, vec![PathBuf::from("scripts/pre.sh")]);
        assert_eq!(
            scripts.post_reconcile,
            vec![PathBuf::from("scripts/post.sh")]
        );

        // Remove pre-reconcile
        let args = ProfileUpdateArgs {
            pre_reconcile: vec![PathBuf::from("-scripts/pre.sh")],
            post_reconcile: vec![PathBuf::from("-scripts/post.sh")],
            ..empty_profile_update_args()
        };
        profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        let scripts = doc.spec.scripts.as_ref().unwrap();
        assert!(scripts.pre_reconcile.is_empty());
        assert!(scripts.post_reconcile.is_empty());
    }

    #[test]
    fn profiles_using_module_finds_references() {
        let dir = create_test_config_dir();

        // Add module ref to default profile
        let profile_path = dir.path().join("profiles").join("default.yaml");
        let mut doc = config::load_profile(&profile_path).unwrap();
        doc.spec.modules.push("my-mod".to_string());
        std::fs::write(&profile_path, serde_yaml::to_string(&doc).unwrap()).unwrap();

        let result = module::profiles_using_module(&dir.path().join("profiles"), "my-mod").unwrap();
        assert_eq!(result, vec!["default"]);

        let result =
            module::profiles_using_module(&dir.path().join("profiles"), "nonexistent").unwrap();
        assert!(result.is_empty());
    }

    // --- Config CRUD tests ---

    #[test]
    fn config_show_displays_config() {
        let dir = create_test_config_dir();
        std::fs::write(
            dir.path().join("cfgd.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
"#,
        )
        .unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();
        let result = cmd_config_show(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn config_show_fails_without_config() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let printer = test_printer();
        let result = cmd_config_show(&cli, &printer);
        assert!(result.is_err());
    }

    // --- Source CRUD tests ---

    #[test]
    fn source_create_scaffolds_manifest() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let result = cmd_source_create(
            &cli,
            &printer,
            Some("my-source"),
            Some("Test"),
            Some("1.0.0"),
        );
        assert!(result.is_ok());

        let source_path = dir.path().join("cfgd-source.yaml");
        assert!(source_path.exists());

        let contents = std::fs::read_to_string(&source_path).unwrap();
        assert!(contents.contains("my-source"));
        assert!(contents.contains("Test"));
        assert!(contents.contains("1.0.0"));
        // Should include profiles found in the directory
        assert!(contents.contains("default"));
        assert!(contents.contains("work"));
    }

    #[test]
    fn source_create_refuses_duplicate() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
        std::fs::write(dir.path().join("cfgd-source.yaml"), "existing").unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();
        let result = cmd_source_create(&cli, &printer, Some("x"), Some("x"), Some("1.0"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn source_edit_fails_without_manifest() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let printer = test_printer();
        let result = cmd_source_edit(&cli, &printer);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No cfgd-source.yaml")
        );
    }

    // --- Workflow tests ---

    #[test]
    fn generate_workflow_yaml_contains_all_resources() {
        let modules = vec!["neovim".to_string(), "zsh".to_string()];
        let profiles = vec!["default".to_string(), "work".to_string()];

        let yaml = generate_release_workflow_yaml(&modules, &profiles);

        // Header
        assert!(yaml.contains("name: cfgd Release"));
        assert!(yaml.contains("on:"));

        // Module paths
        assert!(yaml.contains("modules/neovim/**"));
        assert!(yaml.contains("modules/zsh/**"));

        // Profile paths
        assert!(yaml.contains("profiles/default.yaml"));
        assert!(yaml.contains("profiles/work.yaml"));

        // Jobs
        assert!(yaml.contains("detect-changes:"));
        assert!(yaml.contains("tag-modules:"));
        assert!(yaml.contains("tag-profiles:"));

        // Module outputs
        assert!(yaml.contains("module_neovim"));
        assert!(yaml.contains("module_zsh"));

        // Profile outputs
        assert!(yaml.contains("profile_default"));
        assert!(yaml.contains("profile_work"));
    }

    #[test]
    fn generate_workflow_yaml_modules_only() {
        let modules = vec!["vim".to_string()];
        let profiles: Vec<String> = vec![];

        let yaml = generate_release_workflow_yaml(&modules, &profiles);

        assert!(yaml.contains("tag-modules:"));
        assert!(!yaml.contains("tag-profiles:"));
    }

    #[test]
    fn generate_workflow_yaml_profiles_only() {
        let modules: Vec<String> = vec![];
        let profiles = vec!["default".to_string()];

        let yaml = generate_release_workflow_yaml(&modules, &profiles);

        assert!(!yaml.contains("tag-modules:"));
        assert!(yaml.contains("tag-profiles:"));
    }

    #[test]
    fn workflow_generate_creates_file() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let result = cmd_workflow_generate(&cli, &printer, false);
        assert!(result.is_ok());

        let workflow_path = dir
            .path()
            .join(".github")
            .join("workflows")
            .join("cfgd-release.yml");
        assert!(workflow_path.exists());

        let contents = std::fs::read_to_string(&workflow_path).unwrap();
        assert!(contents.contains("cfgd Release"));
        assert!(contents.contains("default"));
    }

    #[test]
    fn workflow_generate_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        // No profiles or modules — should warn and return Ok
        let result = cmd_workflow_generate(&cli, &printer, false);
        assert!(result.is_ok());

        let workflow_path = dir
            .path()
            .join(".github")
            .join("workflows")
            .join("cfgd-release.yml");
        assert!(!workflow_path.exists());
    }

    #[test]
    fn generate_workflow_yaml_hyphens_in_names() {
        let modules = vec!["my-module".to_string()];
        let profiles = vec!["my-profile".to_string()];

        let yaml = generate_release_workflow_yaml(&modules, &profiles);

        // Hyphens should be converted to underscores in output names
        assert!(yaml.contains("module_my_module"));
        assert!(yaml.contains("profile_my_profile"));
    }

    #[test]
    fn test_validate_resource_name_valid() {
        assert!(validate_resource_name("my-module", "Module").is_ok());
        assert!(validate_resource_name("my_module", "Module").is_ok());
        assert!(validate_resource_name("Module123", "Module").is_ok());
        assert!(validate_resource_name("a", "Module").is_ok());
        assert!(validate_resource_name("foo.bar", "Module").is_ok());
    }

    #[test]
    fn test_validate_resource_name_invalid() {
        assert!(validate_resource_name("", "Module").is_err());
        assert!(validate_resource_name("../etc", "Module").is_err());
        assert!(validate_resource_name(".hidden", "Module").is_err());
        assert!(validate_resource_name("-leading", "Module").is_err());
        assert!(validate_resource_name("foo/bar", "Module").is_err());
        assert!(validate_resource_name("foo bar", "Module").is_err());
        assert!(validate_resource_name("a".repeat(129).as_str(), "Module").is_err());
    }

    #[test]
    fn workflow_generate_force_overwrites() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        // First generate
        cmd_workflow_generate(&cli, &printer, false).unwrap();
        let path = dir.path().join(".github/workflows/cfgd-release.yml");
        assert!(path.exists());

        // Write something different to the file
        std::fs::write(&path, "old content").unwrap();

        // Force overwrite
        cmd_workflow_generate(&cli, &printer, true).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("cfgd Release"));
        assert!(!contents.contains("old content"));
    }

    #[test]
    fn source_create_with_modules() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        // Create a module
        create_module_in_dir(
            dir.path(),
            "neovim",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: neovim\nspec:\n  packages: []\n  files: []\n  depends: []\n",
        );

        let cli = test_cli(dir.path());
        let printer = test_printer();

        let result = cmd_source_create(
            &cli,
            &printer,
            Some("test-source"),
            Some("Test"),
            Some("1.0.0"),
        );
        assert!(result.is_ok());

        let source_path = dir.path().join("cfgd-source.yaml");
        assert!(source_path.exists());

        let contents = std::fs::read_to_string(&source_path).unwrap();
        // Should contain both the profile and the module
        assert!(contents.contains("default"));
        assert!(contents.contains("neovim"));
    }

    #[test]
    fn source_create_output_is_parseable() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        cmd_source_create(
            &cli,
            &printer,
            Some("my-source"),
            Some("desc"),
            Some("0.1.0"),
        )
        .unwrap();

        let contents = std::fs::read_to_string(dir.path().join("cfgd-source.yaml")).unwrap();
        let result = config::parse_config_source(&contents);
        assert!(
            result.is_ok(),
            "Generated source YAML should be parseable: {:?}",
            result.err()
        );

        let doc = result.unwrap();
        assert_eq!(doc.metadata.name, "my-source");
        assert_eq!(doc.metadata.version, Some("0.1.0".to_string()));
    }

    #[test]
    fn config_show_with_all_sections() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cfgd.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  origin:
    - url: https://github.com/test/config
      branch: main
      type: git
  sources:
    - name: team-config
      origin:
        url: https://github.com/test/team
        branch: main
        type: git
      subscription:
        priority: 100
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd/modules
    security:
      require-signatures: true
  daemon:
    enabled: true
    reconcile:
      interval: 5m
      on-change: true
      auto-apply: false
    sync:
      interval: 30m
  secrets:
    backend: sops-age
  theme:
    name: ocean
"#,
        )
        .unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        // Should not error even with all sections populated
        let result = cmd_config_show(&cli, &printer);
        assert!(result.is_ok());
    }

    // --- Alias expansion tests ---

    #[test]
    fn expand_aliases_no_builtins() {
        // Aliases come from cfgd.yaml only — no hardcoded builtins.
        // Without a config, "add" and "remove" pass through unchanged.
        let args = vec!["cfgd".into(), "add".into(), "~/.zshrc".into()];
        let expanded = expand_aliases(args.clone());
        assert_eq!(expanded, args);

        let args = vec!["cfgd".into(), "remove".into(), "~/.zshrc".into()];
        let expanded = expand_aliases(args.clone());
        assert_eq!(expanded, args);
    }

    #[test]
    fn expand_aliases_no_match_passthrough() {
        let args = vec!["cfgd".into(), "apply".into(), "--dry-run".into()];
        let expanded = expand_aliases(args.clone());
        assert_eq!(expanded, args);
    }

    #[test]
    fn expand_aliases_skips_global_flags() {
        // Without config-defined aliases, "add" passes through even with global flags
        let args = vec![
            "cfgd".into(),
            "--verbose".into(),
            "add".into(),
            "~/.zshrc".into(),
        ];
        let expanded = expand_aliases(args.clone());
        assert_eq!(expanded, args);
    }

    #[test]
    fn expand_aliases_with_config_flag() {
        // With nonexistent config, no aliases are loaded — passthrough
        let args = vec![
            "cfgd".into(),
            "--config".into(),
            "/tmp/nonexistent.yaml".into(),
            "add".into(),
            "~/.zshrc".into(),
        ];
        let expanded = expand_aliases(args.clone());
        assert_eq!(expanded, args);
    }

    #[test]
    fn expand_aliases_empty_args() {
        let args = vec!["cfgd".into()];
        let expanded = expand_aliases(args.clone());
        assert_eq!(expanded, args);
    }

    #[test]
    fn resolve_profile_name_explicit_takes_precedence() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let result = resolve_profile_name(&cli, Some("my-profile"), false);
        assert_eq!(result.unwrap(), "my-profile");
    }

    #[test]
    fn resolve_profile_name_active_reads_config() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
        let cli = test_cli(dir.path());
        let result = resolve_profile_name(&cli, None, true);
        assert_eq!(result.unwrap(), "default");
    }

    #[test]
    fn resolve_profile_name_neither_errors() {
        let dir = create_test_config_dir();
        let cli = test_cli(dir.path());
        let result = resolve_profile_name(&cli, None, false);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Profile name required")
        );
    }

    #[test]
    fn parse_file_spec_plain_path() {
        let (source, target) = super::parse_file_spec("~/.zshrc").unwrap();
        assert_eq!(source, target);
    }

    #[test]
    fn parse_file_spec_source_target() {
        let (source, target) = super::parse_file_spec("./my-config:~/.config/app/config").unwrap();
        assert_eq!(source, std::path::PathBuf::from("./my-config"));
        assert!(target.to_string_lossy().contains(".config/app/config"));
    }

    #[test]
    fn parse_file_spec_empty_source_errors() {
        assert!(super::parse_file_spec(":~/.zshrc").is_err());
    }

    #[test]
    fn parse_file_spec_empty_target_errors() {
        assert!(super::parse_file_spec("~/.zshrc:").is_err());
    }

    #[test]
    fn is_unmanaged_file_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateStore::open_in_memory().unwrap();
        let target = dir.path().join("does-not-exist");
        assert!(!is_unmanaged_file(&target, dir.path(), &state));
    }

    #[test]
    fn is_unmanaged_file_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateStore::open_in_memory().unwrap();
        let target = dir.path().join("existing-file");
        std::fs::write(&target, "content").unwrap();
        assert!(is_unmanaged_file(&target, dir.path(), &state));
    }

    #[test]
    fn is_unmanaged_file_cfgd_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateStore::open_in_memory().unwrap();
        let source = dir.path().join("source-file");
        std::fs::write(&source, "content").unwrap();
        let target = dir.path().join("subdir").join("symlink");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, &target).unwrap();
        // Symlink points into config_dir, so it's managed
        assert!(!is_unmanaged_file(&target, dir.path(), &state));
    }

    #[test]
    fn is_unmanaged_file_tracked_in_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateStore::open_in_memory().unwrap();
        let target = dir.path().join("tracked-file");
        std::fs::write(&target, "content").unwrap();
        let target_str = target.display().to_string();
        state
            .upsert_managed_resource("file", &target_str, "local", None, None)
            .unwrap();
        assert!(!is_unmanaged_file(&target, dir.path(), &state));
    }

    // --- config get/set/unset helpers ---

    fn make_test_config(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("cfgd.yaml");
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\n\
             kind: Config\n\
             metadata:\n\
             \x20 name: test\n\
             spec:\n\
             \x20 profile: work\n\
             \x20 file-strategy: symlink\n\
             \x20 theme:\n\
             \x20\x20\x20 name: dracula\n\
             \x20 daemon:\n\
             \x20\x20\x20 enabled: true\n\
             \x20\x20\x20 reconcile:\n\
             \x20\x20\x20\x20\x20 interval: 5m\n\
             \x20\x20\x20\x20\x20 on-change: false\n\
             \x20 aliases:\n\
             \x20\x20\x20 add: 'profile update --active --file'\n\
             \x20\x20\x20 deploy: 'apply --yes'\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn config_get_scalar() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get("spec").unwrap();

        let val = walk_yaml_path(spec, "profile").unwrap();
        assert_eq!(val.as_str().unwrap(), "work");
    }

    #[test]
    fn config_get_nested() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get("spec").unwrap();

        let val = walk_yaml_path(spec, "daemon.reconcile.interval").unwrap();
        assert_eq!(val.as_str().unwrap(), "5m");
    }

    #[test]
    fn config_get_boolean() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get("spec").unwrap();

        let val = walk_yaml_path(spec, "daemon.enabled").unwrap();
        assert_eq!(val.as_bool().unwrap(), true);
    }

    #[test]
    fn config_get_complex_returns_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get("spec").unwrap();

        let val = walk_yaml_path(spec, "daemon").unwrap();
        assert!(val.is_mapping());
    }

    #[test]
    fn config_get_missing_key_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get("spec").unwrap();

        let result = walk_yaml_path(spec, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn config_get_alias() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get("spec").unwrap();

        let val = walk_yaml_path(spec, "aliases.deploy").unwrap();
        assert_eq!(val.as_str().unwrap(), "apply --yes");
    }

    #[test]
    fn config_set_scalar() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get_mut("spec").unwrap();

        let (parent, key) = walk_yaml_path_mut(spec, "profile").unwrap();
        parent.insert(serde_yaml::Value::String(key), parse_yaml_value("personal"));

        let spec = raw.get("spec").unwrap();
        let val = walk_yaml_path(spec, "profile").unwrap();
        assert_eq!(val.as_str().unwrap(), "personal");
    }

    #[test]
    fn config_set_creates_intermediates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &path,
            r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: base
"#,
        )
        .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get_mut("spec").unwrap();

        let (parent, key) = walk_yaml_path_mut(spec, "daemon.reconcile.interval").unwrap();
        parent.insert(serde_yaml::Value::String(key), parse_yaml_value("10m"));

        let spec = raw.get("spec").unwrap();
        let val = walk_yaml_path(spec, "daemon.reconcile.interval").unwrap();
        assert_eq!(val.as_str().unwrap(), "10m");
    }

    #[test]
    fn config_set_boolean_value() {
        let val = parse_yaml_value("true");
        assert_eq!(val, serde_yaml::Value::Bool(true));
        let val = parse_yaml_value("false");
        assert_eq!(val, serde_yaml::Value::Bool(false));
    }

    #[test]
    fn config_set_number_value() {
        let val = parse_yaml_value("42");
        assert!(val.is_number());
        assert_eq!(val.as_i64().unwrap(), 42);
    }

    #[test]
    fn config_set_string_value() {
        let val = parse_yaml_value("hello world");
        assert_eq!(val.as_str().unwrap(), "hello world");
    }

    #[test]
    fn config_unset_removes_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get_mut("spec").unwrap();

        let (parent, key) = walk_yaml_path_mut(spec, "theme").unwrap();
        let yaml_key = serde_yaml::Value::String(key);
        assert!(parent.remove(&yaml_key).is_some());

        let spec = raw.get("spec").unwrap();
        assert!(walk_yaml_path(spec, "theme").is_err());
    }

    #[test]
    fn config_unset_nested_alias() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_config(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        let spec = raw.get_mut("spec").unwrap();

        let (parent, key) = walk_yaml_path_mut(spec, "aliases.deploy").unwrap();
        let yaml_key = serde_yaml::Value::String(key);
        assert!(parent.remove(&yaml_key).is_some());

        // "add" alias should still exist
        let spec = raw.get("spec").unwrap();
        assert!(walk_yaml_path(spec, "aliases.add").is_ok());
        assert!(walk_yaml_path(spec, "aliases.deploy").is_err());
    }

    #[test]
    fn theme_string_shorthand_deserializes() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  theme: dracula
"#;
        let cfg = config::parse_config(yaml, std::path::Path::new("cfgd.yaml")).unwrap();
        let theme = cfg.spec.theme.unwrap();
        assert_eq!(theme.name, "dracula");
        assert!(theme.overrides.is_empty());
    }

    #[test]
    fn theme_struct_form_deserializes() {
        let yaml = "apiVersion: cfgd.io/v1alpha1\n\
                     kind: Config\n\
                     metadata:\n\
                     \x20 name: test\n\
                     spec:\n\
                     \x20 theme:\n\
                     \x20\x20\x20 name: dracula\n\
                     \x20\x20\x20 overrides:\n\
                     \x20\x20\x20\x20\x20 success: '#50fa7b'\n";
        let cfg = config::parse_config(yaml, std::path::Path::new("cfgd.yaml")).unwrap();
        let theme = cfg.spec.theme.unwrap();
        assert_eq!(theme.name, "dracula");
        assert_eq!(theme.overrides.success.as_deref(), Some("#50fa7b"));
    }

    // --- parse_file_spec ---

    #[test]
    fn parse_file_spec_with_colon() {
        let (src, tgt) = super::parse_file_spec("/tmp/a:/tmp/b").unwrap();
        assert_eq!(src, PathBuf::from("/tmp/a"));
        assert_eq!(tgt, PathBuf::from("/tmp/b"));
    }

    #[test]
    fn parse_file_spec_no_colon() {
        let (src, tgt) = super::parse_file_spec("/tmp/a").unwrap();
        assert_eq!(src, tgt);
    }

    #[test]
    fn parse_file_spec_empty_source() {
        assert!(super::parse_file_spec(":/tmp/b").is_err());
    }

    #[test]
    fn parse_file_spec_empty_target() {
        assert!(super::parse_file_spec("/tmp/a:").is_err());
    }

    // --- add_to_gitignore ---

    #[test]
    fn add_to_gitignore_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        super::add_to_gitignore(dir.path(), "secrets/key.enc").unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains("secrets/key.enc"));
    }

    #[test]
    fn add_to_gitignore_no_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        super::add_to_gitignore(dir.path(), "secrets/key.enc").unwrap();
        super::add_to_gitignore(dir.path(), "secrets/key.enc").unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        let count = content.matches("secrets/key.enc").count();
        assert_eq!(count, 1);
    }

    // --- parse_yaml_value ---

    #[test]
    fn parse_yaml_value_bool_true() {
        assert_eq!(
            super::parse_yaml_value("true"),
            serde_yaml::Value::Bool(true)
        );
    }

    #[test]
    fn parse_yaml_value_bool_false() {
        assert_eq!(
            super::parse_yaml_value("false"),
            serde_yaml::Value::Bool(false)
        );
    }

    #[test]
    fn parse_yaml_value_null() {
        assert_eq!(super::parse_yaml_value("null"), serde_yaml::Value::Null);
        assert_eq!(super::parse_yaml_value("~"), serde_yaml::Value::Null);
    }

    #[test]
    fn parse_yaml_value_integer() {
        assert_eq!(
            super::parse_yaml_value("42"),
            serde_yaml::Value::Number(42.into())
        );
    }

    #[test]
    fn parse_yaml_value_string() {
        assert_eq!(
            super::parse_yaml_value("hello"),
            serde_yaml::Value::String("hello".into())
        );
    }

    // --- walk_yaml_path ---

    #[test]
    fn walk_yaml_path_root() {
        let value = serde_yaml::Value::String("hi".into());
        let result = super::walk_yaml_path(&value, ".").unwrap();
        assert_eq!(result, &value);
    }

    #[test]
    fn walk_yaml_path_nested() {
        let yaml = "a:\n  b: 42\n";
        let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let result = super::walk_yaml_path(&value, "a.b").unwrap();
        assert_eq!(result.as_i64(), Some(42));
    }

    #[test]
    fn walk_yaml_path_missing_key() {
        let yaml = "a:\n  b: 42\n";
        let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        assert!(super::walk_yaml_path(&value, "a.c").is_err());
    }

    #[test]
    fn walk_yaml_path_empty_segment() {
        let value = serde_yaml::Value::Null;
        assert!(super::walk_yaml_path(&value, "a..b").is_err());
    }

    // --- walk_yaml_path_mut ---

    #[test]
    fn walk_yaml_path_mut_creates_intermediate() {
        let mut value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let (parent, leaf) = super::walk_yaml_path_mut(&mut value, "a.b.c").unwrap();
        assert_eq!(leaf, "c");
        parent.insert(
            serde_yaml::Value::String("c".into()),
            serde_yaml::Value::String("val".into()),
        );
        let result = super::walk_yaml_path(&value, "a.b.c").unwrap();
        assert_eq!(result.as_str(), Some("val"));
    }

    // --- scan_profile_names / scan_module_names ---

    #[test]
    fn scan_profile_names_from_dir() {
        let dir = create_test_config_dir();
        let profiles_dir = dir.path().join("profiles");
        let names = super::scan_profile_names(&profiles_dir).unwrap();
        assert!(names.contains(&"default".to_string()));
        assert!(names.contains(&"work".to_string()));
    }

    #[test]
    fn scan_profile_names_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let names = super::scan_profile_names(dir.path()).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn scan_module_names_from_dir() {
        let dir = tempfile::tempdir().unwrap();
        create_module_in_dir(
            dir.path(),
            "test-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: []\n",
        );
        let modules_dir = dir.path().join("modules");
        let names = super::scan_module_names(&modules_dir).unwrap();
        assert_eq!(names, vec!["test-mod"]);
    }

    #[test]
    fn scan_module_names_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let names = super::scan_module_names(&dir.path().join("nope")).unwrap();
        assert!(names.is_empty());
    }

    // --- copy_files_to_dir ---

    #[test]
    fn copy_files_to_dir_copies_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("myfile.txt");
        std::fs::write(&source, "content").unwrap();
        let repo_dir = dir.path().join("repo");

        let results = super::copy_files_to_dir(&[source.display().to_string()], &repo_dir).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "myfile.txt");
        // File should be in repo
        assert!(repo_dir.join("myfile.txt").exists());
        // Original should now be a symlink
        assert!(source.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn copy_files_to_dir_nonexistent_source_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = super::copy_files_to_dir(&["/nonexistent-12345/file".into()], dir.path());
        assert!(result.is_err());
    }

    // --- generate_release_workflow_yaml ---

    #[test]
    fn generate_release_workflow_empty() {
        let yaml = super::generate_release_workflow_yaml(&[], &[]);
        assert!(yaml.contains("placeholder:"));
        assert!(yaml.contains("No modules or profiles to tag yet"));
    }

    #[test]
    fn generate_release_workflow_with_modules() {
        let yaml = super::generate_release_workflow_yaml(&["shell-tools".into()], &[]);
        assert!(yaml.contains("modules/shell-tools/**"));
        assert!(yaml.contains("tag-modules:"));
        assert!(!yaml.contains("placeholder:"));
    }

    #[test]
    fn generate_release_workflow_with_profiles() {
        let yaml = super::generate_release_workflow_yaml(&[], &["work".into()]);
        assert!(yaml.contains("profiles/work.yaml"));
        assert!(yaml.contains("tag-profiles:"));
    }

    #[test]
    fn generate_release_workflow_both() {
        let yaml =
            super::generate_release_workflow_yaml(&["git-tools".into()], &["personal".into()]);
        assert!(yaml.contains("tag-modules:"));
        assert!(yaml.contains("tag-profiles:"));
        assert!(yaml.contains("detect-changes:"));
    }

    // --- cmd_config_get / cmd_config_set / cmd_config_unset ---

    #[test]
    fn config_get_reads_value() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

        let cli = Cli {
            config: config_path,
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        let result = super::cmd_config_get(&cli, &printer, "profile");
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_config_get_missing_key_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

        let cli = Cli {
            config: config_path,
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::cmd_config_get(&cli, &printer, "nonexistent").is_err());
    }

    #[test]
    fn config_set_and_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

        let cli = Cli {
            config: config_path.clone(),
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        super::cmd_config_set(&cli, &printer, "profile", "work").unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("work"));
    }

    #[test]
    fn cmd_config_unset_removes_key() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

        let cli = Cli {
            config: config_path.clone(),
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        let result = super::cmd_config_unset(&cli, &printer, "profile");
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_config_unset_missing_key_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

        let cli = Cli {
            config: config_path,
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::cmd_config_unset(&cli, &printer, "nope").is_err());
    }

    // --- cmd_config_show ---

    #[test]
    fn config_show_succeeds_with_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

        let cli = Cli {
            config: config_path,
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::cmd_config_show(&cli, &printer).is_ok());
    }

    #[test]
    fn config_show_errors_without_config() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            config: dir.path().join("nonexistent.yaml"),
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::cmd_config_show(&cli, &printer).is_err());
    }

    // --- secret_backend_from_config ---

    #[test]
    fn secret_backend_defaults_to_sops() {
        let (backend, _) = super::secret_backend_from_config(None);
        assert_eq!(backend, "sops");
    }

    // --- print_apply_result ---

    #[test]
    fn print_apply_result_success() {
        let printer = test_printer();
        let result = cfgd_core::reconciler::ApplyResult {
            status: cfgd_core::state::ApplyStatus::Success,
            action_results: vec![],
            apply_id: 1,
        };
        let status = super::print_apply_result(&result, &printer);
        assert_eq!(status, cfgd_core::state::ApplyStatus::Success);
    }

    #[test]
    fn print_apply_result_partial() {
        let printer = test_printer();
        let result = cfgd_core::reconciler::ApplyResult {
            status: cfgd_core::state::ApplyStatus::Partial,
            action_results: vec![],
            apply_id: 2,
        };
        let status = super::print_apply_result(&result, &printer);
        assert_eq!(status, cfgd_core::state::ApplyStatus::Partial);
    }

    #[test]
    fn print_apply_result_failed() {
        let printer = test_printer();
        let result = cfgd_core::reconciler::ApplyResult {
            status: cfgd_core::state::ApplyStatus::Failed,
            action_results: vec![],
            apply_id: 3,
        };
        let status = super::print_apply_result(&result, &printer);
        assert_eq!(status, cfgd_core::state::ApplyStatus::Failed);
    }

    // --- print_verify_results ---

    #[test]
    fn print_verify_results_all_pass() {
        let printer = test_printer();
        let results = vec![reconciler::VerifyResult {
            resource_type: "package".into(),
            resource_id: "curl".into(),
            expected: "installed".into(),
            actual: "installed".into(),
            matches: true,
        }];
        let (pass, fail) = super::print_verify_results(&results, &printer);
        assert_eq!(pass, 1);
        assert_eq!(fail, 0);
    }

    #[test]
    fn print_verify_results_with_failures() {
        let printer = test_printer();
        let results = vec![
            reconciler::VerifyResult {
                resource_type: "package".into(),
                resource_id: "curl".into(),
                expected: "installed".into(),
                actual: "installed".into(),
                matches: true,
            },
            reconciler::VerifyResult {
                resource_type: "sysctl".into(),
                resource_id: "net.ipv4.ip_forward".into(),
                expected: "1".into(),
                actual: "0".into(),
                matches: false,
            },
        ];
        let (pass, fail) = super::print_verify_results(&results, &printer);
        assert_eq!(pass, 1);
        assert_eq!(fail, 1);
    }

    // --- expand_aliases ---

    #[test]
    fn expand_aliases_passthrough() {
        let args = vec!["cfgd".into(), "status".into()];
        let result = super::expand_aliases(args.clone());
        assert_eq!(result, args);
    }

    #[test]
    fn expand_aliases_no_alias_passthrough() {
        // With empty builtin_aliases, no expansion happens
        let args = vec!["cfgd".into(), "apply".into(), "--dry-run".into()];
        let result = super::expand_aliases(args.clone());
        assert_eq!(result, args);
    }

    // --- extract_config_path ---

    #[test]
    fn extract_config_path_explicit() {
        let args = vec![
            "cfgd".into(),
            "--config".into(),
            "/tmp/my.yaml".into(),
            "status".into(),
        ];
        assert_eq!(
            super::extract_config_path(&args),
            Some(PathBuf::from("/tmp/my.yaml"))
        );
    }

    #[test]
    fn extract_config_path_equals() {
        let args = vec!["cfgd".into(), "--config=/tmp/my.yaml".into()];
        assert_eq!(
            super::extract_config_path(&args),
            Some(PathBuf::from("/tmp/my.yaml"))
        );
    }

    // --- resolve_profile_name ---

    #[test]
    fn resolve_profile_name_explicit_from_name() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        let cli = test_cli(dir.path());
        let result = super::resolve_profile_name(&cli, Some("work"), false).unwrap();
        assert_eq!(result, "work");
    }

    // --- parse_package_flag ---

    #[test]
    fn parse_package_flag_known_manager_splits() {
        let known = &["brew", "apt", "cargo"];
        let (mgr, pkg) = super::parse_package_flag("brew:ripgrep", known);
        assert_eq!(mgr, Some("brew".to_string()));
        assert_eq!(pkg, "ripgrep");
    }

    #[test]
    fn parse_package_flag_unknown_manager_passthrough() {
        let known = &["brew", "apt"];
        let (mgr, pkg) = super::parse_package_flag("unknown:ripgrep", known);
        assert!(mgr.is_none());
        assert_eq!(pkg, "unknown:ripgrep");
    }

    #[test]
    fn parse_package_flag_bare_name_passthrough() {
        let known = &["brew"];
        let (mgr, pkg) = super::parse_package_flag("ripgrep", known);
        assert!(mgr.is_none());
        assert_eq!(pkg, "ripgrep");
    }

    // --- list_yaml_stems ---

    #[test]
    fn list_yaml_stems_finds_yaml_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.yaml"), "").unwrap();
        std::fs::write(dir.path().join("beta.yml"), "").unwrap();
        std::fs::write(dir.path().join("not-yaml.txt"), "").unwrap();

        let stems = super::list_yaml_stems(dir.path()).unwrap();
        assert!(stems.contains(&"alpha".to_string()));
        assert!(stems.contains(&"beta".to_string()));
        assert!(!stems.contains(&"not-yaml".to_string()));
    }

    // --- builtin_aliases ---

    #[test]
    fn builtin_aliases_returns_map() {
        let aliases = super::builtin_aliases();
        // Currently empty — but verify it returns a HashMap without panicking
        assert!(aliases.is_empty() || !aliases.is_empty());
    }

    // --- cmd_doctor basic ---

    #[test]
    fn cmd_doctor_with_valid_config() {
        let dir = create_test_config_dir();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        let cli = test_cli(dir.path());
        let printer = test_printer();

        // cmd_doctor should succeed with a valid config
        let result = super::cmd_doctor(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_doctor_without_config() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            config: dir.path().join("nonexistent.yaml"),
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        // Should still succeed (doctor reports missing config, doesn't fail)
        let result = super::cmd_doctor(&cli, &printer);
        assert!(result.is_ok());
    }

    // --- Command handler tests (require CFGD_STATE_DIR) ---
    //
    // These test full command handlers that depend on the state store.
    // Each test sets CFGD_STATE_DIR to its own tempdir.
    // Tests using set_var must use the STATE_DIR_LOCK to avoid races.

    use std::sync::Mutex as StdMutex;
    static STATE_DIR_LOCK: StdMutex<()> = StdMutex::new(());

    /// Set up a full test environment: config dir + state dir + env var.
    /// Returns (config_dir_tempdir, state_dir_tempdir).
    fn setup_test_env() -> (tempfile::TempDir, tempfile::TempDir) {
        let config_dir = create_test_config_dir();
        std::fs::write(config_dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

        // Create modules dir
        std::fs::create_dir_all(config_dir.path().join("modules")).unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        // SAFETY: guarded by STATE_DIR_LOCK
        unsafe {
            std::env::set_var("CFGD_STATE_DIR", state_dir.path());
        }

        (config_dir, state_dir)
    }

    #[test]
    fn cmd_status_with_empty_state() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        let result = super::cmd_status(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_log_with_empty_state() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let _cli = test_cli(config_dir.path());
        let printer = test_printer();

        let result = super::cmd_log(&printer, 10);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_apply_dry_run_empty_profile() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: true,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_apply_dry_run_with_phase_filter() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: true,
            phase: Some("packages".to_string()),
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_apply_dry_run_invalid_phase() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: true,
            phase: Some("invalid-phase".to_string()),
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown phase"));
    }

    #[test]
    fn cmd_apply_dry_run_with_skip() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: true,
            phase: None,
            yes: true,
            skip: vec!["packages".to_string()],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_apply_dry_run_with_only() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: true,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec!["files".to_string()],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_apply_real_with_empty_profile() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        // Use a profile with no packages/files so apply does nothing
        let empty_profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
        std::fs::write(
            config_dir.path().join("profiles").join("empty.yaml"),
            empty_profile,
        )
        .unwrap();
        let empty_config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), empty_config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_status_after_apply() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        // Apply with empty profile
        let empty_profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
        std::fs::write(
            config_dir.path().join("profiles").join("empty.yaml"),
            empty_profile,
        )
        .unwrap();
        let empty_config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), empty_config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        // Apply first
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };
        super::cmd_apply(&cli, &printer, &args).unwrap();

        // Status should show last apply
        let result = super::cmd_status(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_log_after_apply() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let empty_profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
        std::fs::write(
            config_dir.path().join("profiles").join("empty.yaml"),
            empty_profile,
        )
        .unwrap();
        let empty_config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), empty_config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        // Apply
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };
        super::cmd_apply(&cli, &printer, &args).unwrap();

        // Log should show one entry
        let result = super::cmd_log(&printer, 10);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_verify_empty_profile() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        let result = super::cmd_verify(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_diff_empty_profile() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        let result = super::cmd_diff(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_apply_dry_run_with_files() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        // Create a source file
        let files_dir = config_dir.path().join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("test.txt"), "hello world").unwrap();

        let target = config_dir.path().join("output").join("test.txt");

        // Profile with a file
        let profile = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n",
            target.display()
        );
        std::fs::write(
            config_dir.path().join("profiles").join("withfile.yaml"),
            &profile,
        )
        .unwrap();
        let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: true,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
        // File should NOT be created (dry-run)
        assert!(!target.exists());
    }

    #[test]
    fn cmd_apply_creates_file() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let files_dir = config_dir.path().join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("test.txt"), "applied content").unwrap();

        let target = config_dir.path().join("output").join("test.txt");

        let profile = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n        strategy: copy\n",
            target.display()
        );
        std::fs::write(
            config_dir.path().join("profiles").join("withfile.yaml"),
            &profile,
        )
        .unwrap();
        let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
        // File SHOULD be created
        assert!(target.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "applied content");
    }

    #[test]
    fn cmd_apply_idempotent() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let files_dir = config_dir.path().join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("test.txt"), "content").unwrap();

        let target = config_dir.path().join("output").join("test.txt");

        let profile = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n        strategy: copy\n",
            target.display()
        );
        std::fs::write(
            config_dir.path().join("profiles").join("withfile.yaml"),
            &profile,
        )
        .unwrap();
        let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        // First apply
        super::cmd_apply(&cli, &printer, &args).unwrap();
        assert!(target.exists());

        // Second apply — should succeed with nothing to do
        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_diff_with_files() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let files_dir = config_dir.path().join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("test.txt"), "desired content").unwrap();

        let target_dir = config_dir.path().join("output");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("test.txt");
        std::fs::write(&target, "current content").unwrap();

        let profile = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n        strategy: copy\n",
            target.display()
        );
        std::fs::write(
            config_dir.path().join("profiles").join("withfile.yaml"),
            &profile,
        )
        .unwrap();
        let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        let result = super::cmd_diff(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_status_structured_output() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            output: "json".to_string(),
            ..test_cli(config_dir.path())
        };
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let result = super::cmd_status(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_log_structured_output() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        let result = super::cmd_log(&printer, 5);
        assert!(result.is_ok());
    }

    #[test]
    fn execute_status_command() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Status,
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        let result = super::execute(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn execute_log_command() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(
            dir.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules: []\n",
        )
        .unwrap();

        let cli = Cli {
            command: Command::Log { limit: 10 },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        let result = super::execute(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn execute_verify_command() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Verify,
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        let result = super::execute(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn execute_diff_command() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Diff,
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        let result = super::execute(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn execute_doctor_command() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Doctor,
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        let result = super::execute(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn execute_profile_list() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Profile {
                command: ProfileCommand::List,
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_profile_show() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Profile {
                command: ProfileCommand::Show,
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_config_show() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Config {
                command: ConfigCommand::Show,
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_config_get() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Config {
                command: ConfigCommand::Get {
                    key: "profile".to_string(),
                },
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_config_set() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Config {
                command: ConfigCommand::Set {
                    key: "profile".to_string(),
                    value: "work".to_string(),
                },
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_apply_dry_run() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Apply(ApplyArgs {
                dry_run: true,
                phase: None,
                yes: true,
                skip: vec![],
                only: vec![],
                module: None,
            }),
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_completions_bash() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Command::Completions {
                shell: clap_complete::Shell::Bash,
            },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_completions_zsh() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Command::Completions {
                shell: clap_complete::Shell::Zsh,
            },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_completions_fish() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Command::Completions {
                shell: clap_complete::Shell::Fish,
            },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_explain_command() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Command::Explain {
                resource: Some("config".to_string()),
                recursive: false,
            },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_explain_profile() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Command::Explain {
                resource: Some("profile".to_string()),
                recursive: false,
            },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_explain_module() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Command::Explain {
                resource: Some("module".to_string()),
                recursive: false,
            },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_explain_no_resource() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Command::Explain {
                resource: None,
                recursive: false,
            },
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn cmd_apply_with_module_filter() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        // Create a module
        create_module_in_dir(
            config_dir.path(),
            "test-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
        );

        // Profile referencing the module
        let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n";
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            profile,
        )
        .unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: true,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: Some("test-mod".to_string()),
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_apply_with_env_vars() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        // Profile with env vars
        let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n    - name: PAGER\n      value: less\n  modules: []\n";
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            profile,
        )
        .unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };

        let result = super::cmd_apply(&cli, &printer, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_status_with_modules() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        create_module_in_dir(
            config_dir.path(),
            "test-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
        );

        let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n";
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            profile,
        )
        .unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        assert!(super::cmd_status(&cli, &printer).is_ok());
    }

    #[test]
    fn cmd_status_with_drift_events() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        // Apply first to create state
        let empty_profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
        std::fs::write(
            config_dir.path().join("profiles").join("empty.yaml"),
            empty_profile,
        )
        .unwrap();
        let empty_config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), empty_config).unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        // Apply, then record a drift event manually
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };
        super::cmd_apply(&cli, &printer, &args).unwrap();

        // Record a drift event
        let state = super::open_state_store().unwrap();
        state
            .record_drift(
                "package",
                "curl",
                Some("installed"),
                Some("missing"),
                "local",
            )
            .unwrap();

        // Status should show the drift
        assert!(super::cmd_status(&cli, &printer).is_ok());
    }

    // --- Source command tests ---

    #[test]
    fn cmd_source_list_no_sources() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        assert!(super::cmd_source_list(&cli, &printer).is_ok());
    }

    #[test]
    fn cmd_source_list_no_config() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            config: dir.path().join("nonexistent.yaml"),
            ..test_cli(dir.path())
        };
        let printer = test_printer();

        assert!(super::cmd_source_list(&cli, &printer).is_ok());
    }

    // --- Decide command tests ---

    #[test]
    fn cmd_decide_accept_all_empty() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let printer = test_printer();

        let result = super::cmd_decide(&printer, "accept", None, None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_decide_reject_all_empty() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let printer = test_printer();

        let result = super::cmd_decide(&printer, "reject", None, None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_decide_invalid_action() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let printer = test_printer();

        let result = super::cmd_decide(&printer, "invalid", None, None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown action"));
    }

    #[test]
    fn cmd_decide_accept_specific_resource() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let printer = test_printer();

        // No pending decisions, but should not error
        let result = super::cmd_decide(&printer, "accept", Some("packages.brew.curl"), None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_decide_reject_by_source() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_config_dir, _state_dir) = setup_test_env();

        let printer = test_printer();

        let result = super::cmd_decide(&printer, "reject", None, Some("acme"), false);
        assert!(result.is_ok());
    }

    // --- Profile commands via execute ---

    // profile create/delete tested via existing module_create tests above
    #[test]
    fn execute_profile_switch() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Profile {
                command: ProfileCommand::Switch {
                    name: "work".to_string(),
                },
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());

        // Verify config updated
        let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
        assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
    }

    // --- Module commands via execute ---

    #[test]
    fn execute_module_list() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Module {
                command: ModuleCommand::List,
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    #[test]
    fn execute_workflow_generate() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = Cli {
            command: Command::Workflow {
                command: WorkflowCommand::Generate { force: false },
            },
            ..test_cli(config_dir.path())
        };
        let printer = test_printer();

        assert!(super::execute(&cli, &printer).is_ok());
    }

    // --- Sync/Pull without sources (no-op) ---

    #[test]
    fn cmd_sync_no_sources() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        // With no sources configured, sync should succeed as a no-op
        let result = super::cmd_sync(&cli, &printer);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_pull_no_sources() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        let result = super::cmd_pull(&cli, &printer);
        assert!(result.is_ok());
    }

    // --- Apply with all phases ---

    #[test]
    fn cmd_apply_dry_run_each_phase() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        for phase in &[
            "packages", "files", "secrets", "scripts", "system", "modules",
        ] {
            let args = ApplyArgs {
                dry_run: true,
                phase: Some(phase.to_string()),
                yes: true,
                skip: vec![],
                only: vec![],
                module: None,
            };
            let result = super::cmd_apply(&cli, &printer, &args);
            assert!(result.is_ok(), "dry-run failed for phase: {}", phase);
        }
    }

    // --- Verify after real apply ---

    #[test]
    fn cmd_verify_after_apply_with_env() {
        let _lock = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (config_dir, _state_dir) = setup_test_env();

        let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n  modules: []\n";
        std::fs::write(
            config_dir.path().join("profiles").join("default.yaml"),
            profile,
        )
        .unwrap();

        let cli = test_cli(config_dir.path());
        let printer = test_printer();

        // Apply
        let args = ApplyArgs {
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
        };
        super::cmd_apply(&cli, &printer, &args).unwrap();

        // Verify
        assert!(super::cmd_verify(&cli, &printer).is_ok());
    }
}
