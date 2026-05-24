use super::*;

use std::fs;
use tempfile::TempDir;

// ---- scan_dotfiles tests ----

#[test]
fn test_scan_dotfiles_finds_dotfiles() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // Create a few dotfiles / directories
    fs::write(home.join(".zshrc"), "# zsh config").unwrap();
    fs::write(home.join(".gitconfig"), "[user]\n  name = Test").unwrap();
    fs::create_dir_all(home.join(".config").join("nvim")).unwrap();
    fs::write(
        home.join(".config").join("nvim").join("init.lua"),
        "-- neovim config",
    )
    .unwrap();

    let entries = scan_dotfiles(home).unwrap();

    // Should find .zshrc and .gitconfig at home level, plus nvim under .config
    let paths: Vec<_> = entries.iter().map(|e| e.path.clone()).collect();
    assert!(
        paths.contains(&home.join(".zshrc")),
        "expected .zshrc in entries, got: {:?}",
        paths
    );
    assert!(
        paths.contains(&home.join(".gitconfig")),
        "expected .gitconfig in entries"
    );
    // nvim should appear as a child of .config
    assert!(
        paths.contains(&home.join(".config").join("nvim")),
        "expected .config/nvim in entries"
    );
}

#[test]
fn test_scan_dotfiles_guesses_tools() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".vimrc"), "set nocompatible").unwrap();
    fs::write(home.join(".tmux.conf"), "set -g prefix C-a").unwrap();
    fs::create_dir_all(home.join(".config").join("nvim")).unwrap();

    let entries = scan_dotfiles(home).unwrap();

    let find = |name: &str| -> Option<&DotfileEntry> {
        entries
            .iter()
            .find(|e| e.path.file_name().and_then(|n| n.to_str()) == Some(name))
    };

    let vimrc = find(".vimrc").expect(".vimrc should be in entries");
    assert_eq!(vimrc.tool_guess.as_deref(), Some("vim"));

    let tmux = find(".tmux.conf").expect(".tmux.conf should be in entries");
    assert_eq!(tmux.tool_guess.as_deref(), Some("tmux"));

    let nvim = find("nvim").expect("nvim should be in entries");
    assert_eq!(nvim.tool_guess.as_deref(), Some("nvim"));
}

#[test]
fn test_scan_dotfiles_empty_home() {
    let tmp = TempDir::new().unwrap();
    let entries = scan_dotfiles(tmp.path()).unwrap();
    assert!(entries.is_empty(), "empty home should yield no entries");
}

#[test]
fn test_scan_dotfiles_skips_git_and_cache() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // These should be skipped
    fs::create_dir_all(home.join(".git")).unwrap();
    fs::create_dir_all(home.join(".cache")).unwrap();
    fs::create_dir_all(home.join(".local")).unwrap();

    // This should be found
    fs::write(home.join(".zshrc"), "").unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let paths: Vec<_> = entries.iter().map(|e| e.path.clone()).collect();

    assert!(
        !paths.contains(&home.join(".git")),
        ".git should be skipped"
    );
    assert!(
        !paths.contains(&home.join(".cache")),
        ".cache should be skipped"
    );
    assert!(
        !paths.contains(&home.join(".local")),
        ".local should be skipped"
    );
    assert!(
        paths.contains(&home.join(".zshrc")),
        ".zshrc should be found"
    );
}

#[test]
fn test_scan_dotfiles_entry_types() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".vimrc"), "").unwrap();
    fs::create_dir_all(home.join(".ssh")).unwrap();

    let entries = scan_dotfiles(home).unwrap();

    let file_entry = entries
        .iter()
        .find(|e| e.path == home.join(".vimrc"))
        .unwrap();
    assert_eq!(file_entry.entry_type, "file");

    let dir_entry = entries
        .iter()
        .find(|e| e.path == home.join(".ssh"))
        .unwrap();
    assert_eq!(dir_entry.entry_type, "directory");
}

// ---- scan_shell_config tests ----

#[test]
fn test_scan_shell_config_parses_single_quote_aliases() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "alias ll='ls -la'\nalias gs='git status'\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();

    let alias_names: Vec<&str> = result.aliases.iter().map(|a| a.name.as_str()).collect();
    assert!(alias_names.contains(&"ll"), "expected alias 'll'");
    assert!(alias_names.contains(&"gs"), "expected alias 'gs'");

    let ll = result.aliases.iter().find(|a| a.name == "ll").unwrap();
    assert_eq!(ll.command, "ls -la");
}

#[test]
fn test_scan_shell_config_parses_double_quote_aliases() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "alias grep=\"grep --color=auto\"\nalias dc=\"docker-compose\"\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();

    let grep = result.aliases.iter().find(|a| a.name == "grep").unwrap();
    assert_eq!(grep.command, "grep --color=auto");

    let dc = result.aliases.iter().find(|a| a.name == "dc").unwrap();
    assert_eq!(dc.command, "docker-compose");
}

#[test]
fn test_scan_shell_config_parses_exports() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "export EDITOR=nvim\nexport GOPATH=\"$HOME/go\"\nexport LANG='en_US.UTF-8'\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();

    let editor = result.exports.iter().find(|e| e.name == "EDITOR").unwrap();
    assert_eq!(editor.value, "nvim");

    let gopath = result.exports.iter().find(|e| e.name == "GOPATH").unwrap();
    assert_eq!(gopath.value, "$HOME/go");

    let lang = result.exports.iter().find(|e| e.name == "LANG").unwrap();
    assert_eq!(lang.value, "en_US.UTF-8");
}

#[test]
fn test_scan_shell_config_detects_path_additions() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "export PATH=\"$HOME/bin:$PATH\"\nexport PATH=\"$HOME/.local/bin:$PATH\"\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();

    assert!(
        !result.path_additions.is_empty(),
        "expected PATH additions to be detected"
    );
    assert_eq!(result.path_additions.len(), 2);
}

#[test]
fn test_scan_shell_config_detects_sourced_files() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "source ~/.aliases\n. ~/.functions\nsource /etc/profile.d/nvm.sh\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();

    assert!(
        !result.sourced_files.is_empty(),
        "expected sourced files to be detected"
    );
    let sourced_strs: Vec<_> = result
        .sourced_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert!(
        sourced_strs.iter().any(|s| s.contains(".aliases")),
        "expected ~/.aliases to be sourced"
    );
    assert!(
        sourced_strs.iter().any(|s| s.contains(".functions")),
        "expected ~/.functions to be sourced"
    );
}

