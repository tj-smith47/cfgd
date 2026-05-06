mod apply;
mod checkin;
mod compliance;
mod config_cmd;
mod daemon;
mod decide;
mod diff;
mod doctor;
mod explain;
pub mod generate;
mod init;
mod kubectl;
mod log;
mod module;
mod output_types;
mod plan;
mod plan_ops;
pub mod plugin;
mod profile;
mod pull;
mod rollback;
mod secret;
mod source;
mod status;
mod sync;
mod upgrade;
mod verify;
mod workflow;

use output_types::*;
use plan_ops::*;
#[cfg(test)]
pub(in crate::cli) use source::{
    add_source_to_config, count_policy_items, display_policy_items, infer_source_name,
    remove_source_from_config,
};
pub(in crate::cli) use source::{
    build_permission_input, display_pending_decisions, mutate_config_yaml, source_cache_dir,
};
use workflow::{generate_release_workflow_yaml, maybe_update_workflow};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;

use crate::files::CfgdFileManager;
use crate::packages;
use crate::secrets;
use cfgd_core::composition::{self, CompositionInput, SubscriptionConfig};
use cfgd_core::config::{self, CfgdConfig, MergedProfile, ResolvedProfile};
use cfgd_core::modules;
use cfgd_core::output::Printer;
use cfgd_core::platform::Platform;
use cfgd_core::providers::{
    FileAction, PackageAction, ProviderRegistry, SecretAction, SecretBackend,
};
use cfgd_core::reconciler::{self, PhaseName, ReconcileContext, Reconciler};
use cfgd_core::sources::SourceManager;
use cfgd_core::state::StateStore;

const MSG_NO_CONFIG: &str = "No cfgd.yaml found — run 'cfgd init' first";
const MSG_RUN_APPLY: &str = "Run 'cfgd apply --dry-run' to preview changes, then 'cfgd apply'";
const MSG_NOTHING_TO_DO: &str = "Nothing to do — everything is up to date";

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

#[derive(Debug, Clone)]
pub struct OutputFormatArg(pub cfgd_core::output::OutputFormat);

impl std::str::FromStr for OutputFormatArg {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        use cfgd_core::output::OutputFormat;
        match s {
            "table" => Ok(Self(OutputFormat::Table)),
            "wide" => Ok(Self(OutputFormat::Wide)),
            "json" => Ok(Self(OutputFormat::Json)),
            "yaml" => Ok(Self(OutputFormat::Yaml)),
            "name" => Ok(Self(OutputFormat::Name)),
            other => {
                if let Some(expr) = other.strip_prefix("jsonpath=") {
                    Ok(Self(OutputFormat::Jsonpath(expr.to_string())))
                } else if let Some(tmpl) = other.strip_prefix("template=") {
                    Ok(Self(OutputFormat::Template(tmpl.to_string())))
                } else if let Some(path) = other.strip_prefix("template-file=") {
                    Ok(Self(OutputFormat::TemplateFile(std::path::PathBuf::from(
                        path,
                    ))))
                } else {
                    Err(format!(
                        "unknown output format '{}'. Valid: table, wide, json, yaml, name, jsonpath=EXPR, template=TMPL, template-file=PATH",
                        other
                    ))
                }
            }
        }
    }
}

impl From<OutputFormatArg> for clap::builder::OsStr {
    fn from(_: OutputFormatArg) -> clap::builder::OsStr {
        clap::builder::OsStr::from("table")
    }
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

    /// Verbose output (-v = debug, -vv = trace). Also accepts CFGD_VERBOSE as an on/off flag.
    #[arg(
        long,
        short,
        global = true,
        action = clap::ArgAction::Count,
        env = "CFGD_VERBOSE",
        conflicts_with = "quiet"
    )]
    pub verbose: u8,

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

    /// Output format: table, wide, json, yaml, name, jsonpath=EXPR, template=TMPL, template-file=PATH
    #[arg(long, short = 'o', global = true, default_value = "table")]
    pub output: OutputFormatArg,

    /// [DEPRECATED — use --output jsonpath=EXPR] JSONPath expression to extract from structured output
    #[arg(long, global = true, hide = true)]
    pub jsonpath: Option<String>,

    /// Override state directory (default: $CFGD_STATE_DIR or platform data dir)
    #[arg(long, global = true, env = "CFGD_STATE_DIR")]
    pub state_dir: Option<PathBuf>,

    // Optional so `cfgd` with no args prints help and exits 0. A required
    // subcommand (non-Option) makes clap emit a "usage" error and exit with
    // code 2, which package-manager validators (winget's, chocolatey's)
    // treat as install failure since they smoke-test the installed binary
    // with no args.
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Parser)]
pub struct ApplyArgs {
    /// Config source: git URL to clone, or local path to an existing config directory
    #[arg(long)]
    pub from: Option<String>,
    /// Preview changes without applying
    #[arg(long)]
    pub dry_run: bool,
    /// Apply only a specific phase
    #[arg(long, value_enum)]
    pub phase: Option<ApplyPhase>,
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
    /// Skip all script hooks (pre/post/onChange)
    #[arg(long)]
    pub skip_scripts: bool,
}

