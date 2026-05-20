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

use cfgd_core::output_v2::Printer;
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