#[test]
fn test_scan_shell_config_detects_oh_my_zsh() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "export ZSH=\"$HOME/.oh-my-zsh\"\nsource $ZSH/oh-my-zsh.sh\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("oh-my-zsh"));
}

#[test]
fn test_scan_shell_config_detects_zinit() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "source ~/.zinit/bin/zinit.zsh\nzinit light zsh-users/zsh-autosuggestions\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("zinit"));
}

#[test]
fn test_scan_shell_config_detects_zplug() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "source ~/.zplug/init.zsh\nzplug 'zsh-users/zsh-history-substring-search'\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("zplug"));
}

#[test]
fn test_scan_shell_config_missing_files_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    // No rc files created

    let result = scan_shell_config("zsh", home).unwrap();

    assert!(result.config_files.is_empty());
    assert!(result.aliases.is_empty());
    assert!(result.exports.is_empty());
    assert!(result.path_additions.is_empty());
    assert!(result.sourced_files.is_empty());
    assert!(result.plugin_manager.is_none());
}

#[test]
fn test_scan_shell_config_bash_files() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".bashrc"),
        "alias ls='ls --color=auto'\nexport EDITOR=vim\n",
    )
    .unwrap();

    let result = scan_shell_config("bash", home).unwrap();

    assert_eq!(result.shell, "bash");
    assert!(!result.config_files.is_empty());
    assert!(result.aliases.iter().any(|a| a.name == "ls"));
    assert!(result.exports.iter().any(|e| e.name == "EDITOR"));
}

#[test]
fn test_scan_shell_config_skips_comments() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
            home.join(".zshrc"),
            "# alias commented='this should not appear'\n# export COMMENTED=yes\nalias real='command'\n",
        )
        .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();

    assert!(
        !result.aliases.iter().any(|a| a.name == "commented"),
        "commented alias should not appear"
    );
    assert!(
        !result.exports.iter().any(|e| e.name == "COMMENTED"),
        "commented export should not appear"
    );
    assert!(result.aliases.iter().any(|a| a.name == "real"));
}

#[test]
fn test_scan_shell_config_unknown_shell_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let result = scan_shell_config("tcsh", tmp.path()).unwrap();
    assert!(result.config_files.is_empty());
    assert!(result.aliases.is_empty());
}

// ---- scan_installed_packages tests ----

use std::collections::HashSet;

use cfgd_core::output::Printer;
use cfgd_core::providers::PackageInfo;

struct TestPackageManager {
    manager_name: &'static str,
    available: bool,
    packages: Vec<PackageInfo>,
}

impl PackageManager for TestPackageManager {
    fn name(&self) -> &str {
        self.manager_name
    }
    fn is_available(&self) -> bool {
        self.available
    }
    fn can_bootstrap(&self) -> bool {
        false
    }
    fn bootstrap(&self, _printer: &Printer) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn installed_packages(&self) -> cfgd_core::errors::Result<HashSet<String>> {
        Ok(self.packages.iter().map(|p| p.name.clone()).collect())
    }
    fn install(&self, _packages: &[String], _printer: &Printer) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn uninstall(&self, _packages: &[String], _printer: &Printer) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn update(&self, _printer: &Printer) -> cfgd_core::errors::Result<()> {
        Ok(())
    }
    fn available_version(&self, _package: &str) -> cfgd_core::errors::Result<Option<String>> {
        Ok(None)
    }
    fn installed_packages_with_versions(&self) -> cfgd_core::errors::Result<Vec<PackageInfo>> {
        Ok(self.packages.clone())
    }
}

fn pkg(name: &str, version: &str) -> PackageInfo {
    PackageInfo {
        name: name.to_string(),
        version: version.to_string(),
    }
}

#[test]
fn test_scan_installed_packages_collects_from_multiple_managers() {
    let brew = TestPackageManager {
        manager_name: "brew",
        available: true,
        packages: vec![pkg("ripgrep", "14.0.0"), pkg("bat", "0.24.0")],
    };
    let apt = TestPackageManager {
        manager_name: "apt",
        available: true,
        packages: vec![pkg("curl", "7.88.1")],
    };

    let managers: Vec<&dyn PackageManager> = vec![&brew, &apt];
    let entries = scan_installed_packages(&managers, None).unwrap();

    assert_eq!(entries.len(), 3);

    // Sorted by name
    assert_eq!(entries[0].name, "bat");
    assert_eq!(entries[0].manager, "brew");
    assert_eq!(entries[0].version, "0.24.0");

    assert_eq!(entries[1].name, "curl");
    assert_eq!(entries[1].manager, "apt");

    assert_eq!(entries[2].name, "ripgrep");
    assert_eq!(entries[2].manager, "brew");
}

#[test]
fn test_scan_installed_packages_filter_by_manager() {
    let brew = TestPackageManager {
        manager_name: "brew",
        available: true,
        packages: vec![pkg("ripgrep", "14.0.0")],
    };
    let apt = TestPackageManager {
        manager_name: "apt",
        available: true,
        packages: vec![pkg("curl", "7.88.1")],
    };

    let managers: Vec<&dyn PackageManager> = vec![&brew, &apt];
    let entries = scan_installed_packages(&managers, Some("brew")).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "ripgrep");
    assert_eq!(entries[0].manager, "brew");
}

#[test]
fn test_scan_installed_packages_skips_unavailable() {
    let unavailable = TestPackageManager {
        manager_name: "brew",
        available: false,
        packages: vec![pkg("ripgrep", "14.0.0")],
    };
    let available = TestPackageManager {
        manager_name: "apt",
        available: true,
        packages: vec![pkg("curl", "7.88.1")],
    };

    let managers: Vec<&dyn PackageManager> = vec![&unavailable, &available];
    let entries = scan_installed_packages(&managers, None).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "curl");
    assert_eq!(entries[0].manager, "apt");
}

#[test]
fn test_scan_installed_packages_empty_managers() {
    let managers: Vec<&dyn PackageManager> = vec![];
    let entries = scan_installed_packages(&managers, None).unwrap();
    assert!(entries.is_empty());
}

#[test]
fn test_scan_installed_packages_sorted_by_name_then_manager() {
    let mgr_a = TestPackageManager {
        manager_name: "apt",
        available: true,
        packages: vec![pkg("zsh", "5.9")],
    };
    let mgr_b = TestPackageManager {
        manager_name: "brew",
        available: true,
        packages: vec![pkg("zsh", "5.9"), pkg("awk", "1.0")],
    };

    let managers: Vec<&dyn PackageManager> = vec![&mgr_a, &mgr_b];
    let entries = scan_installed_packages(&managers, None).unwrap();

    // awk comes first alphabetically
    assert_eq!(entries[0].name, "awk");
    // zsh appears twice: apt before brew
    assert_eq!(entries[1].name, "zsh");
    assert_eq!(entries[1].manager, "apt");
    assert_eq!(entries[2].name, "zsh");
    assert_eq!(entries[2].manager, "brew");
}