#[derive(Parser)]
pub struct PlanArgs {
    /// Config source: git URL to clone, or local path to an existing config directory
    #[arg(long)]
    pub from: Option<String>,
    /// Plan only a specific phase
    #[arg(long, value_enum)]
    pub phase: Option<ApplyPhase>,
    /// Skip specific items by dot-notation path (e.g., packages.brew.ripgrep, system.sysctl)
    #[arg(long)]
    pub skip: Vec<String>,
    /// Plan only items matching dot-notation paths (e.g., packages, files)
    #[arg(long)]
    pub only: Vec<String>,
    /// Plan only the specified module and its dependencies
    #[arg(long)]
    pub module: Option<String>,
    /// Skip all script hooks (pre/post/onChange)
    #[arg(long)]
    pub skip_scripts: bool,
    /// Reconciliation context: apply (default) or reconcile
    #[arg(long, default_value = "apply")]
    pub context: String,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new cfgd configuration repository
    #[command(
        long_about = "Scaffold or clone a cfgd configuration repository.\n\nExamples:\n  cfgd init\n  cfgd init --from https://github.com/acme/cfgd-config\n  cfgd init ~/cfgd --theme solarized-dark --apply"
    )]
    Init {
        /// Directory to initialize (default: current directory)
        #[arg(value_hint = clap::ValueHint::DirPath)]
        path: Option<String>,

        /// Config source: git URL to clone, or local path to an existing config directory
        #[arg(long)]
        from: Option<String>,

        /// Git branch to clone (default: master).
        ///
        /// This defaults to `master` because `init` materializes the config dir up-front
        /// and needs a concrete ref to check out. The split with `SourceCommand::Add` —
        /// where `--branch` has NO default and stays `Option<String>` — is intentional:
        /// `source add` stores the caller's intent as-is, and the operator later resolves
        /// `None` via `origin/HEAD`, so downstream syncs follow the remote's chosen default.
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
    #[command(
        long_about = "Apply the active profile to this machine.\n\nExamples:\n  cfgd apply\n  cfgd apply --dry-run\n  cfgd apply --phase packages --yes\n  cfgd apply --module nettools"
    )]
    Apply(ApplyArgs),

    /// Preview the reconciliation plan without applying
    #[command(
        long_about = "Render the reconciliation plan without applying it.\n\nExamples:\n  cfgd plan\n  cfgd plan --phase system\n  cfgd plan --skip packages.brew --only files"
    )]
    Plan(PlanArgs),

    /// Show configuration status and drift
    #[command(
        long_about = "Show apply status, drift, and pending decisions.\n\nWith --exit-code, exit codes are:\n  0  no drift detected\n  1  runtime error\n  5  drift detected\n\nExamples:\n  cfgd status\n  cfgd status --module nettools\n  cfgd status --exit-code"
    )]
    Status {
        /// Show status for a specific module (no profile required)
        #[arg(long)]
        module: Option<String>,
        /// Exit 5 when drift is detected (for CI gating)
        #[arg(long = "exit-code", short = 'e')]
        exit_code: bool,
    },

    /// Show detailed diffs
    #[command(
        long_about = "Show line-level diffs between desired and actual state.\n\nWith --exit-code, exit codes are:\n  0  no drift detected\n  1  runtime error\n  5  drift detected\n\nExamples:\n  cfgd diff\n  cfgd diff --module nettools\n  cfgd diff --exit-code"
    )]
    Diff {
        /// Show diff for a specific module only
        #[arg(long)]
        module: Option<String>,
        /// Exit 5 when drift is detected (for CI gating)
        #[arg(long = "exit-code", short = 'e')]
        exit_code: bool,
    },

    /// Show apply history
    #[command(
        long_about = "Show history of past apply operations.\n\nExamples:\n  cfgd log\n  cfgd log -n 50\n  cfgd log --show-output 42"
    )]
    Log {
        /// Number of entries to show
        #[arg(long, short = 'n', default_value = "20")]
        limit: u32,
        /// Show captured script output for a specific apply ID
        #[arg(long)]
        show_output: Option<i64>,
    },

    /// Sync with remote
    #[command(long_about = "Fetch remote config changes and apply.\n\nExamples:\n  cfgd sync")]
    Sync,

    /// Pull remote changes
    #[command(
        long_about = "Pull remote changes for the config repo without applying.\n\nExamples:\n  cfgd pull"
    )]
    Pull,

    /// Manage the daemon
    #[command(
        long_about = "Run or manage the cfgd reconciliation daemon.\n\nExamples:\n  cfgd daemon               # run in foreground (default)\n  cfgd daemon status\n  cfgd daemon install       # install as a system service\n  cfgd daemon uninstall"
    )]
    Daemon {
        #[command(subcommand)]
        command: Option<DaemonCommand>,
    },

    /// Manage secrets
    #[command(
        long_about = "Encrypt, decrypt, and edit SOPS-managed secret files.\n\nExamples:\n  cfgd secret init\n  cfgd secret encrypt secrets.yaml\n  cfgd secret edit secrets.yaml\n  cfgd secret decrypt secrets.yaml"
    )]
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },

    /// Manage profiles
    #[command(
        long_about = "List, inspect, and switch between profiles.\n\nExamples:\n  cfgd profile list\n  cfgd profile use laptop\n  cfgd profile show server"
    )]
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },

    /// Verify all managed resources match desired state
    #[command(
        long_about = "Verify managed state matches the applied profile.\n\nWith --exit-code, exit codes are:\n  0  all resources match desired state\n  1  runtime error\n  5  one or more resources do not match (drift)\n\nExamples:\n  cfgd verify\n  cfgd verify --module nettools\n  cfgd verify --exit-code"
    )]
    Verify {
        /// Verify only a specific module (no profile required)
        #[arg(long)]
        module: Option<String>,
        /// Exit 5 when any resource does not match desired state (for CI gating)
        #[arg(long = "exit-code", short = 'e')]
        exit_code: bool,
    },

    /// Check system health and dependencies
    #[command(
        long_about = "Diagnose environment prerequisites, tool versions, and config validity.\n\nExamples:\n  cfgd doctor\n  cfgd --output json doctor"
    )]
    Doctor,

    /// Manage modules
    #[command(
        long_about = "Create, inspect, push, and manage modules.\n\nExamples:\n  cfgd module list\n  cfgd module push ./my-module --artifact ghcr.io/me/my-module:1.0.0\n  cfgd module registry add https://github.com/my-org/cfgd-modules --name my-org"
    )]
    Module {
        #[command(subcommand)]
        command: ModuleCommand,
    },

    /// Manage config sources
    #[command(
        long_about = "Subscribe to, override, or remove upstream config sources.\n\nExamples:\n  cfgd source add https://github.com/team/config --priority 700\n  cfgd source list\n  cfgd source override team set env.EDITOR vim\n  cfgd source remove team --keep-all"
    )]
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },

    /// Check for and install updates
    #[command(
        long_about = "Check for, download, and install a newer cfgd release.\n\nWith --check, exit codes are:\n  0  already at latest version\n  1  network / IO error\n  2  update available (action needed, not an error)\n\nExamples:\n  cfgd upgrade\n  cfgd upgrade --check"
    )]
    Upgrade {
        /// Only check if an update is available (exit 0 = current, exit 2 = update available, exit 1 = error)
        #[arg(long)]
        check: bool,
    },

    /// Accept or reject pending source decisions
    #[command(
        long_about = "Accept or reject pending decisions from subscribed sources.\n\nExamples:\n  cfgd decide accept packages.brew.ripgrep\n  cfgd decide reject --source team\n  cfgd decide accept --all"
    )]
    Decide {
        /// Action: accept or reject
        #[arg(value_enum)]
        action: DecideAction,

        /// Resource path to decide on (e.g. packages.brew.k9s). Omit for batch operations.
        #[arg(conflicts_with_all = ["source", "all"])]
        resource: Option<String>,

        /// Apply decision to all pending items from this source
        #[arg(long, conflicts_with = "all")]
        source: Option<String>,

        /// Apply decision to all pending items
        #[arg(long)]
        all: bool,
    },

    /// Show schema and field documentation for cfgd resource types
    #[command(
        long_about = "Render schema + field docs for a cfgd resource type.\n\nExamples:\n  cfgd explain module\n  cfgd explain profile.spec.packages --recursive"
    )]
    Explain {
        /// Resource type or field path (e.g., "module", "profile.spec.packages")
        #[arg(value_hint = clap::ValueHint::Other)]
        resource: Option<String>,

        /// Show all fields expanded recursively
        #[arg(long)]
        recursive: bool,
    },

    /// View or edit the cfgd configuration
    #[command(
        long_about = "Show, edit, get, set, or unset config values.\n\nExamples:\n  cfgd config show\n  cfgd config get spec.theme\n  cfgd config set spec.theme dracula\n  cfgd config unset spec.theme"
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Manage GitHub Actions workflows for config repo releases
    #[command(
        long_about = "Generate or refresh GitHub Actions workflows for releasing config repo modules.\n\nExamples:\n  cfgd workflow generate\n  cfgd workflow generate --force"
    )]
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },

    /// Check in with the device gateway and report status
    #[command(
        long_about = "Report compliance status to the device gateway.\n\nExamples:\n  cfgd checkin --server-url https://gateway.example.com\n  CFGD_SERVER_URL=https://gw.example.com cfgd checkin"
    )]
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
    #[command(
        long_about = "Enroll this device with a gateway via bootstrap token or SSH/GPG signing key.\n\nExamples:\n  cfgd enroll --server-url https://gw.example.com --token $BOOTSTRAP\n  cfgd enroll --server-url https://gw.example.com --ssh-key ~/.ssh/id_ed25519\n  cfgd enroll --server-url https://gw.example.com --gpg-key 0xABCDEF"
    )]
    Enroll {
        /// Device gateway URL
        #[arg(long, env = "CFGD_SERVER_URL")]
        server_url: String,

        /// Bootstrap token for token-based enrollment
        #[arg(long, env = "CFGD_ENROLL_TOKEN")]
        token: Option<String>,

        /// SSH key file for signing (default: auto-detect from agent or ~/.ssh/)
        #[arg(long, conflicts_with = "gpg_key")]
        ssh_key: Option<String>,

        /// GPG key ID for signing
        #[arg(long, conflicts_with = "ssh_key")]
        gpg_key: Option<String>,

        /// Username to enroll as (default: current system user)
        #[arg(long, env = "CFGD_ENROLL_USERNAME")]
        username: Option<String>,
    },

    /// Generate shell completions
    #[command(
        long_about = "Emit shell-completion script for bash/zsh/fish/elvish/powershell on stdout.\n\nExamples:\n  cfgd completions bash > /etc/bash_completion.d/cfgd\n  cfgd completions zsh > ~/.zfunc/_cfgd"
    )]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// AI-guided configuration generation
    #[command(
        long_about = "Generate config fragments (profiles, modules) with an LLM backend.\n\nExamples:\n  cfgd generate                    # scan system and propose full structure\n  cfgd generate module kubectl\n  cfgd generate profile laptop --model claude-opus-4-6\n  cfgd generate --scan-only --shell zsh --home ~/\n  cfgd generate --yes              # skip confirmation prompts"
    )]
    Generate(generate::GenerateArgs),

    /// Roll back a previous apply by restoring file backups
    #[command(
        long_about = "Restore files to their pre-apply state using captured backups.\n\nExamples:\n  cfgd log\n  cfgd rollback 42 --yes"
    )]
    Rollback {
        /// Apply ID to roll back (from 'cfgd log')
        apply_id: i64,

        /// Skip confirmation prompt
        #[arg(long, short, env = "CFGD_YES")]
        yes: bool,
    },

    /// Start MCP server for AI editor integration
    #[command(
        name = "mcp-server",
        long_about = "Run an MCP server on stdio for AI/editor tool integration.\n\nExamples:\n  cfgd mcp-server"
    )]
    McpServer,

    /// Compliance status and evidence export
    #[command(
        long_about = "Export or inspect compliance snapshots for audit.\n\nExamples:\n  cfgd compliance export\n  cfgd compliance history --since 7d\n  cfgd compliance diff 42 47"
    )]
    Compliance {
        #[command(subcommand)]
        command: Option<ComplianceCommand>,
    },
}

