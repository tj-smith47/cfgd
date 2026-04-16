use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use cfgd_core::errors::CfgdError;
use cfgd_core::providers::PackageManager;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInspection {
    pub name: String,
    pub version: Option<String>,
    pub config_paths: Vec<PathBuf>,
    pub plugin_system: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageQueryResult {
    pub package: String,
    pub manager: String,
    pub available: bool,
    pub version: Option<String>,
    pub aliases: Vec<String>,
}

// ---------------------------------------------------------------------------
// inspect_tool
// ---------------------------------------------------------------------------

/// Try to get the version of `name` by running it with common version flags.
/// Returns None if the tool is not installed or does not respond.
fn probe_version(name: &str) -> Option<String> {
    for flag in &["--version", "-V", "-version"] {
        if let Ok(output) = Command::new(name).arg(flag).output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let first_line = stdout.lines().next().unwrap_or("").trim();
            if !first_line.is_empty() {
                return Some(first_line.to_string());
            }
            // Some tools (e.g. vim) write version to stderr even on success
            if !output.stderr.is_empty() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let first_line = stderr.lines().next().unwrap_or("").trim();
                if !first_line.is_empty() {
                    return Some(first_line.to_string());
                }
            }
        }
    }
    None
}

/// Gather config paths for `name` under `home`.
///
/// Checked candidates:
///   - `~/.config/<name>/`
///   - `~/.<name>rc`
///   - `~/.<name>/`
///   - `~/.<name>.conf`
///   - `~/.<name>.toml`
///   - `~/.<name>.yaml`
fn collect_config_paths(name: &str, home: &Path) -> Vec<PathBuf> {
    let candidates = [
        home.join(".config").join(name),
        home.join(format!(".{}rc", name)),
        home.join(format!(".{}", name)),
        home.join(format!(".{}.conf", name)),
        home.join(format!(".{}.toml", name)),
        home.join(format!(".{}.yaml", name)),
    ];

    candidates.into_iter().filter(|p| p.exists()).collect()
}

/// Detect a plugin system by scanning the content of config files.
///
/// Rules:
///   - "lazy"      in nvim config  → "lazy.nvim"
///   - "plug"      in vim config   → "vim-plug"
///   - "tpm"       in tmux config  → "tpm"
///   - "oh-my-zsh" in zsh config   → "oh-my-zsh"
fn detect_plugin_system(name: &str, config_paths: &[PathBuf]) -> Option<String> {
    for config_path in config_paths {
        let content = read_config_dir_content(config_path);

        match name {
            "nvim" | "neovim" if content.contains("lazy") => {
                return Some("lazy.nvim".to_string());
            }
            "vim" if content.contains("plug") => {
                return Some("vim-plug".to_string());
            }
            "tmux" if content.contains("tpm") => {
                return Some("tpm".to_string());
            }
            "zsh" if content.contains("oh-my-zsh") => {
                return Some("oh-my-zsh".to_string());
            }
            _ => {}
        }
    }
    None
}

/// Read the text content of a config path. For directories, reads all files
/// within (non-recursively) and concatenates them. For files, reads directly.
fn read_config_dir_content(path: &Path) -> String {
    if path.is_file() {
        return std::fs::read_to_string(path).unwrap_or_default();
    }

    if path.is_dir() {
        let mut combined = String::new();
        if let Ok(iter) = std::fs::read_dir(path) {
            for entry in iter.flatten() {
                let p = entry.path();
                if p.is_file()
                    && let Ok(text) = std::fs::read_to_string(&p)
                {
                    combined.push_str(&text);
                    combined.push('\n');
                }
            }
        }
        return combined;
    }

    String::new()
}

/// Inspect an installed tool: detect its version and locate its config files.
///
/// If the tool is not installed, `version` will be `None` and `config_paths`
/// will be empty. The function never returns an error for missing tools; errors
/// are only returned for unexpected I/O failures.
pub fn inspect_tool(name: &str, home: &Path) -> Result<ToolInspection, CfgdError> {
    let version = probe_version(name);
    let config_paths = collect_config_paths(name, home);
    let plugin_system = detect_plugin_system(name, &config_paths);

    Ok(ToolInspection {
        name: name.to_string(),
        version,
        config_paths,
        plugin_system,
    })
}

// ---------------------------------------------------------------------------
// query_package_manager
// ---------------------------------------------------------------------------