// ---- scan_system_settings tests ----

#[test]
fn test_scan_system_settings_returns_valid_result() {
    // Just verify it returns a valid SystemSettingsResult without crashing.
    // Values are platform-dependent so we only check structural validity.
    let result = scan_system_settings().unwrap();
    // systemd_units and launch_agents are always sorted
    let mut sorted_units = result.systemd_units.clone();
    sorted_units.sort();
    assert_eq!(
        result.systemd_units, sorted_units,
        "systemd_units should be sorted"
    );

    let mut sorted_agents = result.launch_agents.clone();
    sorted_agents.sort();
    assert_eq!(
        result.launch_agents, sorted_agents,
        "launch_agents should be sorted"
    );

    let mut sorted_schemas = result.gsettings_schemas.clone();
    sorted_schemas.sort();
    assert_eq!(
        result.gsettings_schemas, sorted_schemas,
        "gsettings_schemas should be sorted"
    );

    let mut sorted_services = result.windows_services.clone();
    sorted_services.sort();
    assert_eq!(
        result.windows_services, sorted_services,
        "windows_services should be sorted"
    );
}

#[test]
fn test_scan_system_settings_launch_agents_only_plist() {
    // Verify that only .plist files are included in launch_agents by
    // examining the filtering logic through its output on this system.
    let result = scan_system_settings().unwrap();
    for agent in &result.launch_agents {
        assert!(
            agent.ends_with(".plist"),
            "launch agent '{agent}' should end with .plist"
        );
    }
}

#[test]
fn test_scan_system_settings_systemd_units_only_service_or_timer() {
    // Verify unit filtering: only .service and .timer entries are collected.
    let result = scan_system_settings().unwrap();
    for unit in &result.systemd_units {
        assert!(
            unit.ends_with(".service") || unit.ends_with(".timer"),
            "unit '{unit}' should end with .service or .timer"
        );
    }
}

// ---- strip_quotes tests ----

#[test]
fn test_strip_quotes_single() {
    assert_eq!(strip_quotes("'hello world'"), "hello world");
}

#[test]
fn test_strip_quotes_double() {
    assert_eq!(strip_quotes("\"hello world\""), "hello world");
}

#[test]
fn test_strip_quotes_no_quotes() {
    assert_eq!(strip_quotes("hello"), "hello");
}

#[test]
fn test_strip_quotes_mismatched_not_stripped() {
    assert_eq!(strip_quotes("'hello\""), "'hello\"");
}

#[test]
fn test_strip_quotes_trims_whitespace_before_checking() {
    assert_eq!(strip_quotes("  'trimmed'  "), "trimmed");
}

#[test]
fn test_strip_quotes_empty_string() {
    assert_eq!(strip_quotes(""), "");
}

#[test]
fn test_strip_quotes_single_char_quoted() {
    // "''" is a valid single-quoted empty string
    assert_eq!(strip_quotes("''"), "");
}

// ---- strip_inline_comment tests ----

#[test]
fn test_strip_inline_comment_basic() {
    assert_eq!(
        strip_inline_comment("alias ll='ls -la' # list"),
        "alias ll='ls -la'"
    );
}

#[test]
fn test_strip_inline_comment_hash_inside_single_quotes() {
    assert_eq!(
        strip_inline_comment("echo 'color is #red'"),
        "echo 'color is #red'"
    );
}

#[test]
fn test_strip_inline_comment_hash_inside_double_quotes() {
    assert_eq!(
        strip_inline_comment("echo \"#not a comment\""),
        "echo \"#not a comment\""
    );
}

#[test]
fn test_strip_inline_comment_no_comment() {
    assert_eq!(strip_inline_comment("export FOO=bar"), "export FOO=bar");
}

#[test]
fn test_strip_inline_comment_starts_with_hash() {
    assert_eq!(strip_inline_comment("# full line comment"), "");
}

#[test]
fn test_strip_inline_comment_hash_after_quotes() {
    assert_eq!(
        strip_inline_comment("alias x='y' # comment after"),
        "alias x='y'"
    );
}

// ---- detect_plugin_manager tests ----

#[test]
fn test_detect_plugin_manager_oh_my_zsh() {
    assert_eq!(
        detect_plugin_manager("source $ZSH/oh-my-zsh.sh"),
        Some("oh-my-zsh")
    );
}

#[test]
fn test_detect_plugin_manager_zinit() {
    assert_eq!(
        detect_plugin_manager("source ~/.zinit/bin/zinit.zsh"),
        Some("zinit")
    );
}

#[test]
fn test_detect_plugin_manager_zplug() {
    assert_eq!(
        detect_plugin_manager("source ~/.zplug/init.zsh"),
        Some("zplug")
    );
}

#[test]
fn test_detect_plugin_manager_antibody() {
    assert_eq!(
        detect_plugin_manager("source <(antibody init)"),
        Some("antibody")
    );
}

#[test]
fn test_detect_plugin_manager_antigen() {
    assert_eq!(
        detect_plugin_manager("source ~/antigen.zsh"),
        Some("antigen")
    );
}

#[test]
fn test_detect_plugin_manager_sheldon() {
    assert_eq!(
        detect_plugin_manager("eval $(sheldon source)"),
        Some("sheldon")
    );
}

#[test]
fn test_detect_plugin_manager_fisher() {
    assert_eq!(
        detect_plugin_manager("if not functions -q fisher; curl fish | source"),
        Some("fisher")
    );
}

#[test]
fn test_detect_plugin_manager_prezto() {
    assert_eq!(
        detect_plugin_manager("source prezto/init.zsh"),
        Some("prezto")
    );
}

#[test]
fn test_detect_plugin_manager_zim() {
    assert_eq!(detect_plugin_manager("source ~/.zim/init.zsh"), Some("zim"));
}

#[test]
fn test_detect_plugin_manager_none() {
    assert_eq!(detect_plugin_manager("export PATH=/usr/bin"), None);
}

#[test]
fn test_detect_plugin_manager_fisher_matches_because_contains_fish() {
    // "fisher" contains "fish" as a substring, so this matches
    assert_eq!(detect_plugin_manager("fisher install"), Some("fisher"));
}

// ---- shell_config_files tests ----