#[derive(Subcommand)]
pub enum ComplianceCommand {
    /// Export compliance snapshot to file or stdout
    Export,
    /// Show compliance snapshot history
    History {
        /// Show only snapshots since this duration ago (e.g. 7d, 24h, 30m)
        #[arg(long)]
        since: Option<String>,
    },
    /// Show diff between two snapshots
    Diff {
        /// Base snapshot ID (the reference to compare against)
        #[arg(value_name = "BASE_ID")]
        base_id: i64,
        /// Target snapshot ID (the snapshot being compared)
        #[arg(value_name = "TARGET_ID")]
        target_id: i64,
    },
}

#[derive(Parser)]
pub struct SourceAddArgs {
    /// Git URL of the source
    pub url: String,
    /// Name for this source (default: inferred from URL)
    #[arg(long)]
    pub name: Option<String>,
    /// Git branch to subscribe to.
    ///
    /// Deliberately has NO default value (unlike `init --branch`): leaving this unset
    /// stores `None` in the source config so downstream syncs resolve against the
    /// remote's current default (via `origin/HEAD`). Only set `--branch` when you
    /// need to pin the subscription to a specific ref.
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
    #[arg(long = "version-pin", alias = "pin-version")]
    pub version_pin: Option<String>,
    /// Skip confirmation prompt
    #[arg(long, short, env = "CFGD_YES")]
    pub yes: bool,
}

