use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use cfgd_core::errors::CfgdError;
use cfgd_core::providers::PackageManager;

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
        "fish" => vec![home.join(".config").join("fish").join("config.fish")],
        "sh" | "dash" => vec![home.join(".profile")],
        _ => vec![],
    }
}

/// Detect plugin manager from a line.
fn detect_plugin_manager(line: &str) -> Option<&'static str> {
    let l = line.trim();
    if l.contains("oh-my-zsh") || l.contains("$ZSH/oh-my-zsh.sh") || l.contains("ohmyzsh/ohmyzsh") {
        Some("oh-my-zsh")
    } else if l.contains("zinit") || l.contains("zdharma-continuum/zinit") || l.contains("zplugin")
    {
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
    } else if l.contains("prezto") {
        Some("prezto")
    } else if l.contains("zim") {
        Some("zim")
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
                    aliases.push(ScannedAlias {
                        name,
                        command: value,
                    });
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
            if plugin_manager.is_none()
                && let Some(pm) = detect_plugin_manager(line)
            {
                *plugin_manager = Some(pm.to_string());
            }
            let src = strip_quotes(src);
            if !src.is_empty() {
                sourced.push(PathBuf::from(src));
            }
            continue;
        }

        // --- plugin manager detection (non-source lines) ---
        if plugin_manager.is_none()
            && let Some(pm) = detect_plugin_manager(line)
        {
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
    let config_files: Vec<PathBuf> = candidate_files.into_iter().filter(|p| p.exists()).collect();

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
// scan_installed_packages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageEntry {
    pub name: String,
    pub version: String,
    pub manager: String,
}

/// Scan installed packages across all available package managers.
pub fn scan_installed_packages(
    managers: &[&dyn PackageManager],
    filter_manager: Option<&str>,
) -> Result<Vec<InstalledPackageEntry>, CfgdError> {
    let mut entries = vec![];
    for manager in managers {
        if let Some(filter) = filter_manager
            && manager.name() != filter
        {
            continue;
        }
        if !manager.is_available() {
            continue;
        }
        match manager.installed_packages_with_versions() {
            Ok(pkgs) => {
                for pkg in pkgs {
                    entries.push(InstalledPackageEntry {
                        name: pkg.name,
                        version: pkg.version,
                        manager: manager.name().to_string(),
                    });
                }
            }
            Err(e) => {
                // Log but don't fail — some managers may not be usable
                tracing::warn!("Failed to list packages from {}: {}", manager.name(), e);
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name).then(a.manager.cmp(&b.manager)));
    Ok(entries)
}

// ---------------------------------------------------------------------------
// scan_system_settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSettingsResult {
    pub macos_defaults: Option<serde_yaml::Value>,
    pub systemd_units: Vec<String>,
    pub launch_agents: Vec<String>,
    pub gsettings_schemas: Vec<String>,
    pub windows_services: Vec<String>,
    /// Registry values from well-known paths (analogous to `defaults domains` on macOS).
    /// Maps "HIVE\\Subkey\\ValueName" → current value string.
    pub windows_registry: std::collections::BTreeMap<String, String>,
}

/// Scan platform-specific system settings.
pub fn scan_system_settings() -> Result<SystemSettingsResult, CfgdError> {
    let mut result = SystemSettingsResult {
        macos_defaults: None,
        systemd_units: vec![],
        launch_agents: vec![],
        gsettings_schemas: vec![],
        windows_services: vec![],
        windows_registry: std::collections::BTreeMap::new(),
    };

    // macOS: run `defaults domains` and parse comma-separated list — don't export all, just list them
    if cfgd_core::command_available("defaults")
        && let Ok(output) = std::process::Command::new("defaults")
            .arg("domains")
            .output()
        && output.status.success()
    {
        let domains_str = String::from_utf8_lossy(&output.stdout);
        let domains: Vec<String> = domains_str
            .trim()
            .split(", ")
            .map(|s| s.to_string())
            .collect();
        result.macos_defaults = serde_yaml::to_value(domains).map(Some).unwrap_or_else(|e| {
            tracing::warn!("Failed to serialize macOS domains list: {e}");
            None
        });
    }

    // Linux: list user systemd units
    if cfgd_core::command_available("systemctl")
        && let Ok(output) = std::process::Command::new("systemctl")
            .args(["--user", "list-unit-files", "--no-pager", "--plain"])
            .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines().skip(1) {
            // Format: "unit-name.service  enabled"
            if let Some(unit) = line.split_whitespace().next()
                && (unit.ends_with(".service") || unit.ends_with(".timer"))
            {
                result.systemd_units.push(unit.to_string());
            }
        }
    }

    // macOS: list launch agents
    if let Ok(home) = std::env::var("HOME") {
        let agents_dir = std::path::PathBuf::from(&home).join("Library/LaunchAgents");
        if agents_dir.exists()
            && let Ok(dir_entries) = std::fs::read_dir(&agents_dir)
        {
            for entry in dir_entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && name.ends_with(".plist")
                {
                    result.launch_agents.push(name.to_string());
                }
            }
        }
    }

    // Linux: list gsettings schemas
    if cfgd_core::command_available("gsettings")
        && let Ok(output) = std::process::Command::new("gsettings")
            .arg("list-schemas")
            .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let schema = line.trim();
            if !schema.is_empty() {
                result.gsettings_schemas.push(schema.to_string());
            }
        }
    }

    // Windows: scan well-known registry paths (analogous to `defaults domains` on macOS)
    if cfgd_core::command_available("reg") {
        let well_known_paths = [
            // Desktop appearance & behavior
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            r"HKCU\Software\Microsoft\Windows\DWM",
            r"HKCU\Control Panel\Desktop",
            // Input devices
            r"HKCU\Control Panel\Mouse",
            r"HKCU\Control Panel\Keyboard",
            r"HKCU\Control Panel\Accessibility",
            // Policies (group-policy-style user preferences)
            r"HKCU\Software\Policies\Microsoft\Windows",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\Explorer",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System",
            // Environment
            r"HKCU\Environment",
        ];
        for reg_path in &well_known_paths {
            if let Ok(output) = std::process::Command::new("reg")
                .args(["query", reg_path])
                .output()
                && output.status.success()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Some((name, _reg_type, value)) =
                        crate::system::parse_reg_line(line)
                    {
                        result
                            .windows_registry
                            .insert(format!(r"{}\{}", reg_path, name), value.to_string());
                    }
                }
            }
        }
    }

    // Windows: list installed services via sc.exe
    if cfgd_core::command_available("sc.exe")
        && let Ok(output) = std::process::Command::new("sc.exe")
            .args(["query", "type=", "service", "state=", "all"])
            .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            // Lines like: "SERVICE_NAME: MyService"
            if let Some(rest) = line.trim().strip_prefix("SERVICE_NAME:") {
                let name = rest.trim();
                if !name.is_empty() {
                    result.windows_services.push(name.to_string());
                }
            }
        }
    }

    result.systemd_units.sort();
    result.launch_agents.sort();
    result.gsettings_schemas.sort();
    result.windows_services.sort();
    Ok(result)
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
        fn install(
            &self,
            _packages: &[String],
            _printer: &Printer,
        ) -> cfgd_core::errors::Result<()> {
            Ok(())
        }
        fn uninstall(
            &self,
            _packages: &[String],
            _printer: &Printer,
        ) -> cfgd_core::errors::Result<()> {
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
}
