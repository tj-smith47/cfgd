use clap::{Args, Subcommand};

use cfgd_core::PathDisplayExt;
use cfgd_core::config::{self, AiConfig};
use cfgd_core::generate::{PresentYamlRequest, PresentYamlResponse};
use cfgd_core::output::{Doc, Printer, Role};

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
    #[arg(long, short, env = "CFGD_YES")]
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
    // --scan-only short-circuits the AI conversation loop.
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
        crate::cli::cli_error_ctx(
            cfgd_core::errors::GenerateError::ApiKeyNotFound {
                env_var: ai_config.api_key_env.clone(),
            }
            .into(),
            ai_config.api_key_env.clone(),
            "api_error",
            format!("API key env var '{}' not set", ai_config.api_key_env),
            serde_json::json!({ "envVar": ai_config.api_key_env }),
        )
    })?;

    // 3. Consent disclosure (unless --yes)
    if !args.yes {
        let consent = printer.section("Consent");
        consent.status(
            Role::Warn,
            format!(
                "This command sends file contents and system information to {}'s API to generate your configuration.",
                provider
            ),
        );
        consent.note(
            "Only files in your home directory are accessible, and private keys/credentials are excluded.",
        );
        // Routed through Printer::prompt_confirm so -o json / structured mode
        // early-returns with a non-interactive error instead of hanging.
        let proceed = printer.prompt_confirm("Continue?")?;
        if !proceed {
            drop(consent);
            printer.emit(
                Doc::new()
                    .status(Role::Info, "Aborted.")
                    .with_data(serde_json::json!({
                        "target": target_label(&args.target),
                        "outputPath": "",
                        "scanned": 0,
                        "modulesGenerated": 0,
                        "aborted": true,
                    })),
            );
            return Ok(());
        }
    }

    // 4. Create session
    let repo_root = config_dir(cli);
    let mut session = cfgd_core::generate::session::GenerateSession::new(
        repo_root.clone(),
        env!("CARGO_PKG_VERSION"),
    );

    // 5. Build system prompt
    let skill = cfgd_core::generate::skill_model_for(match &args.target {
        Some(GenerateTarget::Profile { .. }) => cfgd_core::generate::SkillKind::Profile,
        _ => cfgd_core::generate::SkillKind::Module,
    })
    .render_system_prompt();
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
            printer.status_simple(
                Role::Warn,
                format!("Conversation reached the {MAX_TURNS}-turn limit; stopping."),
            );
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
                    printer.status_simple(Role::Info, text);
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
    let generated_count = generated.len();
    let generated_files: Vec<serde_json::Value> = generated
        .iter()
        .map(|g| {
            serde_json::json!({
                "name": g.name,
                "path": g.path.display().to_string(),
            })
        })
        .collect();
    let mut committed = false;
    if !generated.is_empty() {
        {
            let sec = printer.section("Generated files");
            for item in &generated {
                sec.status(Role::Ok, format!("{}: {}", item.name, item.path.posix()));
            }
        }

        let commit = if args.yes {
            true
        } else {
            printer.prompt_confirm("Commit all generated files?")?
        };
        if commit {
            // Scope git operations to the generated-config repo. Without
            // `current_dir`, git runs in the cargo process cwd — fine for
            // operator use but in test/CI parallel runs it ends up locking
            // the host workspace's `.git/index.lock` and stomps on sibling
            // tests' git operations. `repo_root` is the dir holding cfgd.yaml.
            let mut add_cmd = cfgd_core::git_cmd_local();
            add_cmd.current_dir(&repo_root);
            add_cmd.arg("add");
            for g in &generated {
                add_cmd.arg(g.path.as_os_str());
            }
            let add_out = add_cmd.output()?;
            if !add_out.status.success() {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "git add failed: {}",
                        cfgd_core::stderr_lossy_trimmed(&add_out)
                    ),
                );
            } else {
                let commit_out = cfgd_core::git_cmd_local()
                    .current_dir(&repo_root)
                    .args([
                        "commit",
                        "-m",
                        "feat: add AI-generated configuration profiles and modules",
                    ])
                    .output()?;
                if commit_out.status.success() {
                    committed = true;
                    printer.status_simple(Role::Ok, "Changes committed.");
                } else {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "git commit failed: {}",
                            cfgd_core::stderr_lossy_trimmed(&commit_out)
                        ),
                    );
                }
            }
        }
    }

    // 11. Show token usage
    let (input_tokens, output_tokens) = conversation.total_tokens();
    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Generated {} file(s)", generated_count))
            .kv(
                "Tokens",
                format!("{} in, {} out", input_tokens, output_tokens),
            )
            .hint("Run 'cfgd apply --dry-run' to preview what would be applied.")
            .with_data(serde_json::json!({
                "target": target_label(&args.target),
                "provider": provider,
                "model": model,
                "modulesGenerated": generated_count,
                "scanned": 0,
                "files": generated_files,
                "committed": committed,
                "inputTokens": input_tokens,
                "outputTokens": output_tokens,
            })),
    );

    Ok(())
}