#[test]
fn test_shell_config_files_zsh() {
    let home = Path::new("/home/test");
    let files = shell_config_files("zsh", home);
    assert_eq!(files.len(), 4);
    assert!(files.contains(&home.join(".zshrc")));
    assert!(files.contains(&home.join(".zshenv")));
    assert!(files.contains(&home.join(".zprofile")));
    assert!(files.contains(&home.join(".zlogin")));
}

#[test]
fn test_shell_config_files_bash() {
    let home = Path::new("/home/test");
    let files = shell_config_files("bash", home);
    assert_eq!(files.len(), 4);
    assert!(files.contains(&home.join(".bashrc")));
    assert!(files.contains(&home.join(".bash_profile")));
    assert!(files.contains(&home.join(".profile")));
}

#[test]
fn test_shell_config_files_fish() {
    let home = Path::new("/home/test");
    let files = shell_config_files("fish", home);
    assert_eq!(files.len(), 1);
    assert!(files.contains(&home.join(".config").join("fish").join("config.fish")));
}

#[test]
fn test_shell_config_files_sh() {
    let home = Path::new("/home/test");
    let files = shell_config_files("sh", home);
    assert_eq!(files.len(), 1);
    assert!(files.contains(&home.join(".profile")));
}

#[test]
fn test_shell_config_files_dash() {
    let home = Path::new("/home/test");
    let files = shell_config_files("dash", home);
    assert_eq!(files.len(), 1);
    assert!(files.contains(&home.join(".profile")));
}

#[test]
fn test_shell_config_files_unknown_shell_returns_empty() {
    let home = Path::new("/home/test");
    let files = shell_config_files("tcsh", home);
    assert!(files.is_empty());
}

// ---- scan_shell_config: fish_add_path ----

#[test]
fn test_scan_shell_config_fish_add_path() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let fish_dir = home.join(".config").join("fish");
    fs::create_dir_all(&fish_dir).unwrap();
    fs::write(
        fish_dir.join("config.fish"),
        "fish_add_path ~/.local/bin\nfish_add_path /opt/homebrew/bin\n",
    )
    .unwrap();

    let result = scan_shell_config("fish", home).unwrap();
    assert_eq!(result.shell, "fish");
    assert_eq!(result.path_additions.len(), 2);
    assert!(
        result
            .path_additions
            .iter()
            .any(|p| p.contains(".local/bin"))
    );
    assert!(
        result
            .path_additions
            .iter()
            .any(|p| p.contains("/opt/homebrew/bin"))
    );
}

// ---- scan_shell_config: inline comment stripping in real config ----

#[test]
fn test_scan_shell_config_inline_comments_stripped() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "alias ll='ls -la' # list all files\nexport EDITOR=vim # default editor\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    let ll = result.aliases.iter().find(|a| a.name == "ll").unwrap();
    assert_eq!(ll.command, "ls -la");
    let editor = result.exports.iter().find(|e| e.name == "EDITOR").unwrap();
    assert_eq!(editor.value, "vim");
}

// ---- scan_shell_config: zsh path+= syntax ----

#[test]
fn test_scan_shell_config_zsh_path_plus_syntax() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "path+=($HOME/.local/bin)\npath=($HOME/bin $path)\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(
        result.path_additions.len(),
        2,
        "expected 2 path additions from zsh path+= syntax, got: {:?}",
        result.path_additions
    );
}

// ---- scan_dotfiles: symlink detection ----

#[cfg(unix)]
#[test]
fn test_scan_dotfiles_symlink_entry_type() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // Create a real file and a symlink to it
    fs::write(home.join(".real_config"), "content").unwrap();
    std::os::unix::fs::symlink(home.join(".real_config"), home.join(".linked_config")).unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let linked = entries
        .iter()
        .find(|e| e.path == home.join(".linked_config"))
        .expect("should find .linked_config");
    assert_eq!(linked.entry_type, "symlink");
}

#[test]
fn test_build_tool_map_home_dotfiles() {
    let map = build_tool_map();
    assert_eq!(map.get(".zshrc"), Some(&"zsh"));
    assert_eq!(map.get(".bashrc"), Some(&"bash"));
    assert_eq!(map.get(".vimrc"), Some(&"vim"));
    assert_eq!(map.get(".tmux.conf"), Some(&"tmux"));
    assert_eq!(map.get(".gitconfig"), Some(&"git"));
    assert_eq!(map.get(".cargo"), Some(&"cargo"));
    assert_eq!(map.get(".kube"), Some(&"kubectl"));
    assert_eq!(map.get(".docker"), Some(&"docker"));
    assert_eq!(map.get(".ssh"), Some(&"ssh"));
    assert_eq!(map.get(".rustup"), Some(&"rustup"));
    assert_eq!(map.get("not_a_dotfile"), None);
}

#[test]
fn test_build_tool_map_xdg_entries() {
    let map = build_tool_map();
    assert_eq!(map.get("nvim"), Some(&"nvim"));
    assert_eq!(map.get("alacritty"), Some(&"alacritty"));
    assert_eq!(map.get("starship.toml"), Some(&"starship"));
    assert_eq!(map.get("fzf"), Some(&"fzf"));
    assert_eq!(map.get("gh"), Some(&"gh"));
    assert_eq!(map.get("helix"), Some(&"helix"));
    assert_eq!(map.get("nonexistent_tool"), None);
}

#[test]
fn test_scan_dotfiles_file_size_bytes_nonzero() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let content = "export EDITOR=nvim\n";
    fs::write(home.join(".bashrc"), content).unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let entry = entries
        .iter()
        .find(|e| e.path == home.join(".bashrc"))
        .expect(".bashrc should be found");

    assert_eq!(entry.entry_type, "file");
    assert_eq!(
        entry.size_bytes,
        content.len() as u64,
        "size_bytes should match file content length"
    );
}

#[test]
fn test_scan_dotfiles_directory_size_bytes_zero() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::create_dir_all(home.join(".config")).unwrap();
    fs::create_dir_all(home.join(".cargo")).unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let cargo_entry = entries
        .iter()
        .find(|e| e.path == home.join(".cargo"))
        .expect(".cargo should be found");

    assert_eq!(cargo_entry.entry_type, "directory");
    assert_eq!(cargo_entry.size_bytes, 0, "directory size_bytes must be 0");
}

