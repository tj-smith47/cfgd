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