#[derive(Subcommand)]
pub enum SourceCommand {
    /// Subscribe to a config source
    Add(Box<SourceAddArgs>),

    /// List subscribed sources
    #[command(alias = "ls")]
    List,

    /// Show details of a source
    Show {
        /// Source name
        name: String,
    },

    /// Remove a source subscription
    #[command(alias = "rm")]
    Remove {
        /// Source name
        name: String,

        /// Keep all resources from this source as local
        #[arg(long, conflicts_with = "remove_all")]
        keep_all: bool,

        /// Remove all resources from this source
        #[arg(long, conflicts_with = "keep_all")]
        remove_all: bool,

        /// Skip confirmation prompt (defaults to --keep-all behavior)
        #[arg(long, short, env = "CFGD_YES")]
        yes: bool,
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
        #[arg(value_enum)]
        action: SourceOverrideAction,

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
pub enum DaemonCommand {
    /// Run daemon in foreground (default when no subcommand given)
    Run,
    /// Install as a system service (launchd on macOS, systemd on Linux, Windows Service on Windows)
    Install,
    /// Uninstall the system service
    Uninstall,
    /// Show daemon status
    Status,
    /// Run as a Windows Service (called by SCM, not directly by users)
    #[clap(hide = true)]
    Service,
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
        #[arg(long, short = 'y', alias = "yes", env = "CFGD_YES")]
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
    pub pre_apply: Vec<String>,
    /// Post-apply scripts (repeatable)
    #[arg(long = "post-apply")]
    pub post_apply: Vec<String>,
    /// Pre-reconcile scripts (repeatable)
    #[arg(long = "pre-reconcile")]
    pub pre_reconcile: Vec<String>,
    /// Post-reconcile scripts (repeatable)
    #[arg(long = "post-reconcile")]
    pub post_reconcile: Vec<String>,
    /// On-change scripts (repeatable)
    #[arg(long = "on-change")]
    pub on_change: Vec<String>,
    /// On-drift scripts (repeatable)
    #[arg(long = "on-drift")]
    pub on_drift: Vec<String>,
}

#[derive(Parser)]
pub struct ProfileUpdateArgs {
    /// Profile name (default: active profile)
    pub name: Option<String>,
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
    pub pre_apply: Vec<String>,
    /// Post-apply scripts (repeatable, prefix with - to remove)
    #[arg(long = "post-apply", allow_hyphen_values = true)]
    pub post_apply: Vec<String>,
    /// Pre-reconcile scripts (repeatable, prefix with - to remove)
    #[arg(long = "pre-reconcile", allow_hyphen_values = true)]
    pub pre_reconcile: Vec<String>,
    /// Post-reconcile scripts (repeatable, prefix with - to remove)
    #[arg(long = "post-reconcile", allow_hyphen_values = true)]
    pub post_reconcile: Vec<String>,
    /// On-change scripts (repeatable, prefix with - to remove)
    #[arg(long = "on-change", allow_hyphen_values = true)]
    pub on_change: Vec<String>,
    /// On-drift scripts (repeatable, prefix with - to remove)
    #[arg(long = "on-drift", allow_hyphen_values = true)]
    pub on_drift: Vec<String>,
    /// Mark all --file entries as private (local-only, excluded from git).
    #[arg(long = "private-files")]
    pub private: bool,
}

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// List available profiles
    #[command(alias = "ls")]
    List,
    /// Switch to a different profile (alias: use)
    #[command(alias = "use")]
    Switch {
        /// Profile name
        name: String,
    },
    /// Show the resolved profile
    Show {
        /// Profile name (default: active profile)
        name: Option<String>,
    },
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
        #[arg(long, short, env = "CFGD_YES")]
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
    #[command(alias = "ls")]
    List,
    /// Show module details: packages, files, deps, resolved managers
    Show {
        /// Module name
        name: String,
        /// Show full env variable values (default: masked)
        #[arg(long)]
        show_values: bool,
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
        #[arg(long, short, env = "CFGD_YES")]
        yes: bool,
        /// Also remove files deployed by this module to target locations
        #[arg(long)]
        purge: bool,
    },
    /// Upgrade a remote module to a new version
    Upgrade {
        /// Module name (must be a locked remote module)
        name: String,
        /// New ref to pin to (tag or commit SHA)
        #[arg(long)]
        ref_: Option<String>,
        /// Skip confirmation prompt (for non-interactive use)
        #[arg(long, short, env = "CFGD_YES")]
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
    /// Export a module to another format
    Export {
        /// Module name
        name: String,
        /// Export format (renamed from --format to avoid shadowing the global -o / --output)
        #[arg(long = "as", value_name = "FORMAT", value_enum)]
        as_format: ExportFormat,
        /// Directory to write exported files (default: current directory)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Push a module directory to an OCI registry as an artifact
    Push {
        /// Path to the module directory (must contain module.yaml)
        dir: String,
        /// OCI artifact reference (e.g. ghcr.io/myorg/mymodule:v1.0.0)
        #[arg(long)]
        artifact: String,
        /// Platform annotation (default: auto-detected from OS/arch)
        #[arg(long)]
        platform: Option<String>,
        /// After push, apply a Module CRD to the cluster referencing the artifact
        #[arg(long)]
        apply: bool,
        /// Sign the artifact with cosign after push
        #[arg(long)]
        sign: bool,
        /// Path to cosign private key (omit for keyless signing via Fulcio/Rekor)
        #[arg(long)]
        key: Option<String>,
        /// Attach SLSA provenance attestation after push
        #[arg(long)]
        attest: bool,
    },
    /// Pull a module from an OCI registry
    Pull {
        /// OCI artifact reference (e.g. ghcr.io/myorg/mymodule:v1.0.0)
        #[arg(name = "ref")]
        artifact_ref: String,
        /// Directory to extract the module into
        #[arg(long)]
        dir: String,
        /// Require a cosign signature on the artifact
        #[arg(long)]
        require_signature: bool,
        /// Verify SLSA provenance attestation on the artifact
        #[arg(long = "verify-attest", alias = "verify-attestation")]
        verify_attestation: bool,
        /// Path to cosign public key for verification (omit for keyless)
        #[arg(long)]
        key: Option<String>,
        /// Certificate identity regexp for keyless verification
        #[arg(long)]
        certificate_identity: Option<String>,
        /// Certificate OIDC issuer regexp for keyless verification
        #[arg(long)]
        certificate_oidc_issuer: Option<String>,
    },
    /// Build a module into an OCI-ready artifact using Docker/Podman
    Build {
        /// Path to the module directory (must contain module.yaml)
        dir: String,
        /// Target platform(s), comma-separated (e.g. linux/amd64,linux/arm64)
        #[arg(long)]
        target: Option<String>,
        /// Base container image (default: ubuntu:22.04)
        #[arg(long)]
        base_image: Option<String>,
        /// OCI artifact reference — if provided, push the built artifact
        #[arg(long)]
        artifact: Option<String>,
        /// Sign the artifact after push (requires --artifact and cosign)
        #[arg(long)]
        sign: bool,
        /// Path to cosign private key for signing
        #[arg(long)]
        key: Option<String>,
    },
    /// Manage cosign signing keys for module artifacts
    Keys {
        #[command(subcommand)]
        command: ModuleKeysCommand,
    },
}

#[derive(Subcommand, Clone)]
pub enum ModuleKeysCommand {
    /// Generate a new cosign key pair
    Generate {
        /// Output directory for the key pair (default: current directory)
        #[arg(long, short)]
        dir: Option<String>,
    },
    /// List known signing keys
    #[command(alias = "ls")]
    List,
    /// Rotate signing keys: generate a new pair and re-sign specified artifacts
    Rotate {
        /// Directory containing the current cosign.key to replace
        #[arg(long, short)]
        dir: Option<String>,
        /// OCI artifact references to re-sign with the new key
        #[arg(long)]
        artifacts: Vec<String>,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum ExportFormat {
    /// DevContainer Feature (install.sh + devcontainer-feature.json)
    Devcontainer,
}

/// Decide subcommand action.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum DecideAction {
    Accept,
    Reject,
}

impl DecideAction {
    /// Resolution string persisted in the state store.
    pub fn resolution(self) -> &'static str {
        match self {
            DecideAction::Accept => "accepted",
            DecideAction::Reject => "rejected",
        }
    }
}

/// `source override` subcommand action.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum SourceOverrideAction {
    /// Override a source value locally.
    Set,
    /// Reject a source recommendation.
    Reject,
}

/// Clap-facing phase selector for `apply --phase` / `plan --phase`.
/// Mirrors `cfgd_core::reconciler::PhaseName`; lives in the CLI layer so
/// the help text can use kebab-case consistently with the rest of cfgd.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ApplyPhase {
    PreScripts,
    Env,
    Modules,
    Packages,
    System,
    Files,
    Secrets,
    PostScripts,
}

#[cfg(test)]
impl ApplyPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ApplyPhase::PreScripts => "pre-scripts",
            ApplyPhase::Env => "env",
            ApplyPhase::Modules => "modules",
            ApplyPhase::Packages => "packages",
            ApplyPhase::System => "system",
            ApplyPhase::Files => "files",
            ApplyPhase::Secrets => "secrets",
            ApplyPhase::PostScripts => "post-scripts",
        }
    }
}

