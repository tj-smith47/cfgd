//! Go install package manager (`go install <module>@version`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{
    any_system_manager_available, bootstrap_via_system_manager, brew_available, brew_cmd,
    resolve_tool_with_fallbacks, run_pkg_cmd_live, tool_cmd_with_resolver,
};

pub struct GoInstallManager;

fn go_fallbacks() -> Vec<PathBuf> {
    let mut fallbacks = vec![
        PathBuf::from("/usr/local/go/bin/go"),
        PathBuf::from("/usr/local/bin/go"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        fallbacks.push(PathBuf::from(home).join("go/bin/go"));
    }
    fallbacks
}

pub(super) fn find_go() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("go", &go_fallbacks())
}

pub(super) fn go_available() -> bool {
    find_go().is_some()
}

pub(super) fn go_cmd() -> Command {
    tool_cmd_with_resolver("go", find_go)
}

impl PackageManager for GoInstallManager {
    fn name(&self) -> &str {
        "go"
    }

    fn is_available(&self) -> bool {
        go_available()
    }

    fn can_bootstrap(&self) -> bool {
        any_system_manager_available()
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        if brew_available() {
            let result = printer
                .run_with_output(brew_cmd().args(["install", "go"]), "Installing Go via brew")
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "go".into(),
                    message: format!("brew install go failed: {}", e),
                })?;
            if result.status.success() {
                return Ok(());
            }
        }

        bootstrap_via_system_manager(printer, "golang", "go")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        // Scan $GOPATH/bin (or ~/go/bin) for installed binaries
        let gopath = std::env::var("GOPATH").ok().unwrap_or_else(|| {
            cfgd_core::expand_tilde(std::path::Path::new("~/go"))
                .to_string_lossy()
                .to_string()
        });

        let bin_dir = std::path::PathBuf::from(&gopath).join("bin");
        let mut packages = HashSet::new();

        if let Ok(entries) = std::fs::read_dir(&bin_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    packages.insert(name.to_string());
                }
            }
        }

        Ok(packages)
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            // `go install` requires a full module path with @version
            let install_path = if pkg.contains('@') {
                pkg.clone()
            } else {
                format!("{}@latest", pkg)
            };
            let label = format!("go install {}", install_path);
            run_pkg_cmd_live(
                printer,
                "go",
                go_cmd().args(["install", &install_path]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        // Go has no uninstall command; remove binaries from $GOPATH/bin
        let gopath = std::env::var("GOPATH").ok().unwrap_or_else(|| {
            cfgd_core::expand_tilde(std::path::Path::new("~/go"))
                .to_string_lossy()
                .to_string()
        });

        let bin_dir = std::path::PathBuf::from(&gopath).join("bin");
        for pkg in packages {
            // The binary name is the last path component of the module path.
            // Validate it contains no path separators to prevent traversal.
            let raw_name = pkg.rsplit('/').next().unwrap_or(pkg);
            let bin_name = std::path::Path::new(raw_name)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| PackageError::UninstallFailed {
                    manager: "go".into(),
                    message: format!("invalid binary name derived from package: {}", pkg),
                })?;
            let bin_path = bin_dir.join(bin_name);
            if bin_path.exists() {
                printer.info(&format!("removing {}", bin_path.display()));
                std::fs::remove_file(&bin_path).map_err(|e| PackageError::UninstallFailed {
                    manager: "go".into(),
                    message: format!("failed to remove {}: {}", bin_path.display(), e),
                })?;
            }
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // go install pkg@latest re-installs to update; no separate update command
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // go list -m -json <pkg>@latest → parse "Version" field
        let output = go_cmd()
            .args(["list", "-m", "-json", &format!("{}@latest", package)])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "go".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_go_module_version(&stdout))
    }
}