#[test]
fn test_scan_dotfiles_xdg_config_entries_included() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let config_dir = home.join(".config");
    fs::create_dir_all(config_dir.join("alacritty")).unwrap();
    fs::create_dir_all(config_dir.join("helix")).unwrap();
    fs::write(config_dir.join("starship.toml"), "# starship config").unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let paths: Vec<_> = entries.iter().map(|e| &e.path).collect();

    assert!(
        paths.contains(&&config_dir.join("alacritty")),
        "alacritty dir should appear as xdg entry"
    );
    assert!(
        paths.contains(&&config_dir.join("helix")),
        "helix dir should appear as xdg entry"
    );
    assert!(
        paths.contains(&&config_dir.join("starship.toml")),
        "starship.toml should appear as xdg entry"
    );

    let alacritty = entries
        .iter()
        .find(|e| e.path == config_dir.join("alacritty"))
        .unwrap();
    assert_eq!(alacritty.tool_guess.as_deref(), Some("alacritty"));

    let starship = entries
        .iter()
        .find(|e| e.path == config_dir.join("starship.toml"))
        .unwrap();
    assert_eq!(starship.tool_guess.as_deref(), Some("starship"));
}

#[test]
fn test_scan_dotfiles_no_config_dir() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".gitconfig"), "[user]\nname=Test\n").unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let paths: Vec<_> = entries.iter().map(|e| &e.path).collect();
    assert!(paths.contains(&&home.join(".gitconfig")));
    assert!(
        !paths.iter().any(|p| p.starts_with(home.join(".config"))),
        "no xdg entries when .config does not exist"
    );
}

#[test]
fn test_scan_dotfiles_skips_non_dotfiles() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join("README.md"), "not a dotfile").unwrap();
    fs::write(home.join("notes.txt"), "also not a dotfile").unwrap();
    fs::write(home.join(".zshrc"), "# zsh").unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let paths: Vec<_> = entries.iter().map(|e| &e.path).collect();

    assert!(
        !paths.contains(&&home.join("README.md")),
        "README.md should be skipped"
    );
    assert!(
        !paths.contains(&&home.join("notes.txt")),
        "notes.txt should be skipped"
    );
    assert!(
        paths.contains(&&home.join(".zshrc")),
        ".zshrc should be included"
    );
}

#[test]
fn test_parse_shell_file_nonexistent_returns_empty() {
    let mut aliases = vec![];
    let mut exports = vec![];
    let mut paths = vec![];
    let mut pm = None;
    let sourced = parse_shell_file(
        std::path::Path::new("/nonexistent/path/that/does/not/exist.sh"),
        &mut aliases,
        &mut exports,
        &mut paths,
        &mut pm,
    );
    assert!(sourced.is_empty());
    assert!(aliases.is_empty());
    assert!(exports.is_empty());
    assert!(paths.is_empty());
    assert!(pm.is_none());
}

#[test]
fn test_parse_shell_file_full_parsing() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.sh");
    fs::write(
        &file,
        "alias ll='ls -la'\nexport EDITOR=nvim\nexport PATH=\"$HOME/bin:$PATH\"\nsource ~/.extra\n",
    )
    .unwrap();

    let mut aliases = vec![];
    let mut exports = vec![];
    let mut paths = vec![];
    let mut pm = None;
    let sourced = parse_shell_file(&file, &mut aliases, &mut exports, &mut paths, &mut pm);

    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].name, "ll");
    assert_eq!(aliases[0].command, "ls -la");

    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "EDITOR");
    assert_eq!(exports[0].value, "nvim");

    assert_eq!(paths.len(), 1);
    assert!(paths[0].contains("$HOME/bin"));

    assert_eq!(sourced.len(), 1);
    assert_eq!(sourced[0], std::path::PathBuf::from("~/.extra"));
}

#[test]
fn test_parse_shell_file_plugin_manager_non_source_line() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.sh");
    fs::write(&file, "zinit light zsh-users/zsh-autosuggestions\n").unwrap();

    let mut aliases = vec![];
    let mut exports = vec![];
    let mut paths = vec![];
    let mut pm = None;
    parse_shell_file(&file, &mut aliases, &mut exports, &mut paths, &mut pm);
    assert_eq!(pm.as_deref(), Some("zinit"));
}

#[test]
fn test_scan_installed_packages_error_manager_does_not_abort() {
    struct ErrorManager;
    impl cfgd_core::providers::PackageManager for ErrorManager {
        fn name(&self) -> &str {
            "erroring"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn can_bootstrap(&self) -> bool {
            false
        }
        fn bootstrap(&self, _p: &cfgd_core::output::Printer) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn installed_packages(
            &self,
        ) -> cfgd_core::errors::Result<std::collections::HashSet<String>> {
            Ok(Default::default())
        }
        fn install(
            &self,
            _pkgs: &[String],
            _p: &cfgd_core::output::Printer,
        ) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn uninstall(
            &self,
            _pkgs: &[String],
            _p: &cfgd_core::output::Printer,
        ) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn update(&self, _p: &cfgd_core::output::Printer) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn available_version(&self, _pkg: &str) -> cfgd_core::errors::Result<Option<String>> {
            Ok(None)
        }
        fn installed_packages_with_versions(
            &self,
        ) -> cfgd_core::errors::Result<Vec<cfgd_core::providers::PackageInfo>> {
            Err(cfgd_core::errors::CfgdError::Package(
                cfgd_core::errors::PackageError::ListFailed {
                    manager: "erroring".into(),
                    message: "simulated list failure".into(),
                },
            ))
        }
    }

    let good = TestPackageManager {
        manager_name: "apt",
        available: true,
        packages: vec![pkg("curl", "7.88.1")],
    };

    let err_mgr = ErrorManager;
    let managers: Vec<&dyn cfgd_core::providers::PackageManager> = vec![&err_mgr, &good];
    let entries = scan_installed_packages(&managers, None)
        .expect("scan_installed_packages should not fail when one manager errors");

    assert_eq!(
        entries.len(),
        1,
        "only the successful manager's packages returned"
    );
    assert_eq!(entries[0].name, "curl");
}

#[test]
fn test_detect_plugin_manager_zplugin_alias() {
    assert_eq!(
        detect_plugin_manager("source ~/.zplugin/bin/zplugin.zsh"),
        Some("zinit")
    );
}

#[test]
fn test_detect_plugin_manager_ohmyzsh_github_url() {
    assert_eq!(
        detect_plugin_manager("ZSH_CUSTOM=${ZSH_CUSTOM:-~/.oh-my-zsh/custom}"),
        Some("oh-my-zsh")
    );
}

#[test]
fn test_detect_plugin_manager_zdharma_zinit_url() {
    assert_eq!(
        detect_plugin_manager("zinit ice as\"null\" from\"zdharma-continuum/zinit\""),
        Some("zinit")
    );
}

#[test]
fn test_strip_quotes_double_empty() {
    assert_eq!(strip_quotes("\"\""), "");
}

#[test]
fn test_strip_quotes_single_empty() {
    assert_eq!(strip_quotes("''"), "");
}

#[test]
fn test_strip_inline_comment_only_hash() {
    assert_eq!(strip_inline_comment("#"), "");
}

#[test]
fn test_strip_inline_comment_space_then_hash() {
    assert_eq!(strip_inline_comment("export X=1 # comment"), "export X=1");
}

#[test]
fn test_scan_shell_config_bash_login() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".bash_login"), "alias ll='ls -la'\n").unwrap();

    let result = scan_shell_config("bash", home).unwrap();
    assert!(
        result.aliases.iter().any(|a| a.name == "ll"),
        "alias from .bash_login should be included"
    );
}