/// Map a clap-validated ApplyPhase to the reconciler's PhaseName.
fn apply_phase_to_phase_name(p: ApplyPhase) -> PhaseName {
    match p {
        ApplyPhase::PreScripts => PhaseName::PreScripts,
        ApplyPhase::Env => PhaseName::Env,
        ApplyPhase::Modules => PhaseName::Modules,
        ApplyPhase::Packages => PhaseName::Packages,
        ApplyPhase::System => PhaseName::System,
        ApplyPhase::Files => PhaseName::Files,
        ApplyPhase::Secrets => PhaseName::Secrets,
        ApplyPhase::PostScripts => PhaseName::PostScripts,
    }
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
    #[command(alias = "rm")]
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
    #[command(alias = "ls")]
    List,
}

/// Execute the given CLI command. Returns Ok(()) on success.
pub fn execute(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    // No subcommand: print help and exit 0. Required for package-manager
    // validators (winget, chocolatey) that smoke-test the installed binary
    // with no arguments and treat any non-zero exit code as failure.
    let Some(command) = &cli.command else {
        use clap::CommandFactory;
        let _ = Cli::command().print_help();
        printer.newline();
        return Ok(());
    };
    match command {
        Command::Apply(args) => apply::cmd_apply(cli, printer, args),
        Command::Plan(args) => plan::cmd_plan(cli, printer, args),
        Command::Status { module, exit_code } => {
            status::cmd_status(cli, printer, module.as_deref(), *exit_code)
        }
        Command::Diff { module, exit_code } => {
            diff::cmd_diff(cli, printer, module.as_deref(), *exit_code)
        }
        Command::Log { limit, show_output } => {
            log::cmd_log(printer, *limit, *show_output, cli.state_dir.as_deref())
        }
        Command::Verify { module, exit_code } => {
            verify::cmd_verify(cli, printer, module.as_deref(), *exit_code)
        }
        Command::Profile { command } => match command {
            ProfileCommand::Show { name } => {
                profile::cmd_profile_show(cli, printer, name.as_deref())
            }
            ProfileCommand::List => profile::cmd_profile_list(cli, printer),
            ProfileCommand::Switch { name } => profile::cmd_profile_switch(cli, name, printer),
            ProfileCommand::Create(args) => profile::cmd_profile_create(cli, printer, args),
            ProfileCommand::Update(args) => {
                let profile_name = resolve_profile_name(cli, args.name.as_deref())?;
                profile::cmd_profile_update(cli, printer, &profile_name, args)
            }
            ProfileCommand::Edit { name } => profile::cmd_profile_edit(cli, printer, name),
            ProfileCommand::Delete { name, yes } => {
                profile::cmd_profile_delete(cli, printer, name, *yes)
            }
        },
        Command::Doctor => doctor::cmd_doctor(cli, printer),
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
            ModuleCommand::Show { name, show_values } => {
                module::cmd_module_show(cli, printer, name, *show_values)
            }
            ModuleCommand::Create(args) => module::cmd_module_create(cli, printer, args),
            ModuleCommand::Update(args) => module::cmd_module_update_local(cli, printer, args),
            ModuleCommand::Edit { name } => module::cmd_module_edit(cli, printer, name),
            ModuleCommand::Delete { name, yes, purge } => {
                module::cmd_module_delete(cli, printer, name, *yes, *purge)
            }
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
            ModuleCommand::Export {
                name,
                as_format,
                dir,
            } => module::cmd_module_export(cli, printer, name, as_format, dir.as_deref()),
            ModuleCommand::Push {
                dir,
                artifact,
                platform,
                apply,
                sign,
                key,
                attest,
            } => module::cmd_module_push(
                printer,
                dir,
                artifact,
                module::PushOptions {
                    platform: platform.as_deref(),
                    apply: *apply,
                    sign: *sign,
                    key: key.as_deref(),
                    attest: *attest,
                },
            ),
            ModuleCommand::Pull {
                artifact_ref,
                dir,
                require_signature,
                verify_attestation,
                key,
                certificate_identity,
                certificate_oidc_issuer,
            } => module::cmd_module_pull(
                printer,
                artifact_ref,
                dir,
                *require_signature,
                *verify_attestation,
                cfgd_core::oci::VerifyOptions {
                    key: key.as_deref(),
                    identity: certificate_identity.as_deref(),
                    issuer: certificate_oidc_issuer.as_deref(),
                },
            ),
            ModuleCommand::Build {
                dir,
                target,
                base_image,
                artifact,
                sign,
                key,
            } => module::cmd_module_build(
                printer,
                dir,
                target.as_deref(),
                base_image.as_deref(),
                artifact.as_deref(),
                *sign,
                key.as_deref(),
            ),
            ModuleCommand::Keys { command } => match command {
                ModuleKeysCommand::Generate { dir } => {
                    module::cmd_module_keys_generate(printer, dir.as_deref())
                }
                ModuleKeysCommand::List => module::cmd_module_keys_list(printer),
                ModuleKeysCommand::Rotate { dir, artifacts } => {
                    module::cmd_module_keys_rotate(printer, dir.as_deref(), artifacts)
                }
            },
        },
        Command::Sync => sync::cmd_sync(cli, printer),
        Command::Pull => pull::cmd_pull(cli, printer),
        Command::Daemon { command } => daemon::cmd_daemon(cli, printer, command.as_ref()),
        Command::Secret { command } => match command {
            SecretCommand::Encrypt { file } => secret::cmd_secret_encrypt(cli, printer, file),
            SecretCommand::Decrypt { file } => secret::cmd_secret_decrypt(cli, printer, file),
            SecretCommand::Edit { file } => secret::cmd_secret_edit(cli, printer, file),
            SecretCommand::Init => secret::cmd_secret_init(cli, printer),
        },
        Command::Source { command } => match command {
            SourceCommand::Add(args) => source::cmd_source_add(cli, printer, args),
            SourceCommand::Priority { name, value } => {
                source::cmd_source_priority(cli, printer, name, *value)
            }
            SourceCommand::List => source::cmd_source_list(cli, printer),
            SourceCommand::Show { name } => source::cmd_source_show(cli, printer, name),
            SourceCommand::Remove {
                name,
                keep_all,
                remove_all,
                yes,
            } => source::cmd_source_remove(
                cli,
                printer,
                name,
                *keep_all || (*yes && !*remove_all),
                *remove_all,
            ),
            SourceCommand::Update { name } => {
                source::cmd_source_update(cli, printer, name.as_deref())
            }
            SourceCommand::Override {
                source,
                action,
                path,
                value,
            } => source::cmd_source_override(cli, printer, source, *action, path, value.as_deref()),
            SourceCommand::Replace { old_name, new_url } => {
                source::cmd_source_replace(cli, printer, old_name, new_url)
            }
            SourceCommand::Edit => source::cmd_source_edit(cli, printer),
            SourceCommand::Create {
                name,
                description,
                version,
            } => source::cmd_source_create(
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
        Command::Upgrade { check } => upgrade::cmd_upgrade(printer, *check),
        Command::Decide {
            action,
            resource,
            source,
            all,
        } => decide::cmd_decide(
            printer,
            *action,
            resource.as_deref(),
            source.as_deref(),
            *all,
            cli.state_dir.as_deref(),
        ),
        Command::Config { command } => match command {
            ConfigCommand::Show => config_cmd::cmd_config_show(cli, printer),
            ConfigCommand::Edit => config_cmd::cmd_config_edit(cli, printer),
            ConfigCommand::Get { key } => config_cmd::cmd_config_get(cli, printer, key),
            ConfigCommand::Set { key, value } => {
                config_cmd::cmd_config_set(cli, printer, key, value)
            }
            ConfigCommand::Unset { key } => config_cmd::cmd_config_unset(cli, printer, key),
        },
        Command::Workflow { command } => match command {
            WorkflowCommand::Generate { force } => {
                workflow::cmd_workflow_generate(cli, printer, *force)
            }
        },
        Command::Checkin {
            server_url,
            api_key,
            device_id,
        } => checkin::cmd_checkin(
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
        Command::Generate(args) => generate::cmd_generate(cli, printer, args),
        Command::Rollback { apply_id, yes } => {
            rollback::cmd_rollback(printer, *apply_id, *yes, cli.state_dir.as_deref())
        }
        Command::McpServer => crate::mcp::server::run_mcp_server(&cli.config),
        Command::Compliance { command } => match command {
            None => compliance::cmd_compliance_snapshot(cli, printer),
            Some(ComplianceCommand::Export) => compliance::cmd_compliance_export(cli, printer),
            Some(ComplianceCommand::History { since }) => {
                compliance::cmd_compliance_history(cli, printer, since.as_deref())
            }
            Some(ComplianceCommand::Diff { base_id, target_id }) => {
                compliance::cmd_compliance_diff(cli, printer, *base_id, *target_id)
            }
        },
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

/// Build an empty ResolvedProfile for module-only operations that don't need
/// a real profile (status --module, verify --module, apply --module without profile).
fn empty_resolved_profile(module_name: &str) -> ResolvedProfile {
    ResolvedProfile {
        layers: Vec::new(),
        merged: MergedProfile {
            modules: vec![module_name.to_string()],
            ..Default::default()
        },
    }
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
    // On Windows, paths like C:\foo contain colons that are NOT source:target separators.
    // A drive letter is a single ASCII letter followed by `:` and `\` or `/`.
    // We skip the first colon if it's part of a drive letter prefix.
    let split_pos = spec.char_indices().find_map(|(i, c)| {
        if c == ':' {
            // Skip if this colon is at position 1 and preceded by a single ASCII letter
            // (i.e., a Windows drive letter like C: or D:)
            if i == 1 && spec.as_bytes()[0].is_ascii_alphabetic() {
                return None;
            }
            Some(i)
        } else {
            None
        }
    });

    if let Some(pos) = split_pos {
        let source = &spec[..pos];
        let target = &spec[pos + 1..];
        // Target may also start with a drive letter — handle C:\path after the separator
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

        // Reject sources in system directories to prevent path traversal attacks.
        // module create --file copies the source then replaces it with a symlink,
        // so importing /etc/passwd would delete it and replace with a symlink.
        let canonical_source = source
            .canonicalize()
            .unwrap_or_else(|_| source.to_path_buf());
        // These prefixes are checked against both the original and canonical path.
        // /var is omitted here because on macOS /var/folders is the user temp
        // directory — tempfile crates produce paths under /var/folders/… which
        // must remain importable.  /var on Linux is covered via canonical_source
        // (Linux does not redirect /var, so original == canonical there).
        let forbidden_prefixes: &[&str] = &[
            "/etc",
            "/usr",
            "/bin",
            "/sbin",
            "/boot",
            "/sys",
            "/proc",
            "/lib",
            "/lib64",
            "/dev",
            "/snap",
            // macOS symlinks /etc → /private/etc; check canonical to catch traversal.
            "/private/etc",
        ];
        for prefix in forbidden_prefixes {
            if source.starts_with(prefix) || canonical_source.starts_with(prefix) {
                anyhow::bail!(
                    "Refusing to import '{}': source is in system directory {}",
                    source.display(),
                    prefix
                );
            }
        }
        // Check /var against the canonical path only. On Linux canonical == original
        // so this catches system /var correctly. On macOS /var symlinks to
        // /private/var, so temp files (/var/folders/…) canonicalize to
        // /private/var/folders/… which does not start with /var — safe to allow.
        if canonical_source.starts_with("/var") {
            anyhow::bail!(
                "Refusing to import '{}': source is in system directory /var",
                source.display()
            );
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
        // Symlink back from source location to repo copy so the user's
        // dotfile now points into the cfgd-managed directory.
        if source.exists() && !source.is_symlink() {
            if source.is_dir() {
                std::fs::remove_dir_all(&source)?;
            } else {
                std::fs::remove_file(&source)?;
            }
            cfgd_core::create_symlink(&dest, &source)?;
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
    cfgd_core::atomic_write_str(&gitignore, &content)?;
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
        cfgd_core::expand_tilde(path)
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

    // ShellConfigurator: `chsh` on Unix, Windows Terminal settings.json on Windows
    if cfg!(unix) || cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(ShellConfigurator));
    }

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
        // Linux desktop configurators — each checks CLI availability at runtime via is_available()
        registry
            .system_configurators
            .push(Box::new(GsettingsConfigurator));
        registry
            .system_configurators
            .push(Box::new(KdeConfigConfigurator));
        registry
            .system_configurators
            .push(Box::new(XfconfConfigurator));
    }

    // Environment configurator is available on Unix and Windows
    if cfg!(unix) || cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(EnvironmentConfigurator));
    }

    // Windows registry configurator
    if cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(WindowsRegistryConfigurator));
    }

    // Windows service configurator
    if cfg!(windows) {
        registry
            .system_configurators
            .push(Box::new(WindowsServiceConfigurator));
    }

    // SSH key configurator — available unconditionally (ssh-keygen on all platforms)
    registry
        .system_configurators
        .push(Box::new(SshKeysConfigurator));

    // GPG key configurator — available on any platform where gpg is installed
    if cfgd_core::command_available("gpg") {
        registry
            .system_configurators
            .push(Box::new(GpgKeysConfigurator));
    }

    // Git configurator — cross-platform, gated on git being available at runtime
    if cfgd_core::command_available("git") {
        registry
            .system_configurators
            .push(Box::new(GitConfigurator));
    }

    // Node/infrastructure system configurators (Linux-only, gated at compile time)
    #[cfg(unix)]
    {
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
    }

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

#[cfg(unix)]
fn print_daemon_install_success(printer: &Printer) {
    if cfg!(target_os = "macos") {
        printer.success("Installed launchd service: com.cfgd.daemon");
        printer.info("Load with: launchctl load ~/Library/LaunchAgents/com.cfgd.daemon.plist");
    } else {
        printer.success("Installed systemd user service: cfgd.service");
        printer.info("Enable with: systemctl --user enable --now cfgd.service");
    }
}

fn open_state_store(state_dir: Option<&Path>) -> anyhow::Result<StateStore> {
    if let Some(dir) = state_dir {
        std::fs::create_dir_all(dir)?;
        Ok(StateStore::open(&dir.join("cfgd.db"))?)
    } else {
        Ok(StateStore::open_default()?)
    }
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
    cfgd_core::config::for_each_yaml_file(profiles_dir, |path| {
        if let Ok(doc) = config::load_profile(path) {
            names.push(doc.metadata.name);
        }
        Ok(())
    })?;
    names.sort();
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

    if !file.exists() {
        anyhow::bail!("File not found: {}", file.display());
    }

    match registry.secret_backend {
        Some(ref backend) if !backend.is_available() => {
            anyhow::bail!("{}: not installed", backend.name());
        }
        None => anyhow::bail!("No secret backend configured"),
        _ => {}
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

pub(crate) fn config_dir(cli: &Cli) -> PathBuf {
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
    cfgd_core::config::for_each_yaml_file(dir, |path| {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_string());
        }
        Ok(())
    })?;
    names.sort();
    Ok(names)
}

/// Resolve profile name from explicit name or default to active profile.
fn resolve_profile_name(cli: &Cli, name: Option<&str>) -> anyhow::Result<String> {
    if let Some(n) = name {
        return Ok(n.to_string());
    }
    // Default to active profile
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

fn default_device_id() -> String {
    cfgd_core::hostname_string()
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
    cfg: &config::CfgdConfig,
    local_resolved: &ResolvedProfile,
    printer: &Printer,
) -> anyhow::Result<composition::CompositionResult> {
    if cfg.spec.sources.is_empty() {
        // No sources, return local profile as-is
        return Ok(composition::CompositionResult {
            resolved: local_resolved.clone(),
            conflicts: Vec::new(),
            source_env: std::collections::HashMap::new(),
            source_commits: std::collections::HashMap::new(),
        });
    }

    let cache_dir = source_cache_dir(cli)?;
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

            // Check if local config overrides any locked resources from this source
            if let Err(e) = composition::check_locked_violations(
                &source_spec.name,
                &cached.manifest.spec.policy.locked,
                &local_resolved.merged,
            ) {
                printer.warning(&format!(
                    "Locked resource conflict with source '{}': {}",
                    source_spec.name, e
                ));
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

    let mut result = composition::compose(local_resolved, &inputs)?;

    // Collect source commit hashes for record_source_apply linkage
    for source_spec in &cfg.spec.sources {
        if let Some(cached) = mgr.get(&source_spec.name)
            && let Some(ref commit) = cached.last_commit
        {
            result
                .source_commits
                .insert(source_spec.name.clone(), commit.clone());
        }
    }

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

        // Persist conflicts to state
        if let Ok(state) = open_state_store(cli.state_dir.as_deref()) {
            for conflict in &result.conflicts {
                if let Err(e) = state.record_source_conflict(
                    &conflict.winning_source,
                    "composition",
                    &conflict.resource_id,
                    conflict.resolution_type.label(),
                    Some(&conflict.details),
                ) {
                    tracing::warn!(
                        error = %e,
                        winning_source = %conflict.winning_source,
                        resource_id = %conflict.resource_id,
                        "failed to persist source conflict to state store; conflict history may be incomplete",
                    );
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests;
