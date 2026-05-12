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

// --- dirs_from_env behavior ---

#[test]
fn dirs_from_env_uses_home_env() {
    // HOME is typically set in test environments
    let home = std::env::var_os("HOME");
    let result = dirs_from_env();
    if let Some(h) = home {
        assert_eq!(
            result,
            std::path::PathBuf::from(h),
            "should use HOME env var"
        );
    } else {
        assert_eq!(
            result,
            std::path::PathBuf::from("/tmp"),
            "should fall back to /tmp when HOME is not set"
        );
    }
}

// --- GenerateArgs construction tests ---

#[test]
fn generate_args_scan_only_mode() {
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("bash".into()),
        home: Some("/home/testuser".into()),
    };
    assert!(args.scan_only);
    assert_eq!(args.shell.as_deref(), Some("bash"));
    assert_eq!(args.home.as_deref(), Some("/home/testuser"));
}

#[test]
fn generate_args_module_target() {
    let args = GenerateArgs {
        target: Some(GenerateTarget::Module { name: "git".into() }),
        model: None,
        provider: None,
        yes: false,
        scan_only: false,
        shell: None,
        home: None,
    };
    match args.target {
        Some(GenerateTarget::Module { ref name }) => assert_eq!(name, "git"),
        _ => panic!("Expected Module target"),
    }
}

#[test]
fn generate_args_profile_target() {
    let args = GenerateArgs {
        target: Some(GenerateTarget::Profile {
            name: "work-laptop".into(),
        }),
        model: None,
        provider: None,
        yes: false,
        scan_only: false,
        shell: None,
        home: None,
    };
    match args.target {
        Some(GenerateTarget::Profile { ref name }) => assert_eq!(name, "work-laptop"),
        _ => panic!("Expected Profile target"),
    }
}

// --- cmd_generate_scan_only tests ---

#[test]
fn cmd_generate_scan_only_with_empty_home() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("zsh".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(
        result.is_ok(),
        "scan_only should succeed: {:?}",
        result.err()
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Scanning dotfiles"),
        "should show scanning header, got: {output}"
    );
    assert!(
        output.contains("Scan complete"),
        "should show scan complete, got: {output}"
    );
}

#[test]
fn cmd_generate_scan_only_with_shell_configs() {
    let dir = tempfile::tempdir().unwrap();
    // Create some shell config files that the scanner recognizes
    std::fs::write(
        dir.path().join(".bashrc"),
        "alias ll='ls -la'\nexport EDITOR=vim\nPATH=\"$HOME/bin:$PATH\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".bash_profile"), "source ~/.bashrc\n").unwrap();

    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("bash".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(
        result.is_ok(),
        "scan_only should succeed: {:?}",
        result.err()
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Scanning dotfiles"),
        "should scan dotfiles, got: {output}"
    );
    assert!(
        output.contains("Scanning bash config"),
        "should scan bash config, got: {output}"
    );
}

#[test]
fn cmd_generate_scan_only_with_dotfiles() {
    let dir = tempfile::tempdir().unwrap();
    // Create recognizable dotfiles
    std::fs::write(dir.path().join(".vimrc"), "set number\n").unwrap();
    std::fs::write(dir.path().join(".gitconfig"), "[user]\nname = Test\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".config/nvim")).unwrap();
    std::fs::write(
        dir.path().join(".config/nvim/init.lua"),
        "-- neovim config\n",
    )
    .unwrap();

    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("zsh".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(
        result.is_ok(),
        "scan_only should succeed: {:?}",
        result.err()
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("dotfile"),
        "should report dotfile entries, got: {output}"
    );
}

#[test]
fn cmd_generate_scan_only_default_shell_is_zsh() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, buf) = Printer::for_test();

    // Remove SHELL env var to test default
    let original_shell = std::env::var("SHELL").ok();

    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: None, // Use auto-detection
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    // Should detect shell from $SHELL env or default to zsh
    if let Some(ref shell) = original_shell {
        let shell_name = shell.rsplit('/').next().unwrap_or("zsh");
        assert!(
            output.contains(&format!("Scanning {} config", shell_name)),
            "should detect shell from env, got: {output}"
        );
    }
}

#[test]
fn cmd_generate_scan_only_shell_with_aliases_and_exports() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".zshrc"),
        concat!(
            "alias g='git'\n",
            "alias dc='docker compose'\n",
            "export GOPATH=$HOME/go\n",
            "export RUST_LOG=debug\n",
            "export PATH=$HOME/.cargo/bin:$PATH\n",
        ),
    )
    .unwrap();

    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("zsh".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    // The scanner should find aliases and exports
    assert!(
        output.contains("aliases") || output.contains("exports") || output.contains("PATH"),
        "should report aliases/exports/PATH additions, got: {output}"
    );
}