fn target_label(target: &Option<GenerateTarget>) -> String {
    match target {
        None => "full".to_string(),
        Some(GenerateTarget::Module { name }) => format!("module/{}", name),
        Some(GenerateTarget::Profile { name }) => format!("profile/{}", name),
    }
}

fn handle_present_yaml(
    printer: &Printer,
    tool_use_id: &str,
    input: &serde_json::Value,
    auto_accept: bool,
) -> anyhow::Result<ContentBlock> {
    let req: PresentYamlRequest = serde_json::from_value(input.clone())?;

    printer.heading(format!("Generated {} — {}", req.kind, req.description));
    printer.syntax_highlight(&req.content, "yaml");

    let response = if auto_accept {
        PresentYamlResponse::Accept
    } else {
        let options: Vec<String> = ["Accept", "Reject", "Give feedback", "Step through"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let choice = printer
            .prompt_select("What would you like to do?", &options)?
            .as_str();

        match choice {
            "Accept" => PresentYamlResponse::Accept,
            "Reject" => PresentYamlResponse::Reject,
            "Give feedback" => {
                let feedback = printer.prompt_text("Your feedback:", "")?;
                PresentYamlResponse::Feedback { message: feedback }
            }
            "Step through" => PresentYamlResponse::StepThrough,
            _ => PresentYamlResponse::Reject,
        }
    };

    Ok(ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: serde_json::to_string(&serde_json::to_value(response)?)?,
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

    let dotfiles = generate::scan::scan_dotfiles(&home_path)?;
    let tool_set: std::collections::HashSet<String> = dotfiles
        .iter()
        .filter_map(|e| e.tool_guess.clone())
        .collect();
    let mut sorted_tools: Vec<String> = tool_set.iter().cloned().collect();
    sorted_tools.sort();

    {
        let sec = printer.section("Scanning dotfiles");
        if dotfiles.is_empty() {
            sec.status(Role::Info, "No dotfiles found");
        } else {
            sec.kv("Entries", dotfiles.len().to_string());
            if !sorted_tools.is_empty() {
                sec.kv("Detected tools", sorted_tools.join(", "));
            }
        }
    }

    let shell_result = generate::scan::scan_shell_config(&detected_shell, &home_path)?;
    {
        let sec = printer.section(format!("Scanning {} config", detected_shell));
        if !shell_result.aliases.is_empty() {
            sec.kv("Aliases", shell_result.aliases.len().to_string());
        }
        if !shell_result.exports.is_empty() {
            sec.kv("Exports", shell_result.exports.len().to_string());
        }
        if !shell_result.path_additions.is_empty() {
            sec.kv(
                "PATH additions",
                shell_result.path_additions.len().to_string(),
            );
        }
        if let Some(pm) = &shell_result.plugin_manager {
            sec.kv("Plugin manager", pm);
        }
    }

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                "Scan complete — use without --scan-only to generate config",
            )
            .with_data(serde_json::json!({
                "target": "scan_only",
                "shell": detected_shell,
                "toolsScanned": sorted_tools,
                "dotfileEntries": dotfiles.len(),
                "aliases": shell_result.aliases.len(),
                "exports": shell_result.exports.len(),
                "pathAdditions": shell_result.path_additions.len(),
                "pluginManager": shell_result.plugin_manager,
                "settingsCaptured": shell_result.aliases.len()
                    + shell_result.exports.len()
                    + shell_result.path_additions.len(),
            })),
    );
    Ok(())
}

fn dirs_from_env() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests;