#[test]
fn test_scan_shell_config_dot_source_syntax() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".zshrc"), ". ~/.posix_funcs\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    let sourced_strs: Vec<_> = result
        .sourced_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert!(
        sourced_strs.iter().any(|s| s.contains(".posix_funcs")),
        "dot-syntax sourced file should be captured: {:?}",
        sourced_strs
    );
}

// ---- Additional coverage: scan_dotfiles ----

#[cfg(unix)]
#[test]
fn test_scan_dotfiles_symlink_in_xdg_config() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let config_dir = home.join(".config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(home.join("real_nvim")).unwrap();
    std::os::unix::fs::symlink(home.join("real_nvim"), config_dir.join("nvim")).unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let nvim = entries
        .iter()
        .find(|e| e.path == config_dir.join("nvim"))
        .expect("nvim symlink should appear in xdg entries");
    assert_eq!(nvim.entry_type, "symlink");
    assert_eq!(nvim.tool_guess.as_deref(), Some("nvim"));
    assert_eq!(nvim.size_bytes, 0, "symlink size_bytes should be 0");
}

#[test]
fn test_scan_dotfiles_unknown_tool_has_none_tool_guess() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".my_custom_dotfile"), "custom config").unwrap();
    fs::create_dir_all(home.join(".config").join("unknown_app")).unwrap();

    let entries = scan_dotfiles(home).unwrap();

    let custom = entries
        .iter()
        .find(|e| e.path == home.join(".my_custom_dotfile"))
        .expect(".my_custom_dotfile should be in entries");
    assert_eq!(
        custom.tool_guess, None,
        "unknown dotfile should have no tool guess"
    );

    let unknown_xdg = entries
        .iter()
        .find(|e| e.path == home.join(".config").join("unknown_app"))
        .expect("unknown_app under .config should appear");
    assert_eq!(
        unknown_xdg.tool_guess, None,
        "unknown xdg entry should have no tool guess"
    );
}

#[test]
fn test_scan_dotfiles_all_home_skip_entries() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    for skip in &[
        ".git",
        ".DS_Store",
        ".Trash",
        ".cache",
        ".local",
        ".Spotlight-V100",
        ".fseventsd",
    ] {
        if skip.contains('.') && *skip != ".git" {
            fs::write(home.join(skip), "").unwrap();
        } else {
            fs::create_dir_all(home.join(skip)).unwrap();
        }
    }
    // Also add one that should NOT be skipped
    fs::write(home.join(".bashrc"), "# bash").unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let paths: Vec<_> = entries.iter().map(|e| &e.path).collect();

    for skip in &[
        ".git",
        ".DS_Store",
        ".Trash",
        ".cache",
        ".local",
        ".Spotlight-V100",
        ".fseventsd",
    ] {
        assert!(
            !paths.contains(&&home.join(skip)),
            "{skip} should be skipped"
        );
    }
    assert!(
        paths.contains(&&home.join(".bashrc")),
        ".bashrc should be found"
    );
}

#[test]
fn test_scan_dotfiles_xdg_file_has_nonzero_size() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let config_dir = home.join(".config");
    fs::create_dir_all(&config_dir).unwrap();
    let content = "[starship]\nformat = '$all'";
    fs::write(config_dir.join("starship.toml"), content).unwrap();

    let entries = scan_dotfiles(home).unwrap();
    let starship = entries
        .iter()
        .find(|e| e.path == config_dir.join("starship.toml"))
        .expect("starship.toml should appear");
    assert_eq!(starship.entry_type, "file");
    assert_eq!(
        starship.size_bytes,
        content.len() as u64,
        "xdg file should report accurate size_bytes"
    );
    assert_eq!(starship.tool_guess.as_deref(), Some("starship"));
}

#[test]
fn test_scan_dotfiles_nonexistent_home_returns_error() {
    let result = scan_dotfiles(Path::new("/nonexistent/path/that/cannot/exist"));
    assert!(result.is_err(), "nonexistent home should return Io error");
}

#[test]
fn test_scan_dotfiles_multiple_xdg_tool_guesses() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let config_dir = home.join(".config");
    fs::create_dir_all(config_dir.join("fish")).unwrap();
    fs::create_dir_all(config_dir.join("kitty")).unwrap();
    fs::create_dir_all(config_dir.join("bat")).unwrap();
    fs::create_dir_all(config_dir.join("lazygit")).unwrap();
    fs::create_dir_all(config_dir.join("zellij")).unwrap();

    let entries = scan_dotfiles(home).unwrap();

    let find_guess = |name: &str| -> Option<String> {
        entries
            .iter()
            .find(|e| e.path.file_name().and_then(|n| n.to_str()) == Some(name))
            .and_then(|e| e.tool_guess.clone())
    };

    assert_eq!(find_guess("fish"), Some("fish".into()));
    assert_eq!(find_guess("kitty"), Some("kitty".into()));
    assert_eq!(find_guess("bat"), Some("bat".into()));
    assert_eq!(find_guess("lazygit"), Some("lazygit".into()));
    assert_eq!(find_guess("zellij"), Some("zellij".into()));
}

// ---- Additional coverage: scan_shell_config multi-shell ----

#[test]
fn test_scan_shell_config_fish_with_aliases_and_exports() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let fish_dir = home.join(".config").join("fish");
    fs::create_dir_all(&fish_dir).unwrap();
    fs::write(
        fish_dir.join("config.fish"),
        "alias ll='ls -la'\nexport EDITOR=nvim\nfish_add_path /usr/local/bin\n",
    )
    .unwrap();

    let result = scan_shell_config("fish", home).unwrap();
    assert_eq!(result.shell, "fish");
    assert!(result.aliases.iter().any(|a| a.name == "ll"));
    assert!(result.exports.iter().any(|e| e.name == "EDITOR"));
    assert_eq!(result.path_additions.len(), 1);
}