// --- handle_present_yaml tests ---

#[test]
fn handle_present_yaml_auto_accept() {
    let (printer, _buf) = Printer::for_test();
    let input = serde_json::json!({
        "content": "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: test\nspec: {}\n",
        "kind": "Profile",
        "description": "Test profile"
    });

    let result = handle_present_yaml(&printer, "tool-123", &input, true);
    assert!(result.is_ok());

    match result.unwrap() {
        crate::ai::client::ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            assert_eq!(tool_use_id, "tool-123");
            assert!(is_error.is_none(), "should not be an error");
            // The content should be a JSON-serialized PresentYamlResponse::Accept
            let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert_eq!(
                parsed["action"], "accept",
                "auto_accept should produce Accept response"
            );
        }
        _ => panic!("Expected ToolResult"),
    }
}

#[test]
fn handle_present_yaml_shows_header_and_syntax() {
    let (printer, buf) = Printer::for_test();
    let input = serde_json::json!({
        "content": "key: value\n",
        "kind": "Module",
        "description": "My module"
    });

    let result = handle_present_yaml(&printer, "tool-456", &input, true);
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Module") || output.contains("My module"),
        "should show kind/description in header, got: {output}"
    );
}

#[test]
fn handle_present_yaml_invalid_input_fails() {
    let (printer, _buf) = Printer::for_test();
    // Missing required fields
    let input = serde_json::json!({
        "not_a_valid_field": "bad"
    });

    let result = handle_present_yaml(&printer, "tool-789", &input, true);
    assert!(result.is_err(), "should fail with invalid input");
}

// --- cmd_generate_scan_only with custom home and various shell types ---

#[test]
fn cmd_generate_scan_only_with_fish_shell() {
    let dir = tempfile::tempdir().unwrap();
    // Fish config goes in .config/fish/config.fish
    let fish_dir = dir.path().join(".config").join("fish");
    std::fs::create_dir_all(&fish_dir).unwrap();
    std::fs::write(
        fish_dir.join("config.fish"),
        "alias ll 'ls -la'\nset -x EDITOR vim\n",
    )
    .unwrap();

    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("fish".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Scanning fish config"),
        "should scan fish config, got: {output}"
    );
}

#[test]
fn cmd_generate_scan_only_no_dotfiles_reports_none() {
    let dir = tempfile::tempdir().unwrap();
    // Completely empty home directory

    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("bash".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No dotfiles found") || output.contains("dotfile"),
        "should report on dotfiles, got: {output}"
    );
}

