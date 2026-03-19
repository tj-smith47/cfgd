use clap::{Args, Subcommand};

use cfgd_core::config::{self, AiConfig};
use cfgd_core::output::Printer;

use crate::ai::client::{AnthropicClient, ContentBlock};
use crate::ai::conversation::Conversation;
use crate::ai::tools;
use crate::generate;
use crate::packages;

use super::{Cli, config_dir};

#[derive(Debug, Args)]
pub struct GenerateArgs {
    #[command(subcommand)]
    pub target: Option<GenerateTarget>,

    /// Override AI model
    #[arg(long)]
    pub model: Option<String>,

    /// Override AI provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Skip confirmation prompts
    #[arg(long, short)]
    pub yes: bool,

    /// Only scan dotfiles and shell config; print findings without AI generation
    #[arg(long)]
    pub scan_only: bool,

    /// Shell to scan for aliases and exports (default: auto-detect from $SHELL)
    #[arg(long)]
    pub shell: Option<String>,

    /// Home directory to scan (default: $HOME)
    #[arg(long, value_hint = clap::ValueHint::DirPath)]
    pub home: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum GenerateTarget {
    /// Generate a module for a specific tool
    Module {
        /// Tool name to generate module for
        name: String,
    },
    /// Generate a profile
    Profile {
        /// Profile name to generate
        name: String,
    },
}

pub fn cmd_generate(cli: &Cli, printer: &Printer, args: &GenerateArgs) -> anyhow::Result<()> {
    // Support legacy --scan-only mode
    if args.scan_only {
        return cmd_generate_scan_only(printer, args);
    }

    // 1. Load config, resolve AiConfig
    let ai_config = match config::load_config(&cli.config) {
        Ok(cfg) => cfg.spec.ai.clone().unwrap_or_default(),
        Err(cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::NotFound {
            ..
        })) => AiConfig::default(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse config; using AI defaults");
            AiConfig::default()
        }
    };

    let provider = args.provider.as_deref().unwrap_or(&ai_config.provider);
    let model = args.model.as_deref().unwrap_or(&ai_config.model);

    // 2. Resolve API key
    let api_key = std::env::var(&ai_config.api_key_env).map_err(|_| {
        cfgd_core::errors::GenerateError::ApiKeyNotFound {
            env_var: ai_config.api_key_env.clone(),
        }
    })?;

    // 3. Consent disclosure (unless --yes)
    if !args.yes {
        printer.warning(&format!(
            "This command sends file contents and system information to {}'s API to generate your configuration.",
            provider
        ));
        printer.info(
            "Only files in your home directory are accessible, and private keys/credentials are excluded.",
        );
        let proceed = inquire::Confirm::new("Continue?")
            .with_default(true)
            .prompt()?;
        if !proceed {
            printer.info("Aborted.");
            return Ok(());
        }
    }

    // 4. Create session
    let repo_root = config_dir(cli);
    let mut session = cfgd_core::generate::session::GenerateSession::new(repo_root);

    // 5. Build system prompt
    let skill = generate::GENERATE_SKILL;
    let mode_context = match &args.target {
        None => "Mode: full — scan system, propose structure, generate all profiles and modules."
            .to_string(),
        Some(GenerateTarget::Module { name }) => {
            format!("Mode: module — generate module for tool '{}'.", name)
        }
        Some(GenerateTarget::Profile { name }) => {
            format!("Mode: profile — generate profile '{}'.", name)
        }
    };
    let system_prompt = format!("{}\n\n## Current Session\n\n{}", skill, mode_context);

    // 6. Create client and conversation
    let client = AnthropicClient::new(api_key, model.to_string());
    let mut conversation = Conversation::new(system_prompt);
    let tool_defs = tools::tool_definitions();

    // 7. Build initial user message
    let initial_message = match &args.target {
        None => "Please scan my system and help me organize my configuration into cfgd profiles and modules.".to_string(),
        Some(GenerateTarget::Module { name }) => {
            format!("Please help me create a cfgd module for '{}'.", name)
        }
        Some(GenerateTarget::Profile { name }) => {
            format!("Please help me create a cfgd profile named '{}'.", name)
        }
    };
    conversation.add_user_message(&initial_message);

    // 8. Get package managers for tool dispatch
    let managers: Vec<Box<dyn cfgd_core::providers::PackageManager>> =
        packages::all_package_managers();
    let home = dirs_from_env();