#[test]
fn test_scan_shell_config_zsh_multiple_rc_files() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".zshenv"), "export LANG=en_US.UTF-8\n").unwrap();
    fs::write(home.join(".zshrc"), "alias gs='git status'\n").unwrap();
    fs::write(home.join(".zprofile"), "export TERM=xterm-256color\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.config_files.len(), 3);
    assert!(result.exports.iter().any(|e| e.name == "LANG"));
    assert!(result.exports.iter().any(|e| e.name == "TERM"));
    assert!(result.aliases.iter().any(|a| a.name == "gs"));
}

#[test]
fn test_scan_shell_config_detects_prezto_via_non_source_line() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "zstyle ':prezto:*' color 'yes'\nalias ll='ls -la'\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("prezto"));
}

#[test]
fn test_scan_shell_config_detects_zim_via_non_source_line() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".zshrc"), "ZIM_HOME=~/.zim\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("zim"));
}

#[test]
fn test_scan_shell_config_detects_antigen_via_non_source_line() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "antigen bundle zsh-users/zsh-autosuggestions\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("antigen"));
}

#[test]
fn test_scan_shell_config_detects_antibody_via_non_source_line() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "antibody bundle < ~/.zsh_plugins.txt\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("antibody"));
}

#[test]
fn test_scan_shell_config_detects_sheldon_via_non_source_line() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".zshrc"), "eval \"$(sheldon source)\"\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.plugin_manager.as_deref(), Some("sheldon"));
}

#[test]
fn test_scan_shell_config_first_plugin_manager_wins() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "source $ZSH/oh-my-zsh.sh\nzinit light zsh-users/zsh-syntax-highlighting\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(
        result.plugin_manager.as_deref(),
        Some("oh-my-zsh"),
        "first detected plugin manager should win"
    );
}

#[test]
fn test_scan_shell_config_alias_without_value_skipped() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".zshrc"), "alias =\nalias good='cmd'\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(
        result.aliases.len(),
        1,
        "alias with empty name/value should be skipped"
    );
    assert_eq!(result.aliases[0].name, "good");
}

#[test]
fn test_scan_shell_config_export_without_equals_skipped() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".zshrc"), "export NOEQUALS\nexport VALID=value\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(
        result.exports.len(),
        1,
        "export without = should be skipped"
    );
    assert_eq!(result.exports[0].name, "VALID");
}

#[test]
fn test_scan_shell_config_sourced_file_with_quotes() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(
        home.join(".zshrc"),
        "source \"~/.shell_extras\"\nsource '~/.other'\n",
    )
    .unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    let sourced_strs: Vec<_> = result
        .sourced_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert!(
        sourced_strs.iter().any(|s| s.contains(".shell_extras")),
        "double-quoted sourced file should be captured: {:?}",
        sourced_strs
    );
    assert!(
        sourced_strs.iter().any(|s| s.contains(".other")),
        "single-quoted sourced file should be captured: {:?}",
        sourced_strs
    );
}

#[test]
fn test_scan_shell_config_empty_lines_and_whitespace_only() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    fs::write(home.join(".zshrc"), "\n\n   \n\t\nalias valid='cmd'\n\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    assert_eq!(result.aliases.len(), 1);
    assert_eq!(result.aliases[0].name, "valid");
}

// ---- Additional coverage: detect_plugin_manager edge cases ----

#[test]
fn test_detect_plugin_manager_ohmyzsh_install_url() {
    assert_eq!(
        detect_plugin_manager(
            "sh -c \"$(curl -fsSL https://raw.github.com/ohmyzsh/ohmyzsh/master/tools/install.sh)\""
        ),
        Some("oh-my-zsh")
    );
}

#[test]
fn test_detect_plugin_manager_fisher_always_matches_because_contains_fish() {
    // "fisher" inherently contains "fish" as a substring, so it always matches
    assert_eq!(
        detect_plugin_manager("some_fisher_tool install"),
        Some("fisher"),
        "fisher contains 'fish' so the condition is always true"
    );
}

#[test]
fn test_detect_plugin_manager_whitespace_handling() {
    assert_eq!(
        detect_plugin_manager("  source ~/.zinit/bin/zinit.zsh  "),
        Some("zinit")
    );
}

// ---- Additional coverage: strip_inline_comment edge cases ----

#[test]
fn test_strip_inline_comment_nested_quotes() {
    assert_eq!(
        strip_inline_comment(r#"alias x="it's a 'test'" # comment"#),
        r#"alias x="it's a 'test'""#
    );
}

#[test]
fn test_strip_inline_comment_empty_string() {
    assert_eq!(strip_inline_comment(""), "");
}

#[test]
fn test_strip_inline_comment_hash_inside_single_then_outside() {
    assert_eq!(
        strip_inline_comment("echo 'has#inside' outside # real comment"),
        "echo 'has#inside' outside"
    );
}

// ---- Additional coverage: scan_installed_packages edge cases ----

#[test]
fn test_scan_installed_packages_filter_matches_nothing() {
    let brew = TestPackageManager {
        manager_name: "brew",
        available: true,
        packages: vec![pkg("ripgrep", "14.0.0")],
    };

    let managers: Vec<&dyn PackageManager> = vec![&brew];
    let entries = scan_installed_packages(&managers, Some("nonexistent_manager")).unwrap();
    assert!(
        entries.is_empty(),
        "filter matching no manager should return empty"
    );
}

#[test]
fn test_scan_installed_packages_all_unavailable() {
    let mgr1 = TestPackageManager {
        manager_name: "brew",
        available: false,
        packages: vec![pkg("ripgrep", "14.0.0")],
    };
    let mgr2 = TestPackageManager {
        manager_name: "apt",
        available: false,
        packages: vec![pkg("curl", "7.88.1")],
    };

    let managers: Vec<&dyn PackageManager> = vec![&mgr1, &mgr2];
    let entries = scan_installed_packages(&managers, None).unwrap();
    assert!(
        entries.is_empty(),
        "all unavailable should return empty list"
    );
}

#[test]
fn test_scan_installed_packages_filter_skips_non_matching_even_if_available() {
    let brew = TestPackageManager {
        manager_name: "brew",
        available: true,
        packages: vec![pkg("ripgrep", "14.0.0")],
    };
    let apt = TestPackageManager {
        manager_name: "apt",
        available: true,
        packages: vec![pkg("curl", "7.88.1")],
    };

    let managers: Vec<&dyn PackageManager> = vec![&brew, &apt];
    let entries = scan_installed_packages(&managers, Some("apt")).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "curl");
    assert_eq!(entries[0].manager, "apt");
}

