use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use cfgd_core::errors::CfgdError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DotfileEntry {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub entry_type: String, // "file", "directory", "symlink"
    pub tool_guess: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfigResult {
    pub shell: String,
    pub config_files: Vec<PathBuf>,
    pub aliases: Vec<ScannedAlias>,
    pub exports: Vec<ScannedExport>,
    pub path_additions: Vec<String>,
    pub sourced_files: Vec<PathBuf>,
    pub plugin_manager: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedAlias {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedExport {
    pub name: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Tool-guess map
// ---------------------------------------------------------------------------

/// Returns a map from dotfile name/path suffix → tool name.
/// Keys are matched against the entry's filename (for home-level dotfiles) or
/// the relative path under `~/.config/` (for XDG entries).
fn build_tool_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();

    // Home-level dotfiles
    m.insert(".zshrc", "zsh");
    m.insert(".zshenv", "zsh");
    m.insert(".zprofile", "zsh");
    m.insert(".zlogin", "zsh");
    m.insert(".zlogout", "zsh");
    m.insert(".bashrc", "bash");
    m.insert(".bash_profile", "bash");
    m.insert(".bash_login", "bash");
    m.insert(".bash_logout", "bash");
    m.insert(".vimrc", "vim");
    m.insert(".vim", "vim");
    m.insert(".tmux.conf", "tmux");
    m.insert(".tmux", "tmux");
    m.insert(".gitconfig", "git");
    m.insert(".gitignore_global", "git");
    m.insert(".gitattributes", "git");
    m.insert(".ssh", "ssh");
    m.insert(".gnupg", "gpg");
    m.insert(".gnupg.conf", "gpg");
    m.insert(".profile", "sh");
    m.insert(".inputrc", "readline");
    m.insert(".curlrc", "curl");
    m.insert(".wgetrc", "wget");
    m.insert(".screenrc", "screen");
    m.insert(".editorconfig", "editorconfig");
    m.insert(".npmrc", "npm");
    m.insert(".yarnrc", "yarn");
    m.insert(".pypirc", "pip");
    m.insert(".pip", "pip");
    m.insert(".rbenv", "rbenv");
    m.insert(".rvm", "rvm");
    m.insert(".nvm", "nvm");
    m.insert(".rustup", "rustup");
    m.insert(".cargo", "cargo");
    m.insert(".gradle", "gradle");
    m.insert(".m2", "maven");
    m.insert(".aws", "aws");
    m.insert(".gcloud", "gcloud");
    m.insert(".kube", "kubectl");
    m.insert(".helm", "helm");
    m.insert(".terraform.d", "terraform");
    m.insert(".ansible", "ansible");
    m.insert(".docker", "docker");
    m.insert(".asdf", "asdf");
    m.insert(".direnv", "direnv");
    m.insert(".mise.toml", "mise");
    m.insert(".tool-versions", "asdf");
    m.insert(".huskyrc", "husky");
    m.insert(".eslintrc", "eslint");
    m.insert(".prettierrc", "prettier");
    m.insert(".stylelintrc", "stylelint");
    m.insert(".babelrc", "babel");

    // XDG .config/ entries (matched against the immediate child directory/file name)
    m.insert("nvim", "nvim");
    m.insert("vim", "vim");
    m.insert("tmux", "tmux");
    m.insert("git", "git");
    m.insert("fish", "fish");
    m.insert("alacritty", "alacritty");
    m.insert("wezterm", "wezterm");
    m.insert("kitty", "kitty");
    m.insert("ghostty", "ghostty");
    m.insert("starship.toml", "starship");
    m.insert("starship", "starship");
    m.insert("htop", "htop");
    m.insert("btop", "btop");
    m.insert("lazygit", "lazygit");
    m.insert("bat", "bat");
    m.insert("lsd", "lsd");
    m.insert("eza", "eza");
    m.insert("ripgrep", "ripgrep");
    m.insert("zellij", "zellij");
    m.insert("helix", "helix");
    m.insert("karabiner-elements", "karabiner-elements");
    m.insert("skhd", "skhd");
    m.insert("yabai", "yabai");
    m.insert("i3", "i3");
    m.insert("sway", "sway");
    m.insert("hypr", "hyprland");
    m.insert("waybar", "waybar");
    m.insert("rofi", "rofi");
    m.insert("wofi", "wofi");
    m.insert("dunst", "dunst");
    m.insert("polybar", "polybar");
    m.insert("picom", "picom");
    m.insert("flameshot", "flameshot");
    m.insert("mpv", "mpv");
    m.insert("ranger", "ranger");
    m.insert("lf", "lf");
    m.insert("nnn", "nnn");
    m.insert("yazi", "yazi");
    m.insert("zoxide", "zoxide");
    m.insert("fzf", "fzf");
    m.insert("direnv", "direnv");
    m.insert("mise", "mise");
    m.insert("spotify-tui", "spotify-tui");
    m.insert("bottom", "bottom");
    m.insert("neofetch", "neofetch");
    m.insert("fastfetch", "fastfetch");
    m.insert("gh", "gh");
    m.insert("hub", "hub");
    m.insert("delta", "delta");
    m.insert("difftastic", "difftastic");

    m
}

// Dotfiles to skip entirely at the home level
const HOME_SKIP: &[&str] = &[
    ".git",
    ".DS_Store",
    ".Trash",
    ".cache",
    ".local",
    ".Spotlight-V100",
    ".fseventsd",
    ".VolumeIcon.icns",
];

// ---------------------------------------------------------------------------
// scan_dotfiles
// ---------------------------------------------------------------------------

/// Scan `home` for dotfiles and XDG config entries, annotating each with a
/// best-guess tool name.
pub fn scan_dotfiles(home: &Path) -> Result<Vec<DotfileEntry>, CfgdError> {
    let tool_map = build_tool_map();
    let mut entries: Vec<DotfileEntry> = Vec::new();

    // 1. Home-level dotfiles (names starting with `.`)
    let home_iter = match std::fs::read_dir(home) {
        Ok(it) => it,
        Err(e) => return Err(CfgdError::Io(e)),
    };

    for result in home_iter {
        let entry = result.map_err(CfgdError::Io)?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.starts_with('.') {
            continue;
        }

        if HOME_SKIP.contains(&name_str.as_ref()) {
            continue;
        }

        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue, // skip unreadable entries
        };

        let entry_type = if meta.file_type().is_symlink() {
            "symlink"
        } else if meta.is_dir() {
            "directory"
        } else {
            "file"
        };

        let size_bytes = if meta.is_file() { meta.len() } else { 0 };
        let tool_guess = tool_map.get(name_str.as_ref()).map(|s| s.to_string());

        entries.push(DotfileEntry {
            path,
            size_bytes,
            entry_type: entry_type.to_string(),
            tool_guess,
        });
    }

    // 2. XDG .config/ children — each direct child is likely a tool config
    let config_dir = home.join(".config");
    if config_dir.is_dir() {
        let config_iter = match std::fs::read_dir(&config_dir) {
            Ok(it) => it,
            Err(_) => return Ok(entries), // .config not readable; return what we have
        };

        for result in config_iter {
            let entry = result.map_err(CfgdError::Io)?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            let path = entry.path();
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let entry_type = if meta.file_type().is_symlink() {
                "symlink"
            } else if meta.is_dir() {
                "directory"
            } else {
                "file"
            };

            let size_bytes = if meta.is_file() { meta.len() } else { 0 };
            let tool_guess = tool_map.get(name_str.as_ref()).map(|s| s.to_string());

            entries.push(DotfileEntry {
                path,
                size_bytes,
                entry_type: entry_type.to_string(),
                tool_guess,
            });
        }
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// scan_shell_config
// ---------------------------------------------------------------------------

/// Determine which rc files to scan for the given shell.
fn shell_config_files(shell: &str, home: &Path) -> Vec<PathBuf> {
    match shell {
        "zsh" => vec![
            home.join(".zshenv"),
            home.join(".zprofile"),
            home.join(".zshrc"),
            home.join(".zlogin"),
        ],
        "bash" => vec![
            home.join(".bash_profile"),
            home.join(".bashrc"),
            home.join(".bash_login"),
            home.join(".profile"),
        ],
        "fish" => vec![
            home.join(".config").join("fish").join("config.fish"),
        ],
        "sh" | "dash" => vec![home.join(".profile")],
        _ => vec![],
    }
}

/// Detect plugin manager from a line.
fn detect_plugin_manager(line: &str) -> Option<&'static str> {
    let l = line.trim();
    if l.contains("oh-my-zsh") || l.contains("$ZSH/oh-my-zsh.sh") || l.contains("ohmyzsh/ohmyzsh") {
        Some("oh-my-zsh")
    } else if l.contains("zinit") || l.contains("zdharma-continuum/zinit") || l.contains("zplugin") {
        Some("zinit")
    } else if l.contains("zplug") {
        Some("zplug")
    } else if l.contains("antibody") {
        Some("antibody")
    } else if l.contains("antigen") {
        Some("antigen")
    } else if l.contains("sheldon") {
        Some("sheldon")
    } else if l.contains("fisher") && l.contains("fish") {
        Some("fisher")
    } else if l.contains("prezto") || l.contains("zim") {
        Some("prezto")
    } else {
        None
    }
}

/// Strip surrounding quotes from a value (single or double).
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse a single shell config file and accumulate results into the provided
/// accumulators. Returns the list of sourced files found in this file.
fn parse_shell_file(
    path: &Path,
    aliases: &mut Vec<ScannedAlias>,
    exports: &mut Vec<ScannedExport>,
    path_additions: &mut Vec<String>,
    plugin_manager: &mut Option<String>,
) -> Vec<PathBuf> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut sourced = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Remove inline comments (naive: first unquoted #)
        let line = strip_inline_comment(line);

        // --- alias name='command' or alias name="command" or alias name=command ---
        if let Some(rest) = line.strip_prefix("alias ") {
            if let Some(eq_pos) = rest.find('=') {
                let name = rest[..eq_pos].trim().to_string();
                let value = strip_quotes(rest[eq_pos + 1..].trim()).to_string();
                if !name.is_empty() && !value.is_empty() {
                    aliases.push(ScannedAlias { name, command: value });
                }
            }
            continue;
        }

        // --- export NAME=value or export NAME="value" ---
        if let Some(rest) = line.strip_prefix("export ") {
            if let Some(eq_pos) = rest.find('=') {
                let name = rest[..eq_pos].trim().to_string();
                let raw_val = rest[eq_pos + 1..].trim();
                let value = strip_quotes(raw_val).to_string();

                // PATH additions
                if name == "PATH" {
                    path_additions.push(raw_val.to_string());
                } else {
                    exports.push(ScannedExport { name, value });
                }
            }
            continue;
        }

        // --- fish path additions: fish_add_path or set -gx PATH ... ---
        if let Some(rest) = line.strip_prefix("fish_add_path ") {
            let addition = rest.trim().to_string();
            if !addition.is_empty() {
                path_additions.push(addition);
            }
            continue;
        }

        // --- zsh path+=(...) syntax ---
        if line.starts_with("path+=(") || line.starts_with("path=") {
            path_additions.push(line.to_string());
            continue;
        }

        // --- source / . sourced files ---
        let sourced_path = line
            .strip_prefix("source ")
            .or_else(|| line.strip_prefix(". "))
            .map(|rest| rest.trim());

        if let Some(src) = sourced_path {
            // Check for plugin manager before consuming the line
            if plugin_manager.is_none() && let Some(pm) = detect_plugin_manager(line) {
                *plugin_manager = Some(pm.to_string());
            }
            let src = strip_quotes(src);
            if !src.is_empty() {
                sourced.push(PathBuf::from(src));
            }
            continue;
        }

        // --- plugin manager detection (non-source lines) ---
        if plugin_manager.is_none() && let Some(pm) = detect_plugin_manager(line) {
            *plugin_manager = Some(pm.to_string());
        }
    }

    sourced
}

/// Naive inline comment stripper — stops at the first unquoted `#`.
fn strip_inline_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => {
                return line[..i].trim_end();
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// Scan all rc files for the given shell and return a consolidated result.
pub fn scan_shell_config(shell: &str, home: &Path) -> Result<ShellConfigResult, CfgdError> {
    let candidate_files = shell_config_files(shell, home);
    let config_files: Vec<PathBuf> = candidate_files
        .into_iter()
        .filter(|p| p.exists())
        .collect();

    let mut aliases: Vec<ScannedAlias> = Vec::new();
    let mut exports: Vec<ScannedExport> = Vec::new();
    let mut path_additions: Vec<String> = Vec::new();
    let mut sourced_files: Vec<PathBuf> = Vec::new();
    let mut plugin_manager: Option<String> = None;

    for file in &config_files {
        let sourced = parse_shell_file(
            file,
            &mut aliases,
            &mut exports,
            &mut path_additions,
            &mut plugin_manager,
        );
        sourced_files.extend(sourced);
    }

    // Deduplicate sourced files
    sourced_files.dedup();

    Ok(ShellConfigResult {
        shell: shell.to_string(),
        config_files,
        aliases,
        exports,
        path_additions,
        sourced_files,
        plugin_manager,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
            entries.iter().find(|e| {
                e.path.file_name().and_then(|n| n.to_str()) == Some(name)
            })
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

        assert!(!paths.contains(&home.join(".git")), ".git should be skipped");
        assert!(!paths.contains(&home.join(".cache")), ".cache should be skipped");
        assert!(!paths.contains(&home.join(".local")), ".local should be skipped");
        assert!(paths.contains(&home.join(".zshrc")), ".zshrc should be found");
    }

    #[test]
    fn test_scan_dotfiles_entry_types() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        fs::write(home.join(".vimrc"), "").unwrap();
        fs::create_dir_all(home.join(".ssh")).unwrap();

        let entries = scan_dotfiles(home).unwrap();

        let file_entry = entries.iter().find(|e| e.path == home.join(".vimrc")).unwrap();
        assert_eq!(file_entry.entry_type, "file");

        let dir_entry = entries.iter().find(|e| e.path == home.join(".ssh")).unwrap();
        assert_eq!(dir_entry.entry_type, "directory");
    }

    // ---- scan_shell_config tests ----

    #[test]
    fn test_scan_shell_config_parses_single_quote_aliases() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        fs::write(home.join(".zshrc"), "alias ll='ls -la'\nalias gs='git status'\n").unwrap();

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
}