/// Query a package manager for information about a single package.
///
/// `available` is `true` when `available_version` returns `Some`.
pub fn query_package_manager(
    manager: &dyn PackageManager,
    package: &str,
) -> Result<PackageQueryResult, CfgdError> {
    let version = manager.available_version(package)?;
    let available = version.is_some();
    let aliases = manager.package_aliases(package)?;

    Ok(PackageQueryResult {
        package: package.to_string(),
        manager: manager.name().to_string(),
        available,
        version,
        aliases,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    use cfgd_core::errors::Result;
    use cfgd_core::output::Printer;
    use cfgd_core::providers::PackageManager;
    use tempfile::TempDir;

    // --- Minimal test double for PackageManager ---

    struct TestPackageManager {
        version: Option<String>,
    }

    impl PackageManager for TestPackageManager {
        fn name(&self) -> &str {
            "test"
        }

        fn is_available(&self) -> bool {
            true
        }

        fn can_bootstrap(&self) -> bool {
            false
        }

        fn bootstrap(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }

        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(HashSet::new())
        }

        fn install(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
        }

        fn uninstall(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
        }

        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }

        fn available_version(&self, _package: &str) -> Result<Option<String>> {
            Ok(self.version.clone())
        }
    }

    // --- inspect_tool tests ---

    #[test]
    fn test_inspect_tool_finds_config_paths() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".config/nvim")).unwrap();
        std::fs::write(home.join(".config/nvim/init.lua"), "-- config").unwrap();

        let result = inspect_tool("nvim", home).unwrap();
        assert_eq!(result.name, "nvim");
        assert!(
            result
                .config_paths
                .iter()
                .any(|p| p.ends_with(".config/nvim")),
            "expected .config/nvim in config_paths, got: {:?}",
            result.config_paths
        );
    }

    #[test]
    fn test_inspect_tool_detects_plugin_system() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".config/nvim")).unwrap();
        std::fs::write(home.join(".config/nvim/init.lua"), "require('lazy')").unwrap();

        let result = inspect_tool("nvim", home).unwrap();
        assert_eq!(result.plugin_system.as_deref(), Some("lazy.nvim"));
    }

    #[test]
    fn test_inspect_tool_unknown_tool() {
        let tmp = TempDir::new().unwrap();
        let result = inspect_tool("nonexistent_tool_xyz", tmp.path()).unwrap();
        assert_eq!(result.name, "nonexistent_tool_xyz");
        assert!(result.version.is_none());
        assert!(result.config_paths.is_empty());
    }

    #[test]
    fn test_inspect_tool_dotfile_rc() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(home.join(".zshrc"), "# zsh config").unwrap();

        let result = inspect_tool("zsh", home).unwrap();
        assert!(
            result.config_paths.iter().any(|p| p.ends_with(".zshrc")),
            "expected .zshrc in config_paths, got: {:?}",
            result.config_paths
        );
    }

    #[test]
    fn test_inspect_tool_dot_dir() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".mytool")).unwrap();
        std::fs::write(home.join(".mytool/config.yaml"), "key: value").unwrap();

        let result = inspect_tool("mytool", home).unwrap();
        assert!(
            result.config_paths.iter().any(|p| p.ends_with(".mytool")),
            "expected .mytool directory in config_paths, got: {:?}",
            result.config_paths
        );
    }

    #[test]
    fn test_inspect_tool_detects_vim_plug() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(home.join(".vimrc"), "call plug#begin('~/.vim/plugged')").unwrap();

        let result = inspect_tool("vim", home).unwrap();
        assert_eq!(result.plugin_system.as_deref(), Some("vim-plug"));
    }

    #[test]
    fn test_inspect_tool_detects_tpm() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(home.join(".tmux.conf"), "run '~/.tmux/plugins/tpm/tpm'").unwrap();

        let result = inspect_tool("tmux", home).unwrap();
        assert_eq!(result.plugin_system.as_deref(), Some("tpm"));
    }

    #[test]
    fn test_inspect_tool_detects_oh_my_zsh() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(home.join(".zshrc"), "source $ZSH/oh-my-zsh.sh").unwrap();

        let result = inspect_tool("zsh", home).unwrap();
        assert_eq!(result.plugin_system.as_deref(), Some("oh-my-zsh"));
    }

    #[test]
    fn test_inspect_tool_no_plugin_system() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(home.join(".zshrc"), "export PATH=\"$HOME/bin:$PATH\"").unwrap();

        let result = inspect_tool("zsh", home).unwrap();
        assert!(result.plugin_system.is_none());
    }

    // --- query_package_manager tests ---

    #[test]
    fn test_query_package_manager_available() {
        let mock = TestPackageManager {
            version: Some("1.2.3".into()),
        };
        let result = query_package_manager(&mock, "neovim").unwrap();
        assert!(result.available);
        assert_eq!(result.version.as_deref(), Some("1.2.3"));
        assert_eq!(result.manager, "test");
        assert_eq!(result.package, "neovim");
    }

    #[test]
    fn test_query_package_manager_unavailable() {
        let mock = TestPackageManager { version: None };
        let result = query_package_manager(&mock, "neovim").unwrap();
        assert!(!result.available);
        assert!(result.version.is_none());
        assert_eq!(result.manager, "test");
    }

    #[test]
    fn test_query_package_manager_aliases_empty_by_default() {
        let mock = TestPackageManager {
            version: Some("0.1.0".into()),
        };
        let result = query_package_manager(&mock, "fd").unwrap();
        assert!(result.aliases.is_empty());
    }
}