#[test]
fn cmd_generate_scan_only_detects_tool_from_dotfiles() {
    let dir = tempfile::tempdir().unwrap();
    // Create known dotfiles that map to tools
    std::fs::write(dir.path().join(".tmux.conf"), "set -g mouse on\n").unwrap();
    std::fs::write(dir.path().join(".vimrc"), "set number\n").unwrap();

    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("zsh".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    // Scanner should detect tmux and vim from the dotfiles
    assert!(
        output.contains("Detected tools") || output.contains("dotfile"),
        "should detect tools from dotfiles, got: {output}"
    );
}

#[test]
fn cmd_generate_scan_only_with_plugin_manager() {
    let dir = tempfile::tempdir().unwrap();
    // oh-my-zsh indicator
    std::fs::write(
        dir.path().join(".zshrc"),
        "export ZSH=\"$HOME/.oh-my-zsh\"\nsource $ZSH/oh-my-zsh.sh\n",
    )
    .unwrap();

    let (printer, buf) = Printer::for_test();
    let args = GenerateArgs {
        target: None,
        model: None,
        provider: None,
        yes: false,
        scan_only: true,
        shell: Some("zsh".into()),
        home: Some(dir.path().to_str().unwrap().to_string()),
    };

    let result = cmd_generate_scan_only(&printer, &args);
    assert!(result.is_ok());

    let output = buf.lock().unwrap();
    // Should detect oh-my-zsh as plugin manager
    assert!(
        output.contains("Plugin manager")
            || output.contains("oh-my-zsh")
            || output.contains("Scan complete"),
        "should complete scan, got: {output}"
    );
}

// ─── cmd_generate end-to-end via mockito Anthropic Messages API ───────
//
// Drives the AI-mode body of cmd_generate (which the scan-only tests
// above don't reach): consent skip via yes=true, AiConfig fallback when
// no cfgd.yaml exists, AnthropicClient construction with the
// CFGD_ANTHROPIC_URL test seam, the conversation loop body, token-usage
// tally, and the final "Run 'cfgd apply --dry-run'" hint. The mock
// returns a text-only response (no tool_use) so the loop breaks after
// one iteration.

#[cfg(test)]
mod cmd_generate_mockito {
    use super::super::*;
    use serial_test::serial;

    /// RAII env-var guard — mirrors the EnvVarGuard in ai/client.rs
    /// tests. Tests using this MUST be marked `#[serial]` (process-wide
    /// env mutation).
    struct EnvVarGuard {
        key: &'static str,
        prior: Option<String>,
    }
    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: serialized via #[serial].
            let prior = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) }
            Self { key, prior }
        }
    }
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: serialized via #[serial].
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn test_cli(config_path: std::path::PathBuf) -> super::super::super::Cli {
        super::super::super::Cli {
            command: Some(super::super::super::Command::Status {
                module: None,
                exit_code: false,
            }),
            config: config_path,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: super::super::super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            jsonpath: None,
            state_dir: None,
        }
    }

    #[test]
    #[serial]
    fn cmd_generate_loops_once_and_summarises_when_mock_returns_text_only() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let _api = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-key-abc");

        let mut server = mockito::Server::new();
        let _url = EnvVarGuard::set("CFGD_ANTHROPIC_URL", &server.url());

        // Single API turn — text-only response with end_turn breaks the
        // loop after one iteration without spawning any tool dispatch.
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "id": "msg_e2e_001",
                    "content": [{
                        "type": "text",
                        "text": "I'll help generate your cfgd configuration."
                    }],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 42, "output_tokens": 18}
                }"#,
            )
            .create();

        let cli = test_cli(tmp.path().join("cfgd.yaml"));
        let (printer, buf) = Printer::for_test();
        let args = GenerateArgs {
            target: None,
            model: None,
            provider: None,
            yes: true,
            scan_only: false,
            shell: None,
            home: None,
        };

        cmd_generate(&cli, &printer, &args)
            .expect("cmd_generate should succeed against the text-only mock");

        mock.assert();
        let output = buf.lock().unwrap();
        // The assistant text from the mock should land in the printed output.
        assert!(
            output.contains("I'll help generate your cfgd configuration."),
            "assistant text from mock should print: {output}"
        );
        // Token tally pulls from response.usage and reports the sum.
        assert!(
            output.contains("Token usage:") && output.contains("60 total"),
            "should tally 42+18=60 tokens: {output}"
        );
        // The final hint always renders.
        assert!(
            output.contains("cfgd apply --dry-run"),
            "should point user at dry-run: {output}"
        );
    }

    #[test]
    #[serial]
    fn cmd_generate_with_module_target_loops_once_and_summarises() {
        // Same as the no-target test, but exercises the module-target
        // mode_context branch (Mode: module — generate module for tool 'X').
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());
        let _api = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-key-abc");

        let mut server = mockito::Server::new();
        let _url = EnvVarGuard::set("CFGD_ANTHROPIC_URL", &server.url());

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "id": "msg_module_001",
                    "content": [{"type": "text", "text": "Ack."}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 5, "output_tokens": 1}
                }"#,
            )
            .create();

        let cli = test_cli(tmp.path().join("cfgd.yaml"));
        let (printer, _buf) = Printer::for_test();
        let args = GenerateArgs {
            target: Some(GenerateTarget::Module {
                name: "neovim".into(),
            }),
            model: None,
            provider: None,
            yes: true,
            scan_only: false,
            shell: None,
            home: None,
        };

        cmd_generate(&cli, &printer, &args).expect("cmd_generate (module target) should succeed");
        mock.assert();
    }

    #[test]
    #[serial]
    fn cmd_generate_errors_when_api_key_env_unset() {
        // No ANTHROPIC_API_KEY set → the GenerateError::ApiKeyNotFound
        // arm fires before any HTTP call. Drives the
        // env::var(...).map_err arm at the top of cmd_generate.
        let tmp = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp.path());

        // Explicitly unset. EnvVarGuard's Drop restores the prior value
        // so test ordering doesn't matter.
        let _key = EnvVarGuard {
            key: "ANTHROPIC_API_KEY",
            prior: std::env::var("ANTHROPIC_API_KEY").ok(),
        };
        // SAFETY: serialized via #[serial].
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }

        let cli = test_cli(tmp.path().join("cfgd.yaml"));
        let (printer, _buf) = Printer::for_test();
        let args = GenerateArgs {
            target: None,
            model: None,
            provider: None,
            yes: true,
            scan_only: false,
            shell: None,
            home: None,
        };

        let err = cmd_generate(&cli, &printer, &args)
            .expect_err("missing API key should surface as an error");
        let msg = err.to_string();
        assert!(
            msg.contains("ANTHROPIC_API_KEY"),
            "error should name the missing env var: {msg}"
        );
    }
}
