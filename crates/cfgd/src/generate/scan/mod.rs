use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

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
static TOOL_MAP: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(build_tool_map);

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
    let tool_map = &*TOOL_MAP;
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
                tracing::warn!("failed to list packages from {}: {}", manager.name(), e);
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
            tracing::warn!("failed to serialize macOS domains list: {e}");
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
    {
        let agents_dir = cfgd_core::expand_tilde(std::path::Path::new("~/Library/LaunchAgents"));
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
                    if let Some((name, _reg_type, value)) = crate::system::parse_reg_line(line) {
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
mod tests;