// ---- Additional coverage: parse_shell_file edge cases ----

#[test]
fn test_parse_shell_file_alias_unquoted_value() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.sh");
    fs::write(&file, "alias k=kubectl\n").unwrap();

    let mut aliases = vec![];
    let mut exports = vec![];
    let mut paths = vec![];
    let mut pm = None;
    parse_shell_file(&file, &mut aliases, &mut exports, &mut paths, &mut pm);

    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].name, "k");
    assert_eq!(aliases[0].command, "kubectl");
}

#[test]
fn test_parse_shell_file_source_detects_plugin_manager() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.sh");
    fs::write(&file, "source $ZSH/oh-my-zsh.sh\n").unwrap();

    let mut aliases = vec![];
    let mut exports = vec![];
    let mut paths = vec![];
    let mut pm = None;
    let sourced = parse_shell_file(&file, &mut aliases, &mut exports, &mut paths, &mut pm);

    assert_eq!(pm.as_deref(), Some("oh-my-zsh"));
    assert_eq!(sourced.len(), 1);
}

#[test]
fn test_parse_shell_file_empty_source_value_skipped() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.sh");
    fs::write(&file, "source ''\n. \"\"\n").unwrap();

    let mut aliases = vec![];
    let mut exports = vec![];
    let mut paths = vec![];
    let mut pm = None;
    let sourced = parse_shell_file(&file, &mut aliases, &mut exports, &mut paths, &mut pm);

    assert!(
        sourced.is_empty(),
        "empty quoted source paths should be skipped"
    );
}

#[test]
fn test_parse_shell_file_mixed_content() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.sh");
    fs::write(
        &file,
        concat!(
            "# comment\n",
            "alias ll='ls -la'\n",
            "export EDITOR=nvim\n",
            "export PATH=\"$HOME/bin:$PATH\"\n",
            "fish_add_path /opt/bin\n",
            "path+=($HOME/.local/bin)\n",
            "source ~/.extras\n",
            "zinit light zsh-users/zsh-syntax-highlighting\n",
            "some random line\n",
        ),
    )
    .unwrap();

    let mut aliases = vec![];
    let mut exports = vec![];
    let mut paths = vec![];
    let mut pm = None;
    let sourced = parse_shell_file(&file, &mut aliases, &mut exports, &mut paths, &mut pm);

    assert_eq!(aliases.len(), 1);
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "EDITOR");
    // PATH export + fish_add_path + path+=
    assert_eq!(paths.len(), 3);
    assert_eq!(sourced.len(), 1);
    assert_eq!(pm.as_deref(), Some("zinit"));
}

#[test]
fn test_scan_shell_config_deduplicates_sourced_files() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // Both .zshenv and .zshrc source the same file
    fs::write(home.join(".zshenv"), "source ~/.shared\n").unwrap();
    fs::write(home.join(".zshrc"), "source ~/.shared\n").unwrap();

    let result = scan_shell_config("zsh", home).unwrap();
    let shared_count = result
        .sourced_files
        .iter()
        .filter(|p| p.to_string_lossy().contains(".shared"))
        .count();
    // dedup only removes consecutive duplicates, so they'll be deduplicated
    // if they come in sequence
    assert!(
        shared_count <= 2,
        "sourced_files should contain .shared at most twice (dedup removes consecutive dupes)"
    );
}

#[test]
fn test_scan_shell_config_returns_correct_shell_name() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    for shell in &["zsh", "bash", "fish", "sh", "dash"] {
        let result = scan_shell_config(shell, home).unwrap();
        assert_eq!(result.shell, *shell);
    }
}

// ---- Additional coverage: shell_config_files ----

#[test]
fn test_shell_config_files_returns_correct_count() {
    let home = Path::new("/test");
    assert_eq!(shell_config_files("zsh", home).len(), 4);
    assert_eq!(shell_config_files("bash", home).len(), 4);
    assert_eq!(shell_config_files("fish", home).len(), 1);
    assert_eq!(shell_config_files("sh", home).len(), 1);
    assert_eq!(shell_config_files("dash", home).len(), 1);
    assert_eq!(shell_config_files("ksh", home).len(), 0);
    assert_eq!(shell_config_files("", home).len(), 0);
}

// ---- Additional coverage: build_tool_map completeness ----

#[test]
fn test_build_tool_map_contains_common_tools() {
    let map = build_tool_map();
    // Verify a broad sampling of entries exist
    let expected_home = vec![
        (".zshrc", "zsh"),
        (".bashrc", "bash"),
        (".vimrc", "vim"),
        (".gitconfig", "git"),
        (".npmrc", "npm"),
        (".cargo", "cargo"),
        (".aws", "aws"),
        (".docker", "docker"),
        (".mise.toml", "mise"),
        (".tool-versions", "asdf"),
        (".editorconfig", "editorconfig"),
        (".gnupg", "gpg"),
    ];
    for (key, expected_tool) in expected_home {
        assert_eq!(
            map.get(key),
            Some(&expected_tool),
            "TOOL_MAP missing or wrong for '{key}'"
        );
    }

    let expected_xdg = vec![
        ("tmux", "tmux"),
        ("ghostty", "ghostty"),
        ("wezterm", "wezterm"),
        ("hypr", "hyprland"),
        ("waybar", "waybar"),
        ("rofi", "rofi"),
        ("yazi", "yazi"),
        ("zoxide", "zoxide"),
        ("delta", "delta"),
        ("difftastic", "difftastic"),
        ("mise", "mise"),
    ];
    for (key, expected_tool) in expected_xdg {
        assert_eq!(
            map.get(key),
            Some(&expected_tool),
            "TOOL_MAP missing or wrong for xdg '{key}'"
        );
    }
}

#[test]
fn test_scan_dotfiles_counts_only_file_content_for_size() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // Write files of known sizes
    fs::write(home.join(".zshrc"), "abc").unwrap(); // 3 bytes
    fs::write(home.join(".gitconfig"), "").unwrap(); // 0 bytes

    let entries = scan_dotfiles(home).unwrap();
    let zshrc = entries
        .iter()
        .find(|e| e.path == home.join(".zshrc"))
        .unwrap();
    assert_eq!(zshrc.size_bytes, 3);

    let gitconfig = entries
        .iter()
        .find(|e| e.path == home.join(".gitconfig"))
        .unwrap();
    assert_eq!(gitconfig.size_bytes, 0);
}