    // 9. Conversation loop
    const MAX_TURNS: usize = 100;
    let mut turn = 0usize;
    loop {
        if turn >= MAX_TURNS {
            printer.warning(&format!(
                "Conversation reached the {MAX_TURNS}-turn limit; stopping."
            ));
            break;
        }
        turn += 1;
        let response = client.send_message(
            conversation.messages(),
            conversation.system_prompt(),
            &tool_defs,
            8192,
        )?;

        tracing::debug!(
            id = %response.id,
            stop_reason = ?response.stop_reason,
            "API response received"
        );

        conversation.track_usage(response.usage.input_tokens, response.usage.output_tokens);

        let mut tool_results: Vec<ContentBlock> = vec![];
        let mut has_tool_calls = false;

        for block in &response.content {
            match block {
                ContentBlock::Text { text } => {
                    printer.info(text);
                }
                ContentBlock::ToolUse { id, name, input } => {
                    has_tool_calls = true;

                    if name == "present_yaml" {
                        let result = handle_present_yaml(printer, id, input, args.yes)?;
                        tool_results.push(result);
                    } else {
                        let result =
                            tools::dispatch_tool_call(name, input, &mut session, &home, &managers);
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: result.content,
                            is_error: if result.is_error { Some(true) } else { None },
                        });
                    }
                }
                _ => {}
            }
        }

        // Add assistant message to history
        conversation.add_assistant_message(response.content.clone());

        if has_tool_calls {
            conversation.add_tool_results(tool_results);
        } else {
            break;
        }
    }

    // 10. Show summary
    let generated = session.list_generated();
    if !generated.is_empty() {
        printer.header("Generated files");
        for item in &generated {
            printer.success(&format!("{}: {}", item.name, item.path.display()));
        }

        let commit = if args.yes {
            true
        } else {
            inquire::Confirm::new("Commit all generated files?")
                .with_default(true)
                .prompt()?
        };
        if commit {
            let mut add_cmd = std::process::Command::new("git");
            add_cmd.arg("add");
            for g in &generated {
                add_cmd.arg(g.path.as_os_str());
            }
            let add_out = add_cmd.output()?;
            if !add_out.status.success() {
                printer.warning(&format!(
                    "git add failed: {}",
                    String::from_utf8_lossy(&add_out.stderr).trim()
                ));
            } else {
                let commit_out = std::process::Command::new("git")
                    .args([
                        "commit",
                        "-m",
                        "feat: add AI-generated configuration profiles and modules",
                    ])
                    .output()?;
                if commit_out.status.success() {
                    printer.success("Changes committed.");
                } else {
                    printer.warning(&format!(
                        "git commit failed: {}",
                        String::from_utf8_lossy(&commit_out.stderr).trim()
                    ));
                }
            }
        }
    }

    // 11. Show token usage
    let (input_tokens, output_tokens) = conversation.total_tokens();
    printer.info(&format!(
        "Token usage: {} input, {} output, {} total",
        input_tokens,
        output_tokens,
        input_tokens + output_tokens
    ));

    printer.info("Run 'cfgd apply --dry-run' to preview what would be applied.");

    Ok(())
}

fn handle_present_yaml(
    printer: &Printer,
    tool_use_id: &str,
    input: &serde_json::Value,
    auto_accept: bool,
) -> anyhow::Result<ContentBlock> {
    let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let kind = input
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    printer.header(&format!("Generated {} — {}", kind, description));
    printer.syntax_highlight(content, "yaml");

    let response_json = if auto_accept {
        serde_json::json!({"action": "accept"})
    } else {
        let options = vec!["Accept", "Reject", "Give feedback", "Step through"];
        let choice = inquire::Select::new("What would you like to do?", options).prompt()?;

        match choice {
            "Accept" => serde_json::json!({"action": "accept"}),
            "Reject" => serde_json::json!({"action": "reject"}),
            "Give feedback" => {
                let feedback = inquire::Text::new("Your feedback:").prompt()?;
                serde_json::json!({"action": "feedback", "message": feedback})
            }
            "Step through" => serde_json::json!({"action": "step-through"}),
            _ => serde_json::json!({"action": "reject"}),
        }
    };

    Ok(ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: serde_json::to_string(&response_json)?,
        is_error: None,
    })
}

/// Legacy scan-only mode: scan dotfiles and shell config without AI.
fn cmd_generate_scan_only(printer: &Printer, args: &GenerateArgs) -> anyhow::Result<()> {
    let home_path = if let Some(h) = &args.home {
        std::path::PathBuf::from(h)
    } else {
        dirs_from_env()
    };

    let detected_shell = args
        .shell
        .clone()
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
            let mut sorted_tools: Vec<String> = tool_set.into_iter().collect();
            sorted_tools.sort();
            printer.info(&format!("Detected tools: {}", sorted_tools.join(", ")));
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

    printer.success("Scan complete — use without --scan-only to generate config");
    Ok(())
}

fn dirs_from_env() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_args_default() {
        let args = GenerateArgs {
            target: None,
            model: None,
            provider: None,
            yes: false,
            scan_only: false,
            shell: None,
            home: None,
        };
        assert!(args.target.is_none());
        assert!(!args.yes);
        assert!(!args.scan_only);
    }

    #[test]
    fn generate_args_with_model_override() {
        let args = GenerateArgs {
            target: None,
            model: Some("claude-opus-4-20250514".into()),
            provider: Some("claude".into()),
            yes: true,
            scan_only: false,
            shell: None,
            home: None,
        };
        assert_eq!(args.model.as_deref(), Some("claude-opus-4-20250514"));
        assert_eq!(args.provider.as_deref(), Some("claude"));
        assert!(args.yes);
    }

    #[test]
    fn generate_target_module() {
        let target = GenerateTarget::Module {
            name: "neovim".into(),
        };
        match target {
            GenerateTarget::Module { name } => assert_eq!(name, "neovim"),
            _ => panic!("Expected Module"),
        }
    }

    #[test]
    fn generate_target_profile() {
        let target = GenerateTarget::Profile {
            name: "work".into(),
        };
        match target {
            GenerateTarget::Profile { name } => assert_eq!(name, "work"),
            _ => panic!("Expected Profile"),
        }
    }

    #[test]
    fn dirs_from_env_returns_path() {
        let path = dirs_from_env();
        assert!(!path.as_os_str().is_empty());
    }
}