/// Parse version from `go list -m -json pkg@latest` output.
/// JSON format: `{"Version": "v1.2.3", ...}`
/// Strips the "v" prefix for consistency.
pub(super) fn parse_go_module_version(output: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(output).ok()?;
    let version = parsed.get("Version").and_then(|v| v.as_str())?;
    let version = version.strip_prefix('v').unwrap_or(version);
    Some(version.to_string())
}

#[cfg(test)]
mod tests {
    use cfgd_core::output::Printer;
    use cfgd_core::providers::PackageManager;

    use super::super::shared::any_system_manager_available;
    use super::*;

    #[test]
    fn go_install_manager_name_and_traits() {
        let mgr = GoInstallManager;
        assert_eq!(mgr.name(), "go");
    }

    #[test]
    fn go_install_manager_update_is_noop() {
        let mgr = GoInstallManager;
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.update(&printer).unwrap();
    }

    #[test]
    fn parse_go_module_version_strips_v_prefix() {
        let output = r#"{"Path":"golang.org/x/tools/gopls","Version":"v0.15.3"}"#;
        assert_eq!(parse_go_module_version(output), Some("0.15.3".to_string()));
    }

    #[test]
    fn parse_go_module_version_handles_pseudo_version() {
        // Go pseudo-versions include timestamps and commit hashes
        let output =
            r#"{"Path":"example.com/tool","Version":"v0.0.0-20240301120000-abcdef123456"}"#;
        assert_eq!(
            parse_go_module_version(output),
            Some("0.0.0-20240301120000-abcdef123456".to_string()),
            "should handle pseudo-versions with commit metadata"
        );
    }

    #[test]
    fn parse_go_module_version_extra_fields_ignored() {
        // Real go list -m output has many extra fields — only Version matters
        let output = r#"{"Path":"golang.org/x/tools","Version":"v0.20.0","Time":"2024-04-01T00:00:00Z","GoMod":"golang.org/x/tools@v0.20.0/go.mod"}"#;
        assert_eq!(parse_go_module_version(output), Some("0.20.0".to_string()));
    }

    #[test]
    fn parse_go_module_version_no_v_prefix() {
        // Unlikely but handles gracefully
        let output = r#"{"Path":"example.com/tool","Version":"1.0.0"}"#;
        assert_eq!(parse_go_module_version(output), Some("1.0.0".to_string()));
    }

    #[test]
    fn parse_go_module_version_invalid_json() {
        assert_eq!(parse_go_module_version("not json"), None);
    }

    #[test]
    fn parse_go_module_version_missing_version() {
        let output = r#"{"Path":"example.com/tool"}"#;
        assert_eq!(parse_go_module_version(output), None);
    }

    #[test]
    fn parse_go_module_version_empty_string() {
        assert_eq!(parse_go_module_version(""), None);
    }

    #[test]
    fn parse_go_module_version_null_version() {
        let output = r#"{"Path":"example.com/tool","Version":null}"#;
        assert_eq!(parse_go_module_version(output), None);
    }

    #[test]
    fn parse_go_module_version_real_world() {
        let output = r#"{
            "Path": "golang.org/x/tools/gopls",
            "Version": "v0.15.3",
            "Time": "2024-04-01T12:00:00Z",
            "GoMod": "golang.org/x/tools/gopls@v0.15.3/go.mod",
            "GoVersion": "1.21"
        }"#;
        assert_eq!(parse_go_module_version(output), Some("0.15.3".to_string()));
    }

    #[test]
    fn go_install_manager_can_bootstrap_checks_cascade() {
        let mgr = GoInstallManager;
        assert_eq!(mgr.can_bootstrap(), any_system_manager_available());
    }

    #[test]
    fn go_install_manager_is_available_checks_go() {
        let mgr = GoInstallManager;
        let available = mgr.is_available();
        assert_eq!(available, go_available());
    }

    #[test]
    fn go_update_returns_ok() {
        let mgr = GoInstallManager;
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.update(&printer).unwrap();
    }
}
