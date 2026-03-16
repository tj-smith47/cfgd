use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Output};

use cfgd_core::command_available;
use cfgd_core::config::{MergedProfile, PackagesSpec};
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::{PackageAction, PackageManager};

/// Extract stderr from command output as a lossy UTF-8 string.
fn stderr_lossy(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Run a command, mapping IO errors to PackageError::CommandFailed and non-zero
/// exit to the appropriate PackageError variant based on `error_kind`.
/// `error_kind` should be one of: "install", "uninstall", "list", "update".
/// For "list", returns ListFailed. For "update", returns InstallFailed (matching
/// existing convention). Returns the Output on success.
fn run_pkg_cmd(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
) -> std::result::Result<Output, PackageError> {
    let output = cmd.output().map_err(|e| PackageError::CommandFailed {
        manager: manager.into(),
        source: e,
    })?;
    if !output.status.success() {
        let stderr = stderr_lossy(&output);
        return Err(match error_kind {
            "install" => PackageError::InstallFailed {
                manager: manager.into(),
                message: stderr,
            },
            "uninstall" => PackageError::UninstallFailed {
                manager: manager.into(),
                message: stderr,
            },
            "list" => PackageError::ListFailed {
                manager: manager.into(),
                message: stderr,
            },
            // "update" and anything else — use InstallFailed to match existing convention
            _ => PackageError::InstallFailed {
                manager: manager.into(),
                message: stderr,
            },
        });
    }
    Ok(output)
}

/// Run a command, mapping IO errors to PackageError::CommandFailed and non-zero
/// exit to an error with a custom message prefix.
fn run_pkg_cmd_msg(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
    msg_prefix: &str,
) -> std::result::Result<Output, PackageError> {
    let output = cmd.output().map_err(|e| PackageError::CommandFailed {
        manager: manager.into(),
        source: e,
    })?;
    if !output.status.success() {
        let stderr = stderr_lossy(&output);
        let message = if msg_prefix.is_empty() {
            stderr
        } else {
            format!("{}: {}", msg_prefix, stderr)
        };
        return Err(match error_kind {
            "install" => PackageError::InstallFailed {
                manager: manager.into(),
                message,
            },
            "uninstall" => PackageError::UninstallFailed {
                manager: manager.into(),
                message,
            },
            "list" => PackageError::ListFailed {
                manager: manager.into(),
                message,
            },
            _ => PackageError::InstallFailed {
                manager: manager.into(),
                message,
            },
        });
    }
    Ok(output)
}

const LINUXBREW_PATH: &str = "/home/linuxbrew/.linuxbrew/bin/brew";

/// Check if running as root (UID 0).
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

/// Check if brew is available, including linuxbrew fallback on Linux.
fn brew_available() -> bool {
    if command_available("brew") {
        return true;
    }
    cfg!(target_os = "linux") && std::path::Path::new(LINUXBREW_PATH).exists()
}

/// After brew bootstrap, add brew's bin directories to the current process PATH
/// so that brew-installed binaries (and post-apply scripts that use them) work
/// immediately without requiring a new shell session.
fn update_path_for_brew() {
    let brew_bin = std::path::Path::new(LINUXBREW_PATH).parent().unwrap_or(std::path::Path::new("."));
    let brew_prefix = brew_bin.parent().unwrap_or(brew_bin);
    let sbin = brew_prefix.join("sbin");

    if let Ok(current_path) = std::env::var("PATH") {
        let brew_bin_str = brew_bin.to_string_lossy();
        if !current_path.contains(brew_bin_str.as_ref()) {
            // SAFETY: bootstrap runs single-threaded before any concurrent work.
            // set_var is unsafe in edition 2024 due to potential data races, but
            // we're in the reconciler's sequential apply phase here.
            unsafe {
                std::env::set_var(
                    "PATH",
                    format!("{}:{}:{}", brew_bin_str, sbin.to_string_lossy(), current_path),
                );
            }
        }
    }
}

/// Build a Command for brew, handling linuxbrew paths.
/// On Linux as root with a linuxbrew user, routes through `sudo -u linuxbrew`.
/// On Linux as non-root, uses LINUXBREW_PATH directly if brew is not in PATH.
fn brew_cmd() -> Command {
    if cfg!(target_os = "linux") && std::path::Path::new(LINUXBREW_PATH).exists() {
        if is_root() {
            let mut cmd = Command::new("sudo");
            cmd.args(["-u", "linuxbrew", LINUXBREW_PATH]);
            return cmd;
        }
        if !command_available("brew") {
            return Command::new(LINUXBREW_PATH);
        }
    }
    Command::new("brew")
}

/// Strip trailing "-VERSION" from package names where version starts with a digit.
/// Used by apk, pkg, and nix-env which output "name-version" format.
fn strip_version_suffix(name: &str) -> String {
    let bytes = name.as_bytes();
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            return name[..i].to_string();
        }
    }
    name.to_string()
}

/// Strip architecture suffix (e.g., ".x86_64", ".noarch") from package names.
/// Used by dnf and yum which output "name.arch" format.
fn strip_arch_suffix(name: &str) -> String {
    name.rsplit_once('.').map_or(name, |(n, _)| n).to_string()
}

// --- Brew ---

pub struct BrewManager;

impl BrewManager {
    fn run_brew(&self, args: &[&str]) -> std::result::Result<String, PackageError> {
        let output = run_pkg_cmd("brew", brew_cmd().args(args), "list")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn installed_taps(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["tap"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn installed_casks(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["list", "--cask", "-1"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

// --- BrewTapManager ---

pub struct BrewTapManager;

impl PackageManager for BrewTapManager {
    fn name(&self) -> &str {
        "brew-tap"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn can_bootstrap(&self) -> bool {
        false
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        BrewManager.installed_taps()
    }

    fn install(&self, taps: &[String], printer: &Printer) -> Result<()> {
        for tap in taps {
            printer.info(&format!("brew tap {}", tap));
            run_pkg_cmd_msg(
                "brew-tap",
                brew_cmd().args(["tap", tap]),
                "install",
                &format!("tap {} failed", tap),
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, taps: &[String], printer: &Printer) -> Result<()> {
        for tap in taps {
            printer.info(&format!("brew untap {}", tap));
            run_pkg_cmd_msg(
                "brew-tap",
                brew_cmd().args(["untap", tap]),
                "uninstall",
                &format!("untap {} failed", tap),
            )?;
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        // Taps don't have versions
        Ok(None)
    }
}

// --- BrewCaskManager ---

pub struct BrewCaskManager;

impl PackageManager for BrewCaskManager {
    fn name(&self) -> &str {
        "brew-cask"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn can_bootstrap(&self) -> bool {
        false
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        BrewManager.installed_casks()
    }

    fn install(&self, casks: &[String], printer: &Printer) -> Result<()> {
        if casks.is_empty() {
            return Ok(());
        }
        printer.info(&format!("brew install --cask {}", casks.join(" ")));
        run_pkg_cmd(
            "brew-cask",
            brew_cmd().arg("install").arg("--cask").args(casks),
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, casks: &[String], printer: &Printer) -> Result<()> {
        if casks.is_empty() {
            return Ok(());
        }
        printer.info(&format!("brew uninstall --cask {}", casks.join(" ")));
        run_pkg_cmd(
            "brew-cask",
            brew_cmd().arg("uninstall").arg("--cask").args(casks),
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn available_version(&self, cask: &str) -> Result<Option<String>> {
        // brew info --json=v2 --cask <pkg> → .casks[0].version
        let output = brew_cmd()
            .args(["info", "--json=v2", "--cask", cask])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew-cask".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "brew-cask".into(),
                message: format!("failed to parse brew info output: {}", e),
            })?;
        Ok(parsed
            .pointer("/casks/0/version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }
}

impl PackageManager for BrewManager {
    fn name(&self) -> &str {
        "brew"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn can_bootstrap(&self) -> bool {
        true
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        let install_url = "https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh";

        if cfg!(target_os = "linux") && is_root() {
            // Linuxbrew-as-root: create linuxbrew user, install as that user
            printer.info("Creating linuxbrew system user");
            let user_status = Command::new("useradd")
                .args([
                    "--system",
                    "--create-home",
                    "--shell",
                    "/bin/bash",
                    "linuxbrew",
                ])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!("failed to create linuxbrew user: {}", e),
                })?;
            // Exit code 9 = user already exists, which is fine
            if !user_status.success() && user_status.code() != Some(9) {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: "failed to create linuxbrew system user".into(),
                }
                .into());
            }

            printer.info("Installing Homebrew as linuxbrew user");
            let status = Command::new("sudo")
                .args(["-u", "linuxbrew", "bash", "-c"])
                .arg(format!(
                    "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL {})\"",
                    install_url
                ))
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!("homebrew install failed: {}", e),
                })?;
            if !status.success() {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: "homebrew install script failed".into(),
                }
                .into());
            }

            update_path_for_brew();
        } else {
            printer.info("Installing Homebrew");
            let status = Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL {})\"",
                    install_url
                ))
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!("homebrew install failed: {}", e),
                })?;
            if !status.success() {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: "homebrew install script failed".into(),
                }
                .into());
            }

            update_path_for_brew();
        }

        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["list", "--formulae", "-1"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("brew install {}", packages.join(" ")));
        run_pkg_cmd("brew", brew_cmd().arg("install").args(packages), "install")?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("brew uninstall {}", packages.join(" ")));
        run_pkg_cmd(
            "brew",
            brew_cmd().arg("uninstall").args(packages),
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("brew update");
        run_pkg_cmd_msg("brew", brew_cmd().arg("update"), "update", "update failed")?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // brew info --json=v2 <pkg> → .formulae[0].versions.stable
        let output = brew_cmd()
            .args(["info", "--json=v2", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "brew".into(),
                message: format!("failed to parse brew info output: {}", e),
            })?;
        Ok(parsed
            .pointer("/formulae/0/versions/stable")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }
}

// --- SimpleManager (data-driven package manager) ---

/// A data-driven package manager for system package managers that follow a
/// uniform pattern: list installed, install, uninstall, update.
/// Replaces individual structs for apt, dnf, yum, apk, pacman, zypper, pkg.
pub struct SimpleManager {
    mgr_name: &'static str,
    list_cmd: &'static [&'static str],
    install_cmd: &'static [&'static str],
    uninstall_cmd: &'static [&'static str],
    update_cmd: Option<&'static [&'static str]>,
    /// When true, non-zero exit from the update command is ignored (dnf/yum
    /// check-update returns 100 when updates are available).
    ignore_update_exit: bool,
    parse_list: fn(&str) -> HashSet<String>,
    query_version: fn(&str, &str) -> Result<Option<String>>,
    /// Custom availability check. When None, uses `command_available(mgr_name)`.
    is_available_fn: Option<fn() -> bool>,
}

impl SimpleManager {
    fn display_cmd(&self, cmd_parts: &[&str], packages: &[String]) -> String {
        let mut parts: Vec<&str> = cmd_parts.to_vec();
        for p in packages {
            parts.push(p);
        }
        parts.join(" ")
    }
}

impl PackageManager for SimpleManager {
    fn name(&self) -> &str {
        self.mgr_name
    }

    fn is_available(&self) -> bool {
        if let Some(f) = self.is_available_fn {
            f()
        } else {
            command_available(self.mgr_name)
        }
    }

    fn can_bootstrap(&self) -> bool {
        false
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let (prog, args) = self.list_cmd.split_first().unwrap_or((&"true", &[]));
        let output = run_pkg_cmd(self.mgr_name, Command::new(prog).args(args), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok((self.parse_list)(&stdout))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&self.display_cmd(self.install_cmd, packages));
        let (prog, args) = self.install_cmd.split_first().unwrap_or((&"true", &[]));
        run_pkg_cmd(
            self.mgr_name,
            Command::new(prog).args(args).args(packages),
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&self.display_cmd(self.uninstall_cmd, packages));
        let (prog, args) = self.uninstall_cmd.split_first().unwrap_or((&"true", &[]));
        run_pkg_cmd(
            self.mgr_name,
            Command::new(prog).args(args).args(packages),
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        let Some(update_parts) = self.update_cmd else {
            return Ok(());
        };
        printer.info(&self.display_cmd(update_parts, &[]));
        let (prog, args) = update_parts.split_first().unwrap_or((&"true", &[]));
        if self.ignore_update_exit {
            let _ = Command::new(prog).args(args).output().map_err(|e| {
                PackageError::CommandFailed {
                    manager: self.mgr_name.into(),
                    source: e,
                }
            })?;
        } else {
            run_pkg_cmd_msg(
                self.mgr_name,
                Command::new(prog).args(args),
                "update",
                "update failed",
            )?;
        }
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        (self.query_version)(self.mgr_name, package)
    }
}

// --- Parse helpers for SimpleManager ---

fn parse_simple_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn parse_dnf_yum_lines(stdout: &str, skip_prefixes: &[&str]) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.is_empty() && !skip_prefixes.iter().any(|prefix| l.starts_with(prefix)))
        .filter_map(|l| {
            let name = l.split_whitespace().next()?;
            Some(strip_arch_suffix(name))
        })
        .collect()
}

fn parse_dnf_lines(stdout: &str) -> HashSet<String> {
    parse_dnf_yum_lines(stdout, &["Installed", "Last"])
}

fn parse_yum_lines(stdout: &str) -> HashSet<String> {
    parse_dnf_yum_lines(stdout, &["Installed", "Loaded"])
}

fn parse_apk_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let name = l.split_whitespace().next()?;
            Some(strip_version_suffix(name))
        })
        .collect()
}

fn parse_zypper_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| l.contains('|') && !l.starts_with("--") && !l.starts_with("S "))
        .filter_map(|l| {
            let cols: Vec<&str> = l.split('|').map(|c| c.trim()).collect();
            if cols.len() >= 3 {
                let name = cols[1].trim();
                if !name.is_empty() && name != "Name" {
                    return Some(name.to_string());
                }
            }
            None
        })
        .collect()
}

fn parse_pkg_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| strip_version_suffix(l.trim()))
        .collect()
}

// --- Version query helpers for SimpleManager ---

/// Query version via `<cmd> info <pkg>` and parse "Version:" field.
/// Used by dnf, yum, pacman (-Si), zypper.
fn query_version_info(manager: &str, package: &str) -> Result<Option<String>> {
    let (cmd, args): (&str, &[&str]) = match manager {
        "pacman" => ("pacman", &["-Si"]),
        _ => (manager, &["info"]),
    };
    let output = Command::new(cmd)
        .args(args)
        .arg(package)
        .output()
        .map_err(|e| PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Version")
            && let Some(version) = rest.trim_start().strip_prefix(':')
        {
            return Ok(Some(version.trim().to_string()));
        }
    }
    Ok(None)
}

fn query_version_apt(manager: &str, package: &str) -> Result<Option<String>> {
    let output = Command::new("apt-cache")
        .args(["policy", package])
        .output()
        .map_err(|e| PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Candidate:") {
            let version = rest.trim();
            if version == "(none)" {
                return Ok(None);
            }
            // apt versions often have epoch:version-revision, strip to just version
            let version = version
                .split_once(':')
                .map_or(version, |(_, v)| v)
                .split_once('-')
                .map_or_else(
                    || version.split_once(':').map_or(version, |(_, v)| v),
                    |(v, _)| v,
                );
            return Ok(Some(version.to_string()));
        }
    }
    Ok(None)
}

fn query_version_apk(manager: &str, package: &str) -> Result<Option<String>> {
    let output = Command::new("apk")
        .args(["policy", package])
        .output()
        .map_err(|e| PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // apk policy output format: "package-version:" on first line
    if let Some(first_line) = stdout.lines().next() {
        let trimmed = first_line.trim().trim_end_matches(':');
        let bytes = trimmed.as_bytes();
        for i in (0..bytes.len()).rev() {
            if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                return Ok(Some(trimmed[i + 1..].to_string()));
            }
        }
    }
    Ok(None)
}

fn query_version_pkg(manager: &str, package: &str) -> Result<Option<String>> {
    let output = Command::new("pkg")
        .args(["search", "-e", package])
        .output()
        .map_err(|e| PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let name_ver = line.split_whitespace().next().unwrap_or("");
        let bytes = name_ver.as_bytes();
        for i in (0..bytes.len()).rev() {
            if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                let name = &name_ver[..i];
                if name == package {
                    return Ok(Some(name_ver[i + 1..].to_string()));
                }
                break;
            }
        }
    }
    Ok(None)
}

// --- SimpleManager constructors ---

fn apt_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "apt",
        list_cmd: &["dpkg-query", "-W", "-f", "${Package}\n"],
        install_cmd: &["sudo", "apt", "install", "-y"],
        uninstall_cmd: &["sudo", "apt", "remove", "-y"],
        update_cmd: Some(&["sudo", "apt", "update"]),
        ignore_update_exit: false,
        parse_list: parse_simple_lines,
        query_version: query_version_apt,
        is_available_fn: None,
    }
}

fn dnf_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "dnf",
        list_cmd: &["dnf", "list", "installed", "--quiet"],
        install_cmd: &["sudo", "dnf", "install", "-y"],
        uninstall_cmd: &["sudo", "dnf", "remove", "-y"],
        update_cmd: Some(&["sudo", "dnf", "check-update"]),
        ignore_update_exit: true,
        parse_list: parse_dnf_lines,
        query_version: query_version_info,
        is_available_fn: None,
    }
}

fn yum_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "yum",
        list_cmd: &["yum", "list", "installed", "--quiet"],
        install_cmd: &["sudo", "yum", "install", "-y"],
        uninstall_cmd: &["sudo", "yum", "remove", "-y"],
        update_cmd: Some(&["sudo", "yum", "check-update"]),
        ignore_update_exit: true,
        parse_list: parse_yum_lines,
        query_version: query_version_info,
        is_available_fn: Some(|| !command_available("dnf") && command_available("yum")),
    }
}

fn apk_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "apk",
        list_cmd: &["apk", "list", "--installed", "--quiet"],
        install_cmd: &["apk", "add"],
        uninstall_cmd: &["apk", "del"],
        update_cmd: Some(&["apk", "update"]),
        ignore_update_exit: false,
        parse_list: parse_apk_lines,
        query_version: query_version_apk,
        is_available_fn: None,
    }
}

fn pacman_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "pacman",
        list_cmd: &["pacman", "-Qq"],
        install_cmd: &["sudo", "pacman", "-S", "--noconfirm"],
        uninstall_cmd: &["sudo", "pacman", "-R", "--noconfirm"],
        update_cmd: Some(&["sudo", "pacman", "-Sy", "--noconfirm"]),
        ignore_update_exit: false,
        parse_list: parse_simple_lines,
        query_version: query_version_info,
        is_available_fn: None,
    }
}

fn zypper_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "zypper",
        list_cmd: &[
            "zypper",
            "se",
            "--installed-only",
            "--type",
            "package",
            "-s",
        ],
        install_cmd: &["sudo", "zypper", "install", "-y"],
        uninstall_cmd: &["sudo", "zypper", "remove", "-y"],
        update_cmd: Some(&["sudo", "zypper", "refresh"]),
        ignore_update_exit: false,
        parse_list: parse_zypper_lines,
        query_version: query_version_info,
        is_available_fn: None,
    }
}

fn pkg_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "pkg",
        list_cmd: &["pkg", "info", "-q"],
        install_cmd: &["pkg", "install", "-y"],
        uninstall_cmd: &["pkg", "remove", "-y"],
        update_cmd: Some(&["pkg", "update"]),
        ignore_update_exit: false,
        parse_list: parse_pkg_lines,
        query_version: query_version_pkg,
        is_available_fn: None,
    }
}

// --- Cargo ---

pub struct CargoManager;

/// Check if cargo is available, including ~/.cargo/bin fallback.
fn cargo_available() -> bool {
    if command_available("cargo") {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = std::path::PathBuf::from(home).join(".cargo/bin/cargo");
        return cargo_bin.exists();
    }
    false
}

/// Get the cargo command, preferring PATH but falling back to ~/.cargo/bin.
fn cargo_cmd() -> Command {
    if command_available("cargo") {
        return Command::new("cargo");
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = std::path::PathBuf::from(home).join(".cargo/bin/cargo");
        if cargo_bin.exists() {
            return Command::new(cargo_bin);
        }
    }
    Command::new("cargo")
}

impl PackageManager for CargoManager {
    fn name(&self) -> &str {
        "cargo"
    }

    fn is_available(&self) -> bool {
        cargo_available()
    }

    fn can_bootstrap(&self) -> bool {
        command_available("curl")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        printer.info("Installing Rust via rustup");
        let status = Command::new("bash")
            .arg("-c")
            .arg("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y")
            .status()
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "cargo".into(),
                message: format!("rustup install failed: {}", e),
            })?;
        if !status.success() {
            return Err(PackageError::BootstrapFailed {
                manager: "cargo".into(),
                message: "rustup install script failed".into(),
            }
            .into());
        }
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("cargo", cargo_cmd().args(["install", "--list"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // `cargo install --list` format: "package_name v1.2.3:" followed by indented binary names
        // We only care about the package names (lines that don't start with whitespace)
        Ok(stdout
            .lines()
            .filter(|l| !l.starts_with(' ') && !l.is_empty())
            .filter_map(|l| l.split_whitespace().next())
            .map(|s| s.to_string())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("cargo install {}", pkg));
            run_pkg_cmd_msg("cargo", cargo_cmd().args(["install", pkg]), "install", pkg)?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("cargo uninstall {}", pkg));
            run_pkg_cmd_msg(
                "cargo",
                cargo_cmd().args(["uninstall", pkg]),
                "uninstall",
                pkg,
            )?;
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // cargo install re-installs to update; no separate update command
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // cargo search <pkg> --limit 1 → "package_name = \"version\""
        let output = cargo_cmd()
            .args(["search", package, "--limit", "1"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "cargo".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // First line format: `package_name = "1.2.3"    # description`
        // Only match if the package name exactly matches
        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(3, '"').collect();
            if parts.len() >= 2 {
                let name = line.split_whitespace().next().unwrap_or("");
                if name == package {
                    return Ok(Some(parts[1].to_string()));
                }
            }
        }
        Ok(None)
    }
}

// --- Npm ---

pub struct NpmManager;

/// Find npm binary, checking PATH and common nvm install locations.
fn find_npm() -> Option<std::path::PathBuf> {
    if command_available("npm") {
        return Some(std::path::PathBuf::from("npm"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let nvm_dir = std::path::PathBuf::from(home).join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&nvm_dir) {
            for entry in entries.flatten() {
                let npm_path = entry.path().join("bin/npm");
                if npm_path.exists() {
                    return Some(npm_path);
                }
            }
        }
    }
    None
}

fn npm_available() -> bool {
    find_npm().is_some()
}

fn npm_cmd() -> Command {
    Command::new(find_npm().unwrap_or_else(|| std::path::PathBuf::from("npm")))
}

impl PackageManager for NpmManager {
    fn name(&self) -> &str {
        "npm"
    }

    fn is_available(&self) -> bool {
        npm_available()
    }

    fn can_bootstrap(&self) -> bool {
        // Can bootstrap via system package manager or nvm
        brew_available()
            || command_available("apt")
            || command_available("dnf")
            || command_available("curl")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        // Try system package managers first, fall back to nvm
        if brew_available() {
            printer.info("Installing node via brew");
            let output = brew_cmd().args(["install", "node"]).output().map_err(|e| {
                PackageError::BootstrapFailed {
                    manager: "npm".into(),
                    message: format!("brew install node failed: {}", e),
                }
            })?;
            if output.status.success() {
                return Ok(());
            }
        }

        if command_available("apt") {
            printer.info("Installing nodejs via apt");
            let status = Command::new("sudo")
                .args(["apt", "install", "-y", "nodejs", "npm"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "npm".into(),
                    message: format!("apt install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        if command_available("dnf") {
            printer.info("Installing nodejs via dnf");
            let status = Command::new("sudo")
                .args(["dnf", "install", "-y", "nodejs", "npm"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "npm".into(),
                    message: format!("dnf install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        // Fall back to nvm
        if command_available("curl") {
            printer.info("Installing Node.js via nvm");
            let status = Command::new("bash")
                .arg("-c")
                .arg(concat!(
                    "curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash && ",
                    "export NVM_DIR=\"$HOME/.nvm\" && [ -s \"$NVM_DIR/nvm.sh\" ] && . \"$NVM_DIR/nvm.sh\" && ",
                    "nvm install --lts"
                ))
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "npm".into(),
                    message: format!("nvm install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        Err(PackageError::BootstrapFailed {
            manager: "npm".into(),
            message: "no installation method available".into(),
        }
        .into())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = npm_cmd()
            .args(["list", "-g", "--depth=0", "--json"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;

        // npm list exits non-zero if there are peer dep issues, but still produces valid JSON
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "npm".into(),
                message: format!("failed to parse npm list output: {}", e),
            })?;

        let mut packages = HashSet::new();
        if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_object()) {
            for key in deps.keys() {
                packages.insert(key.clone());
            }
        }

        Ok(packages)
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("npm install -g {}", packages.join(" ")));
        run_pkg_cmd(
            "npm",
            npm_cmd().arg("install").arg("-g").args(packages),
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("npm uninstall -g {}", packages.join(" ")));
        run_pkg_cmd(
            "npm",
            npm_cmd().arg("uninstall").arg("-g").args(packages),
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("npm update -g");
        run_pkg_cmd_msg(
            "npm",
            npm_cmd().args(["update", "-g"]),
            "update",
            "update failed",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // npm view <pkg> version
        let output = npm_cmd()
            .args(["view", package, "version"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if version.is_empty() {
            Ok(None)
        } else {
            Ok(Some(version))
        }
    }
}

// --- Pipx ---

pub struct PipxManager;

/// Find pipx binary, checking PATH and ~/.local/bin fallback.
fn find_pipx() -> Option<std::path::PathBuf> {
    if command_available("pipx") {
        return Some(std::path::PathBuf::from("pipx"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let local_bin = std::path::PathBuf::from(home).join(".local/bin/pipx");
        if local_bin.exists() {
            return Some(local_bin);
        }
    }
    None
}

fn pipx_available() -> bool {
    find_pipx().is_some()
}

fn pipx_cmd() -> Command {
    Command::new(find_pipx().unwrap_or_else(|| std::path::PathBuf::from("pipx")))
}

impl PackageManager for PipxManager {
    fn name(&self) -> &str {
        "pipx"
    }

    fn is_available(&self) -> bool {
        pipx_available()
    }

    fn can_bootstrap(&self) -> bool {
        // Can bootstrap via system package manager or pip
        brew_available()
            || command_available("apt")
            || command_available("dnf")
            || command_available("pip3")
            || command_available("pip")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        // Try system package managers first, fall back to pip
        if brew_available() {
            printer.info("Installing pipx via brew");
            let output = brew_cmd().args(["install", "pipx"]).output().map_err(|e| {
                PackageError::BootstrapFailed {
                    manager: "pipx".into(),
                    message: format!("brew install pipx failed: {}", e),
                }
            })?;
            if output.status.success() {
                return Ok(());
            }
        }

        if command_available("apt") {
            printer.info("Installing pipx via apt");
            let status = Command::new("sudo")
                .args(["apt", "install", "-y", "pipx"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "pipx".into(),
                    message: format!("apt install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        if command_available("dnf") {
            printer.info("Installing pipx via dnf");
            let status = Command::new("sudo")
                .args(["dnf", "install", "-y", "pipx"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "pipx".into(),
                    message: format!("dnf install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        // Fall back to pip
        let pip_cmd = if command_available("pip3") {
            "pip3"
        } else if command_available("pip") {
            "pip"
        } else {
            return Err(PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: "no installation method available".into(),
            }
            .into());
        };

        printer.info(&format!("Installing pipx via {}", pip_cmd));
        let status = Command::new(pip_cmd)
            .args(["install", "--user", "pipx"])
            .status()
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: format!("{} install failed: {}", pip_cmd, e),
            })?;
        if !status.success() {
            return Err(PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: format!("{} install --user pipx failed", pip_cmd),
            }
            .into());
        }

        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("pipx", pipx_cmd().args(["list", "--json"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "pipx".into(),
                message: format!("failed to parse pipx list output: {}", e),
            })?;

        let mut packages = HashSet::new();
        if let Some(venvs) = parsed.get("venvs").and_then(|v| v.as_object()) {
            for key in venvs.keys() {
                packages.insert(key.clone());
            }
        }

        Ok(packages)
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("pipx install {}", pkg));
            run_pkg_cmd_msg("pipx", pipx_cmd().args(["install", pkg]), "install", pkg)?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("pipx uninstall {}", pkg));
            run_pkg_cmd_msg(
                "pipx",
                pipx_cmd().args(["uninstall", pkg]),
                "uninstall",
                pkg,
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("pipx upgrade-all");
        run_pkg_cmd_msg(
            "pipx",
            pipx_cmd().args(["upgrade-all"]),
            "update",
            "upgrade-all failed",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // Query PyPI JSON API: https://pypi.org/pypi/<pkg>/json → .info.version
        let url = format!("https://pypi.org/pypi/{}/json", package);
        let output = Command::new("curl")
            .args(["-fsSL", &url])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "pipx".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "pipx".into(),
                message: format!("failed to parse PyPI response: {}", e),
            })?;
        Ok(parsed
            .pointer("/info/version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }
}

// --- Snap ---

pub struct SnapManager;

impl PackageManager for SnapManager {
    fn name(&self) -> &str {
        "snap"
    }

    fn is_available(&self) -> bool {
        command_available("snap")
    }

    fn can_bootstrap(&self) -> bool {
        // snapd is pre-installed on Ubuntu; on other distros, install via system manager
        command_available("apt") || command_available("dnf") || command_available("zypper")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        if command_available("apt") {
            printer.info("Installing snapd via apt");
            let status = Command::new("sudo")
                .args(["apt", "install", "-y", "snapd"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "snap".into(),
                    message: format!("apt install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        if command_available("dnf") {
            printer.info("Installing snapd via dnf");
            let status = Command::new("sudo")
                .args(["dnf", "install", "-y", "snapd"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "snap".into(),
                    message: format!("dnf install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        if command_available("zypper") {
            printer.info("Installing snapd via zypper");
            let status = Command::new("sudo")
                .args(["zypper", "install", "-y", "snapd"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "snap".into(),
                    message: format!("zypper install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        Err(PackageError::BootstrapFailed {
            manager: "snap".into(),
            message: "no installation method available".into(),
        }
        .into())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("snap", Command::new("snap").args(["list"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // snap list output: "Name  Version  Rev  Tracking  Publisher  Notes"
        // Skip header line
        Ok(stdout
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().next().map(|s| s.to_string()))
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        // Snap requires individual install commands for --classic flag per package
        for pkg in packages {
            printer.info(&format!("snap install {}", pkg));
            let result = run_pkg_cmd_msg(
                "snap",
                Command::new("sudo").arg("snap").arg("install").arg(pkg),
                "install",
                pkg,
            );
            if let Err(ref e) = result {
                // If install fails and stderr mentions classic confinement, retry with --classic
                if e.to_string().contains("classic") {
                    printer.info(&format!("snap install --classic {}", pkg));
                    run_pkg_cmd_msg(
                        "snap",
                        Command::new("sudo").args(["snap", "install", "--classic", pkg]),
                        "install",
                        pkg,
                    )?;
                } else {
                    result?;
                }
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("snap remove {}", packages.join(" ")));
        run_pkg_cmd(
            "snap",
            Command::new("sudo")
                .arg("snap")
                .arg("remove")
                .args(packages),
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("snap refresh");
        run_pkg_cmd_msg(
            "snap",
            Command::new("sudo").args(["snap", "refresh"]),
            "update",
            "refresh failed",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // snap info <pkg> → parse "latest/stable:" or first channel line for version
        let output = Command::new("snap")
            .args(["info", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "snap".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            // Look for "latest/stable:" or "stable:" channel line
            if trimmed.starts_with("latest/stable:") || trimmed.starts_with("stable:") {
                // Format: "latest/stable: 0.10.2 2024-01-01 (1234) 12MB classic"
                let parts: Vec<&str> = trimmed.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let version = parts[1].split_whitespace().next().unwrap_or("");
                    if !version.is_empty() && version != "^" && version != "--" {
                        return Ok(Some(version.to_string()));
                    }
                }
            }
        }
        Ok(None)
    }
}

// --- Flatpak ---

pub struct FlatpakManager;

impl PackageManager for FlatpakManager {
    fn name(&self) -> &str {
        "flatpak"
    }

    fn is_available(&self) -> bool {
        command_available("flatpak")
    }

    fn can_bootstrap(&self) -> bool {
        command_available("apt") || command_available("dnf") || command_available("zypper")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        if command_available("apt") {
            printer.info("Installing flatpak via apt");
            let status = Command::new("sudo")
                .args(["apt", "install", "-y", "flatpak"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "flatpak".into(),
                    message: format!("apt install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        if command_available("dnf") {
            printer.info("Installing flatpak via dnf");
            let status = Command::new("sudo")
                .args(["dnf", "install", "-y", "flatpak"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "flatpak".into(),
                    message: format!("dnf install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        if command_available("zypper") {
            printer.info("Installing flatpak via zypper");
            let status = Command::new("sudo")
                .args(["zypper", "install", "-y", "flatpak"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "flatpak".into(),
                    message: format!("zypper install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        Err(PackageError::BootstrapFailed {
            manager: "flatpak".into(),
            message: "no installation method available".into(),
        }
        .into())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd(
            "flatpak",
            Command::new("flatpak").args(["list", "--app", "--columns=application"]),
            "list",
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("flatpak install -y {}", pkg));
            run_pkg_cmd_msg(
                "flatpak",
                Command::new("flatpak").args(["install", "-y", pkg]),
                "install",
                pkg,
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("flatpak uninstall -y {}", pkg));
            run_pkg_cmd_msg(
                "flatpak",
                Command::new("flatpak").args(["uninstall", "-y", pkg]),
                "uninstall",
                pkg,
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("flatpak update -y");
        run_pkg_cmd_msg(
            "flatpak",
            Command::new("flatpak").args(["update", "-y"]),
            "update",
            "update failed",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // flatpak remote-info flathub <app-id> → parse "Version:" field
        let output = Command::new("flatpak")
            .args(["remote-info", "flathub", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "flatpak".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Version:") {
                return Ok(Some(rest.trim().to_string()));
            }
        }
        Ok(None)
    }
}

// --- Nix ---

pub struct NixManager;

impl PackageManager for NixManager {
    fn name(&self) -> &str {
        "nix"
    }

    fn is_available(&self) -> bool {
        command_available("nix-env") || command_available("nix")
    }

    fn can_bootstrap(&self) -> bool {
        command_available("curl")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        printer.info("Installing Nix");
        let status = Command::new("bash")
            .arg("-c")
            .arg("curl -L https://nixos.org/nix/install | sh -s -- --daemon")
            .status()
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "nix".into(),
                message: format!("nix install failed: {}", e),
            })?;
        if !status.success() {
            return Err(PackageError::BootstrapFailed {
                manager: "nix".into(),
                message: "nix install script failed".into(),
            }
            .into());
        }
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        // Try `nix profile list` first (new-style), fall back to `nix-env -q`
        if command_available("nix") {
            let output = Command::new("nix")
                .args(["profile", "list"])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "nix".into(),
                    source: e,
                })?;

            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // nix profile list output: index, flake ref, store path — extract package name
                // from the flake ref or store path
                let packages: HashSet<String> = stdout
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| {
                        // Format varies; extract the package name from the last path component
                        let parts: Vec<&str> = l.split_whitespace().collect();
                        if parts.len() >= 2 {
                            // Try to extract from flake ref like "nixpkgs#ripgrep"
                            if let Some(name) = parts[1].rsplit('#').next() {
                                return Some(name.to_string());
                            }
                        }
                        None
                    })
                    .collect();
                return Ok(packages);
            }
        }

        // Fallback: nix-env -q
        let output = run_pkg_cmd(
            "nix",
            Command::new("nix-env").args(["-q", "--no-name", "--attr-path"]),
            "list",
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // nix-env -q output: "name-version" — strip version
        Ok(stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| strip_version_suffix(l.trim()))
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            // Prefer nix profile install (new-style) over nix-env -i
            if command_available("nix") {
                printer.info(&format!("nix profile install nixpkgs#{}", pkg));
                run_pkg_cmd_msg(
                    "nix",
                    Command::new("nix").args(["profile", "install", &format!("nixpkgs#{}", pkg)]),
                    "install",
                    pkg,
                )?;
            } else {
                printer.info(&format!("nix-env -iA nixpkgs.{}", pkg));
                run_pkg_cmd_msg(
                    "nix",
                    Command::new("nix-env").args(["-iA", &format!("nixpkgs.{}", pkg)]),
                    "install",
                    pkg,
                )?;
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            if command_available("nix") {
                printer.info(&format!("nix profile remove nixpkgs#{}", pkg));
                run_pkg_cmd_msg(
                    "nix",
                    Command::new("nix").args(["profile", "remove", &format!("nixpkgs#{}", pkg)]),
                    "uninstall",
                    pkg,
                )?;
            } else {
                printer.info(&format!("nix-env -e {}", pkg));
                run_pkg_cmd_msg(
                    "nix",
                    Command::new("nix-env").args(["-e", pkg]),
                    "uninstall",
                    pkg,
                )?;
            }
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // Nix packages are pinned; update is a no-op (channels are managed separately)
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // nix search nixpkgs <pkg> --json → parse version from first matching result
        if command_available("nix") {
            let output = Command::new("nix")
                .args(["search", "nixpkgs", package, "--json"])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "nix".into(),
                    source: e,
                })?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout)
                    && let Some(obj) = parsed.as_object()
                {
                    for value in obj.values() {
                        if let Some(version) = value.get("version").and_then(|v| v.as_str())
                            && !version.is_empty()
                        {
                            return Ok(Some(version.to_string()));
                        }
                    }
                }
            }
        }
        Ok(None)
    }
}

// --- Go Install ---

pub struct GoInstallManager;

/// Find go binary, checking PATH and common install locations.
fn find_go() -> Option<std::path::PathBuf> {
    if command_available("go") {
        return Some(std::path::PathBuf::from("go"));
    }
    for path in ["/usr/local/go/bin/go", "/usr/local/bin/go"] {
        if std::path::Path::new(path).exists() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let go_bin = std::path::PathBuf::from(home).join("go/bin/go");
        if go_bin.exists() {
            return Some(go_bin);
        }
    }
    None
}

fn go_available() -> bool {
    find_go().is_some()
}

fn go_cmd() -> Command {
    Command::new(find_go().unwrap_or_else(|| std::path::PathBuf::from("go")))
}

impl PackageManager for GoInstallManager {
    fn name(&self) -> &str {
        "go"
    }

    fn is_available(&self) -> bool {
        go_available()
    }

    fn can_bootstrap(&self) -> bool {
        // Go can be bootstrapped via system package managers
        brew_available() || command_available("apt") || command_available("dnf")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        if brew_available() {
            printer.info("Installing Go via brew");
            let output = brew_cmd().args(["install", "go"]).output().map_err(|e| {
                PackageError::BootstrapFailed {
                    manager: "go".into(),
                    message: format!("brew install go failed: {}", e),
                }
            })?;
            if output.status.success() {
                return Ok(());
            }
        }

        if command_available("apt") {
            printer.info("Installing Go via apt");
            let status = Command::new("sudo")
                .args(["apt", "install", "-y", "golang"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "go".into(),
                    message: format!("apt install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        if command_available("dnf") {
            printer.info("Installing Go via dnf");
            let status = Command::new("sudo")
                .args(["dnf", "install", "-y", "golang"])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "go".into(),
                    message: format!("dnf install failed: {}", e),
                })?;
            if status.success() {
                return Ok(());
            }
        }

        Err(PackageError::BootstrapFailed {
            manager: "go".into(),
            message: "no installation method available".into(),
        }
        .into())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        // Scan $GOPATH/bin (or $HOME/go/bin) for installed binaries
        let gopath = std::env::var("GOPATH").ok().unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|h| format!("{}/go", h))
                .unwrap_or_default()
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
            printer.info(&format!("go install {}", install_path));
            run_pkg_cmd_msg(
                "go",
                go_cmd().args(["install", &install_path]),
                "install",
                pkg,
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        // Go has no uninstall command; remove binaries from $GOPATH/bin
        let gopath = std::env::var("GOPATH").ok().unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|h| format!("{}/go", h))
                .unwrap_or_default()
        });

        let bin_dir = std::path::PathBuf::from(&gopath).join("bin");
        for pkg in packages {
            // The binary name is the last path component of the module path
            let bin_name = pkg.rsplit('/').next().unwrap_or(pkg);
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
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout)
            && let Some(version) = parsed.get("Version").and_then(|v| v.as_str())
        {
            // Go versions are prefixed with "v", strip it for consistency
            let version = version.strip_prefix('v').unwrap_or(version);
            return Ok(Some(version.to_string()));
        }
        Ok(None)
    }
}

// --- Custom (user-defined) package manager ---

pub struct ScriptedManager {
    mgr_name: String,
    check_cmd: String,
    list_cmd: String,
    install_cmd: String,
    uninstall_cmd: String,
    update_cmd: Option<String>,
}

impl ScriptedManager {
    pub fn from_spec(spec: &cfgd_core::config::CustomManagerSpec) -> Self {
        Self {
            mgr_name: spec.name.clone(),
            check_cmd: spec.check.clone(),
            list_cmd: spec.list_installed.clone(),
            install_cmd: spec.install.clone(),
            uninstall_cmd: spec.uninstall.clone(),
            update_cmd: spec.update.clone(),
        }
    }

    fn run_template(
        &self,
        template: &str,
        packages: &[String],
        printer: &Printer,
        error_kind: &str,
    ) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        if template.contains("{package}") {
            // One-at-a-time mode
            for pkg in packages {
                let cmd = template.replace("{package}", pkg);
                printer.info(&cmd);
                run_pkg_cmd_msg(
                    &self.mgr_name,
                    Command::new("sh").args(["-c", &cmd]),
                    error_kind,
                    pkg,
                )?;
            }
        } else {
            // Batch mode: {packages} or append
            let joined = packages.join(" ");
            let cmd = if template.contains("{packages}") {
                template.replace("{packages}", &joined)
            } else {
                format!("{} {}", template, joined)
            };
            printer.info(&cmd);
            run_pkg_cmd(
                &self.mgr_name,
                Command::new("sh").args(["-c", &cmd]),
                error_kind,
            )?;
        }
        Ok(())
    }
}

impl PackageManager for ScriptedManager {
    fn name(&self) -> &str {
        &self.mgr_name
    }

    fn is_available(&self) -> bool {
        Command::new("sh")
            .args(["-c", &self.check_cmd])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn can_bootstrap(&self) -> bool {
        false
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd(
            &self.mgr_name,
            Command::new("sh").args(["-c", &self.list_cmd]),
            "list",
        )?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        self.run_template(&self.install_cmd, packages, printer, "install")
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        self.run_template(&self.uninstall_cmd, packages, printer, "uninstall")
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        if let Some(ref cmd) = self.update_cmd {
            printer.info(cmd);
            run_pkg_cmd_msg(
                &self.mgr_name,
                Command::new("sh").args(["-c", cmd]),
                "update",
                "update failed",
            )?;
        }
        Ok(())
    }

    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        // Custom managers don't have a standard way to query available versions
        Ok(None)
    }
}

/// Create ScriptedManager instances from custom manager specs.
pub fn custom_managers(
    specs: &[cfgd_core::config::CustomManagerSpec],
) -> Vec<Box<dyn PackageManager>> {
    specs
        .iter()
        .map(|s| Box::new(ScriptedManager::from_spec(s)) as Box<dyn PackageManager>)
        .collect()
}

// --- Package Reconciler ---

/// Bootstrap method description for display in plan/doctor output.
pub fn bootstrap_method(manager: &dyn PackageManager) -> &'static str {
    match manager.name() {
        "brew" => "homebrew installer",
        "cargo" => "rustup",
        "npm" => {
            if brew_available() {
                "brew"
            } else if command_available("apt") {
                "apt"
            } else if command_available("dnf") {
                "dnf"
            } else {
                "nvm"
            }
        }
        "pipx" => {
            if brew_available() {
                "brew"
            } else if command_available("apt") {
                "apt"
            } else if command_available("dnf") {
                "dnf"
            } else {
                "pip"
            }
        }
        "snap" => {
            if command_available("apt") {
                "apt"
            } else if command_available("dnf") {
                "dnf"
            } else {
                "zypper"
            }
        }
        "flatpak" => {
            if command_available("apt") {
                "apt"
            } else if command_available("dnf") {
                "dnf"
            } else {
                "zypper"
            }
        }
        "nix" => "nix installer",
        "go" => {
            if brew_available() {
                "brew"
            } else if command_available("apt") {
                "apt"
            } else {
                "dnf"
            }
        }
        _ => "system",
    }
}

/// Plan package actions by diffing installed vs desired for all managers.
/// Handles bootstrap: unavailable managers that can be bootstrapped get Bootstrap
/// actions before their Install actions.
pub fn plan_packages(
    profile: &MergedProfile,
    managers: &[&dyn PackageManager],
) -> Result<Vec<PackageAction>> {
    let mut actions = Vec::new();

    // Pass 1: determine which managers will be bootstrapped
    let mut bootstrapping: HashSet<String> = HashSet::new();
    for manager in managers {
        let desired = cfgd_core::config::desired_packages_for(manager.name(), profile);
        if desired.is_empty() {
            continue;
        }
        if !manager.is_available() && manager.can_bootstrap() {
            bootstrapping.insert(manager.name().to_string());
        }
    }

    // Pass 2: generate actions
    for manager in managers {
        let desired = cfgd_core::config::desired_packages_for(manager.name(), profile);
        if desired.is_empty() {
            continue;
        }

        if manager.is_available() {
            // Normal path: diff installed vs desired
            let installed = manager.installed_packages()?;
            let to_install: Vec<String> = desired
                .iter()
                .filter(|p| !installed.contains(*p))
                .cloned()
                .collect();

            if !to_install.is_empty() {
                actions.push(PackageAction::Install {
                    manager: manager.name().to_string(),
                    packages: to_install,
                    origin: "local".to_string(),
                });
            }
        } else if manager.can_bootstrap() {
            // Unavailable but bootstrappable: add Bootstrap + Install all desired
            actions.push(PackageAction::Bootstrap {
                manager: manager.name().to_string(),
                method: bootstrap_method(*manager).to_string(),
                origin: "local".to_string(),
            });
            actions.push(PackageAction::Install {
                manager: manager.name().to_string(),
                packages: desired,
                origin: "local".to_string(),
            });
        } else if manager
            .name()
            .split('-')
            .next()
            .is_some_and(|prefix| bootstrapping.contains(prefix))
        {
            // Sub-manager whose parent is being bootstrapped (e.g. brew-tap when brew
            // is being bootstrapped). Install all desired — nothing is installed yet.
            actions.push(PackageAction::Install {
                manager: manager.name().to_string(),
                packages: desired,
                origin: "local".to_string(),
            });
        } else {
            actions.push(PackageAction::Skip {
                manager: manager.name().to_string(),
                reason: format!(
                    "'{}' not available — cannot auto-install on this platform",
                    manager.name()
                ),
                origin: "local".to_string(),
            });
        }
    }

    Ok(actions)
}

/// Apply package actions.
#[cfg(test)]
pub fn apply_packages(
    actions: &[PackageAction],
    managers: &[&dyn PackageManager],
    printer: &Printer,
) -> Result<()> {
    for action in actions {
        match action {
            PackageAction::Bootstrap {
                manager: mgr_name, ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.bootstrap(printer)?;
                }
            }
            PackageAction::Install {
                manager: mgr_name,
                packages,
                ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.install(packages, printer)?;
                }
            }
            PackageAction::Uninstall {
                manager: mgr_name,
                packages,
                ..
            } => {
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.uninstall(packages, printer)?;
                }
            }
            PackageAction::Skip {
                manager, reason, ..
            } => {
                printer.warning(&format!("{}: {}", manager, reason));
            }
        }
    }

    Ok(())
}

/// Format package actions as human-readable plan items.
#[cfg(test)]
pub fn format_package_actions(actions: &[PackageAction]) -> Vec<String> {
    actions
        .iter()
        .map(|a| match a {
            PackageAction::Bootstrap {
                manager, method, ..
            } => format!("bootstrap {} via {}", manager, method),
            PackageAction::Install {
                manager, packages, ..
            } => format!("install via {}: {}", manager, packages.join(", ")),
            PackageAction::Uninstall {
                manager, packages, ..
            } => format!("uninstall via {}: {}", manager, packages.join(", ")),
            PackageAction::Skip {
                manager, reason, ..
            } => format!("skip {}: {}", manager, reason),
        })
        .collect()
}

/// Add a package to the profile's package spec.
pub fn add_package(
    manager_name: &str,
    package_name: &str,
    packages: &mut PackagesSpec,
) -> Result<()> {
    match manager_name {
        "brew" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.formulae.contains(&package_name.to_string()) {
                brew.formulae.push(package_name.to_string());
            }
        }
        "brew-tap" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.taps.contains(&package_name.to_string()) {
                brew.taps.push(package_name.to_string());
            }
        }
        "brew-cask" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.casks.contains(&package_name.to_string()) {
                brew.casks.push(package_name.to_string());
            }
        }
        "apt" => {
            let apt = packages.apt.get_or_insert_with(Default::default);
            if !apt.packages.contains(&package_name.to_string()) {
                apt.packages.push(package_name.to_string());
            }
        }
        "cargo" => {
            let cargo = packages.cargo.get_or_insert_with(Default::default);
            if !cargo.packages.contains(&package_name.to_string()) {
                cargo.packages.push(package_name.to_string());
            }
        }
        "npm" => {
            let npm = packages.npm.get_or_insert_with(Default::default);
            if !npm.global.contains(&package_name.to_string()) {
                npm.global.push(package_name.to_string());
            }
        }
        "pipx" => {
            if !packages.pipx.contains(&package_name.to_string()) {
                packages.pipx.push(package_name.to_string());
            }
        }
        "dnf" => {
            if !packages.dnf.contains(&package_name.to_string()) {
                packages.dnf.push(package_name.to_string());
            }
        }
        "apk" => {
            if !packages.apk.contains(&package_name.to_string()) {
                packages.apk.push(package_name.to_string());
            }
        }
        "pacman" => {
            if !packages.pacman.contains(&package_name.to_string()) {
                packages.pacman.push(package_name.to_string());
            }
        }
        "zypper" => {
            if !packages.zypper.contains(&package_name.to_string()) {
                packages.zypper.push(package_name.to_string());
            }
        }
        "yum" => {
            if !packages.yum.contains(&package_name.to_string()) {
                packages.yum.push(package_name.to_string());
            }
        }
        "pkg" => {
            if !packages.pkg.contains(&package_name.to_string()) {
                packages.pkg.push(package_name.to_string());
            }
        }
        "snap" => {
            let snap = packages.snap.get_or_insert_with(Default::default);
            if !snap.packages.contains(&package_name.to_string()) {
                snap.packages.push(package_name.to_string());
            }
        }
        "flatpak" => {
            let flatpak = packages.flatpak.get_or_insert_with(Default::default);
            if !flatpak.packages.contains(&package_name.to_string()) {
                flatpak.packages.push(package_name.to_string());
            }
        }
        "nix" => {
            if !packages.nix.contains(&package_name.to_string()) {
                packages.nix.push(package_name.to_string());
            }
        }
        "go" => {
            if !packages.go.contains(&package_name.to_string()) {
                packages.go.push(package_name.to_string());
            }
        }
        _ => {
            // Check custom managers
            if let Some(custom) = packages.custom.iter_mut().find(|c| c.name == manager_name) {
                if !custom.packages.contains(&package_name.to_string()) {
                    custom.packages.push(package_name.to_string());
                }
            } else {
                return Err(PackageError::ManagerNotAvailable {
                    manager: manager_name.to_string(),
                }
                .into());
            }
        }
    }
    Ok(())
}

/// Remove a package from the profile's package spec.
pub fn remove_package(
    manager_name: &str,
    package_name: &str,
    packages: &mut PackagesSpec,
) -> Result<bool> {
    let removed = match manager_name {
        "brew" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.formulae.len();
                brew.formulae.retain(|p| p != package_name);
                brew.formulae.len() < before
            } else {
                false
            }
        }
        "brew-tap" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.taps.len();
                brew.taps.retain(|p| p != package_name);
                brew.taps.len() < before
            } else {
                false
            }
        }
        "brew-cask" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.casks.len();
                brew.casks.retain(|p| p != package_name);
                brew.casks.len() < before
            } else {
                false
            }
        }
        "apt" => {
            if let Some(ref mut apt) = packages.apt {
                let before = apt.packages.len();
                apt.packages.retain(|p| p != package_name);
                apt.packages.len() < before
            } else {
                false
            }
        }
        "cargo" => {
            if let Some(ref mut cargo) = packages.cargo {
                let before = cargo.packages.len();
                cargo.packages.retain(|p| p != package_name);
                cargo.packages.len() < before
            } else {
                false
            }
        }
        "npm" => {
            if let Some(ref mut npm) = packages.npm {
                let before = npm.global.len();
                npm.global.retain(|p| p != package_name);
                npm.global.len() < before
            } else {
                false
            }
        }
        "pipx" => {
            let before = packages.pipx.len();
            packages.pipx.retain(|p| p != package_name);
            packages.pipx.len() < before
        }
        "dnf" => {
            let before = packages.dnf.len();
            packages.dnf.retain(|p| p != package_name);
            packages.dnf.len() < before
        }
        "apk" => {
            let before = packages.apk.len();
            packages.apk.retain(|p| p != package_name);
            packages.apk.len() < before
        }
        "pacman" => {
            let before = packages.pacman.len();
            packages.pacman.retain(|p| p != package_name);
            packages.pacman.len() < before
        }
        "zypper" => {
            let before = packages.zypper.len();
            packages.zypper.retain(|p| p != package_name);
            packages.zypper.len() < before
        }
        "yum" => {
            let before = packages.yum.len();
            packages.yum.retain(|p| p != package_name);
            packages.yum.len() < before
        }
        "pkg" => {
            let before = packages.pkg.len();
            packages.pkg.retain(|p| p != package_name);
            packages.pkg.len() < before
        }
        "snap" => {
            if let Some(ref mut snap) = packages.snap {
                let before = snap.packages.len() + snap.classic.len();
                snap.packages.retain(|p| p != package_name);
                snap.classic.retain(|p| p != package_name);
                (snap.packages.len() + snap.classic.len()) < before
            } else {
                false
            }
        }
        "flatpak" => {
            if let Some(ref mut flatpak) = packages.flatpak {
                let before = flatpak.packages.len();
                flatpak.packages.retain(|p| p != package_name);
                flatpak.packages.len() < before
            } else {
                false
            }
        }
        "nix" => {
            let before = packages.nix.len();
            packages.nix.retain(|p| p != package_name);
            packages.nix.len() < before
        }
        "go" => {
            let before = packages.go.len();
            packages.go.retain(|p| p != package_name);
            packages.go.len() < before
        }
        _ => {
            // Check custom managers
            if let Some(custom) = packages.custom.iter_mut().find(|c| c.name == manager_name) {
                let before = custom.packages.len();
                custom.packages.retain(|p| p != package_name);
                custom.packages.len() < before
            } else {
                return Err(PackageError::ManagerNotAvailable {
                    manager: manager_name.to_string(),
                }
                .into());
            }
        }
    };
    Ok(removed)
}

/// Build the default provider registry with all workstation package managers.
pub fn all_package_managers() -> Vec<Box<dyn PackageManager>> {
    vec![
        Box::new(BrewManager),
        Box::new(BrewTapManager),
        Box::new(BrewCaskManager),
        Box::new(apt_manager()),
        Box::new(CargoManager),
        Box::new(NpmManager),
        Box::new(PipxManager),
        Box::new(dnf_manager()),
        Box::new(apk_manager()),
        Box::new(pacman_manager()),
        Box::new(zypper_manager()),
        Box::new(yum_manager()),
        Box::new(pkg_manager()),
        Box::new(SnapManager),
        Box::new(FlatpakManager),
        Box::new(NixManager),
        Box::new(GoInstallManager),
    ]
}

// --- Native manifest support ---

/// Parse a Brewfile and extract taps, formulae, and casks.
/// Brewfile format: lines like `tap "name"`, `brew "name"`, `cask "name"`.
/// Comments (#) and blank lines are ignored.
fn parse_brewfile(path: &Path) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "brew".into(),
        message: format!("failed to read Brewfile {}: {}", path.display(), e),
    })?;

    let mut taps = Vec::new();
    let mut formulae = Vec::new();
    let mut casks = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Extract the quoted name from lines like: brew "ripgrep", tap "homebrew/cask"
        // Also handle comma-separated options after the name
        if let Some(name) = extract_brewfile_name(line) {
            if line.starts_with("tap ") {
                taps.push(name);
            } else if line.starts_with("brew ") {
                formulae.push(name);
            } else if line.starts_with("cask ") {
                casks.push(name);
            }
            // Ignore mas, vscode, whalebrew, etc.
        }
    }

    Ok((taps, formulae, casks))
}

/// Extract the package name from a Brewfile line.
/// Handles: `brew "name"`, `brew "name", args: ...`, `brew 'name'`
fn extract_brewfile_name(line: &str) -> Option<String> {
    // Find the first quoted string after the keyword
    let after_keyword = line.split_once(' ')?.1.trim();
    if let Some(rest) = after_keyword.strip_prefix('"') {
        rest.split('"').next().map(|s| s.to_string())
    } else if let Some(rest) = after_keyword.strip_prefix('\'') {
        rest.split('\'').next().map(|s| s.to_string())
    } else {
        // Unquoted: take until comma or end of line
        Some(
            after_keyword
                .split(',')
                .next()
                .unwrap_or(after_keyword)
                .trim()
                .to_string(),
        )
    }
}

/// Parse an apt package list file (one package per line).
/// Comments (#) and blank lines are ignored.
fn parse_apt_manifest(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "apt".into(),
        message: format!("failed to read apt manifest {}: {}", path.display(), e),
    })?;

    Ok(content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect())
}

/// Parse a package.json and extract dependency names.
/// Reads `dependencies` and `devDependencies` keys.
fn parse_npm_package_json(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "npm".into(),
        message: format!("failed to read package.json {}: {}", path.display(), e),
    })?;

    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| PackageError::ListFailed {
            manager: "npm".into(),
            message: format!("failed to parse package.json {}: {}", path.display(), e),
        })?;

    let mut packages = Vec::new();

    for section in ["dependencies", "devDependencies"] {
        if let Some(deps) = json.get(section).and_then(|v| v.as_object()) {
            for key in deps.keys() {
                if !packages.contains(key) {
                    packages.push(key.clone());
                }
            }
        }
    }

    Ok(packages)
}

/// Parse a Cargo.toml and extract dependency names.
/// Reads the `[dependencies]` table keys.
fn parse_cargo_toml(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| PackageError::ListFailed {
        manager: "cargo".into(),
        message: format!("failed to read Cargo.toml {}: {}", path.display(), e),
    })?;

    let toml_val: toml::Value = toml::from_str(&content).map_err(|e| PackageError::ListFailed {
        manager: "cargo".into(),
        message: format!("failed to parse Cargo.toml {}: {}", path.display(), e),
    })?;

    let mut packages = Vec::new();

    if let Some(deps) = toml_val.get("dependencies").and_then(|v| v.as_table()) {
        for key in deps.keys() {
            packages.push(key.clone());
        }
    }

    Ok(packages)
}

/// Resolve manifest files referenced in package specs and merge their contents
/// into the inline package lists. Paths are relative to `config_dir`.
pub fn resolve_manifest_packages(packages: &mut PackagesSpec, config_dir: &Path) -> Result<()> {
    // Brew: parse Brewfile, merge taps/formulae/casks
    if let Some(ref mut brew) = packages.brew
        && let Some(ref file) = brew.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let (taps, formulae, casks) = parse_brewfile(&path)?;
            cfgd_core::union_extend(&mut brew.taps, &taps);
            cfgd_core::union_extend(&mut brew.formulae, &formulae);
            cfgd_core::union_extend(&mut brew.casks, &casks);
        }
    }

    // Apt: parse one-per-line file
    if let Some(ref mut apt) = packages.apt
        && let Some(ref file) = apt.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = parse_apt_manifest(&path)?;
            cfgd_core::union_extend(&mut apt.packages, &pkgs);
        }
    }

    // Npm: parse package.json
    if let Some(ref mut npm) = packages.npm
        && let Some(ref file) = npm.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = parse_npm_package_json(&path)?;
            cfgd_core::union_extend(&mut npm.global, &pkgs);
        }
    }

    // Cargo: parse Cargo.toml
    if let Some(ref mut cargo) = packages.cargo
        && let Some(ref file) = cargo.file
    {
        let path = config_dir.join(file);
        if path.exists() {
            let pkgs = parse_cargo_toml(&path)?;
            cfgd_core::union_extend(&mut cargo.packages, &pkgs);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct MockPackageManager {
        mgr_name: &'static str,
        available: bool,
        bootstrappable: bool,
        installed: HashSet<String>,
        installs: Mutex<Vec<Vec<String>>>,
        uninstalls: Mutex<Vec<Vec<String>>>,
    }

    impl MockPackageManager {
        fn new(name: &'static str, available: bool, installed: Vec<&str>) -> Self {
            Self {
                mgr_name: name,
                available,
                bootstrappable: false,
                installed: installed.into_iter().map(String::from).collect(),
                installs: Mutex::new(Vec::new()),
                uninstalls: Mutex::new(Vec::new()),
            }
        }

        fn with_bootstrap(mut self) -> Self {
            self.bootstrappable = true;
            self
        }
    }

    impl PackageManager for MockPackageManager {
        fn name(&self) -> &str {
            self.mgr_name
        }

        fn is_available(&self) -> bool {
            self.available
        }

        fn can_bootstrap(&self) -> bool {
            self.bootstrappable
        }

        fn bootstrap(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }

        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(self.installed.clone())
        }

        fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            self.installs.lock().unwrap().push(packages.to_vec());
            Ok(())
        }

        fn uninstall(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            self.uninstalls.lock().unwrap().push(packages.to_vec());
            Ok(())
        }

        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }

        fn available_version(&self, _package: &str) -> Result<Option<String>> {
            Ok(None)
        }
    }

    fn test_profile(packages: PackagesSpec) -> MergedProfile {
        MergedProfile {
            packages,
            ..Default::default()
        }
    }

    #[test]
    fn plan_installs_missing_packages() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
        let profile = test_profile(PackagesSpec {
            cargo: Some(cfgd_core::config::CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into(), "fd-find".into()],
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PackageAction::Install {
                manager, packages, ..
            } => {
                assert_eq!(manager, "cargo");
                assert!(packages.contains(&"ripgrep".to_string()));
                assert!(packages.contains(&"fd-find".to_string()));
                assert!(!packages.contains(&"bat".to_string()));
            }
            _ => panic!("expected Install action"),
        }
    }

    #[test]
    fn plan_skips_unavailable_manager() {
        let mock = MockPackageManager::new("brew", false, vec![]);
        let profile = test_profile(PackagesSpec {
            brew: Some(cfgd_core::config::BrewSpec {
                formulae: vec!["ripgrep".into()],
                ..Default::default()
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], PackageAction::Skip { .. }));
    }

    #[test]
    fn plan_empty_when_all_installed() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat", "ripgrep"]);
        let profile = test_profile(PackagesSpec {
            cargo: Some(cfgd_core::config::CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into()],
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert!(actions.is_empty());
    }

    #[test]
    fn plan_skips_manager_with_no_desired_packages() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
        let profile = test_profile(PackagesSpec::default());

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert!(actions.is_empty());
    }

    #[test]
    fn format_actions_produces_readable_strings() {
        let actions = vec![
            PackageAction::Bootstrap {
                manager: "cargo".into(),
                method: "rustup".into(),
                origin: "local".into(),
            },
            PackageAction::Install {
                manager: "brew".into(),
                packages: vec!["ripgrep".into(), "fd".into()],
                origin: "local".into(),
            },
            PackageAction::Skip {
                manager: "apt".into(),
                reason: "not available".into(),
                origin: "local".into(),
            },
        ];

        let formatted = format_package_actions(&actions);
        assert_eq!(formatted.len(), 3);
        assert!(formatted[0].contains("bootstrap"));
        assert!(formatted[0].contains("rustup"));
        assert!(formatted[1].contains("brew"));
        assert!(formatted[1].contains("ripgrep"));
        assert!(formatted[2].contains("skip"));
    }

    #[test]
    fn add_package_to_spec() {
        let mut packages = PackagesSpec::default();

        add_package("cargo", "ripgrep", &mut packages).unwrap();
        assert_eq!(packages.cargo.as_ref().unwrap().packages, vec!["ripgrep"]);

        // Adding again is idempotent
        add_package("cargo", "ripgrep", &mut packages).unwrap();
        assert_eq!(packages.cargo.as_ref().unwrap().packages, vec!["ripgrep"]);

        add_package("brew", "fd", &mut packages).unwrap();
        assert_eq!(packages.brew.as_ref().unwrap().formulae, vec!["fd"]);

        add_package("brew-cask", "firefox", &mut packages).unwrap();
        assert_eq!(packages.brew.as_ref().unwrap().casks, vec!["firefox"]);

        add_package("apt", "curl", &mut packages).unwrap();
        assert_eq!(packages.apt.as_ref().unwrap().packages, vec!["curl"]);

        add_package("npm", "typescript", &mut packages).unwrap();
        assert_eq!(packages.npm.as_ref().unwrap().global, vec!["typescript"]);

        add_package("pipx", "black", &mut packages).unwrap();
        assert_eq!(packages.pipx, vec!["black"]);

        add_package("dnf", "gcc", &mut packages).unwrap();
        assert_eq!(packages.dnf, vec!["gcc"]);
    }

    #[test]
    fn add_package_unknown_manager_errors() {
        let mut packages = PackagesSpec::default();
        let result = add_package("unknown", "pkg", &mut packages);
        assert!(result.is_err());
    }

    #[test]
    fn remove_package_from_spec() {
        let mut packages = PackagesSpec {
            cargo: Some(cfgd_core::config::CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into()],
            }),
            ..Default::default()
        };

        let removed = remove_package("cargo", "bat", &mut packages).unwrap();
        assert!(removed);
        assert_eq!(packages.cargo.as_ref().unwrap().packages, vec!["ripgrep"]);

        let removed = remove_package("cargo", "nonexistent", &mut packages).unwrap();
        assert!(!removed);
    }

    #[test]
    fn remove_package_unknown_manager_errors() {
        let mut packages = PackagesSpec::default();
        let result = remove_package("unknown", "pkg", &mut packages);
        assert!(result.is_err());
    }

    #[test]
    fn apply_calls_install_on_correct_manager() {
        let mock = MockPackageManager::new("cargo", true, vec![]);
        let actions = vec![PackageAction::Install {
            manager: "cargo".into(),
            packages: vec!["ripgrep".into()],
            origin: "local".into(),
        }];

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        apply_packages(&actions, &managers, &printer).unwrap();

        let installs = mock.installs.lock().unwrap();
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0], vec!["ripgrep"]);
    }

    #[test]
    fn apply_calls_uninstall_on_correct_manager() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
        let actions = vec![PackageAction::Uninstall {
            manager: "cargo".into(),
            packages: vec!["bat".into()],
            origin: "local".into(),
        }];

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        apply_packages(&actions, &managers, &printer).unwrap();

        let uninstalls = mock.uninstalls.lock().unwrap();
        assert_eq!(uninstalls.len(), 1);
        assert_eq!(uninstalls[0], vec!["bat"]);
    }

    #[test]
    fn all_package_managers_creates_all() {
        let managers = all_package_managers();
        assert_eq!(managers.len(), 17);

        let names: Vec<&str> = managers.iter().map(|m| m.name()).collect();
        assert!(names.contains(&"brew"));
        assert!(names.contains(&"brew-tap"));
        assert!(names.contains(&"brew-cask"));
        assert!(names.contains(&"apt"));
        assert!(names.contains(&"cargo"));
        assert!(names.contains(&"npm"));
        assert!(names.contains(&"pipx"));
        assert!(names.contains(&"dnf"));
        assert!(names.contains(&"apk"));
        assert!(names.contains(&"pacman"));
        assert!(names.contains(&"zypper"));
        assert!(names.contains(&"yum"));
        assert!(names.contains(&"pkg"));
        assert!(names.contains(&"snap"));
        assert!(names.contains(&"flatpak"));
        assert!(names.contains(&"nix"));
        assert!(names.contains(&"go"));
    }

    #[test]
    fn plan_multiple_managers() {
        let cargo_mock = MockPackageManager::new("cargo", true, vec![]);
        let npm_mock = MockPackageManager::new("npm", true, vec!["typescript"]);

        let profile = test_profile(PackagesSpec {
            cargo: Some(cfgd_core::config::CargoSpec {
                file: None,
                packages: vec!["ripgrep".into()],
            }),
            npm: Some(cfgd_core::config::NpmSpec {
                file: None,
                global: vec!["typescript".into(), "eslint".into()],
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&cargo_mock, &npm_mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        // cargo needs ripgrep, npm needs eslint (typescript already installed)
        assert_eq!(actions.len(), 2);

        let cargo_action = actions.iter().find(|a| match a {
            PackageAction::Install { manager, .. } => manager == "cargo",
            _ => false,
        });
        assert!(cargo_action.is_some());

        let npm_action = actions.iter().find(|a| match a {
            PackageAction::Install { manager, .. } => manager == "npm",
            _ => false,
        });
        assert!(npm_action.is_some());
        if let Some(PackageAction::Install { packages, .. }) = npm_action {
            assert_eq!(packages, &vec!["eslint".to_string()]);
        }
    }

    #[test]
    fn plan_bootstrap_unavailable_bootstrappable_manager() {
        let mock = MockPackageManager::new("cargo", false, vec![]).with_bootstrap();
        let profile = test_profile(PackagesSpec {
            cargo: Some(cfgd_core::config::CargoSpec {
                file: None,
                packages: vec!["ripgrep".into(), "fd-find".into()],
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert_eq!(actions.len(), 2);
        assert!(
            matches!(&actions[0], PackageAction::Bootstrap { manager, .. } if manager == "cargo")
        );
        assert!(
            matches!(&actions[1], PackageAction::Install { manager, packages, .. } if manager == "cargo" && packages.len() == 2)
        );
    }

    #[test]
    fn plan_skip_unavailable_non_bootstrappable_manager() {
        let mock = MockPackageManager::new("apt", false, vec![]);
        let profile = test_profile(PackagesSpec {
            apt: Some(cfgd_core::config::AptSpec {
                file: None,
                packages: vec!["curl".into()],
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PackageAction::Skip {
                manager, reason, ..
            } => {
                assert_eq!(manager, "apt");
                assert!(reason.contains("cannot auto-install"));
            }
            _ => panic!("expected Skip action"),
        }
    }

    #[test]
    fn plan_sub_manager_installs_when_parent_bootstrapping() {
        // brew is unavailable + bootstrappable, brew-tap should get Install (not Skip)
        let brew_mock = MockPackageManager::new("brew", false, vec![]).with_bootstrap();
        let tap_mock = MockPackageManager::new("brew-tap", false, vec![]);

        let profile = test_profile(PackagesSpec {
            brew: Some(cfgd_core::config::BrewSpec {
                formulae: vec!["ripgrep".into()],
                taps: vec!["some/tap".into()],
                ..Default::default()
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&brew_mock, &tap_mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        // Should have: Bootstrap(brew), Install(brew: ripgrep), Install(brew-tap: some/tap)
        assert_eq!(actions.len(), 3);
        assert!(
            matches!(&actions[0], PackageAction::Bootstrap { manager, .. } if manager == "brew")
        );
        assert!(matches!(&actions[1], PackageAction::Install { manager, .. } if manager == "brew"));
        assert!(
            matches!(&actions[2], PackageAction::Install { manager, .. } if manager == "brew-tap")
        );
    }

    // --- Manifest parsing tests ---

    #[test]
    fn parse_brewfile_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Brewfile");
        std::fs::write(
            &path,
            r#"# My Brewfile
tap "homebrew/cask"
tap "homebrew/core"

brew "ripgrep"
brew "fd"
brew "bat", restart_service: :changed

cask "firefox"
cask "visual-studio-code"

# macOS app store (ignored)
mas "Xcode", id: 497799835
"#,
        )
        .unwrap();

        let (taps, formulae, casks) = parse_brewfile(&path).unwrap();
        assert_eq!(taps, vec!["homebrew/cask", "homebrew/core"]);
        assert_eq!(formulae, vec!["ripgrep", "fd", "bat"]);
        assert_eq!(casks, vec!["firefox", "visual-studio-code"]);
    }

    #[test]
    fn parse_brewfile_single_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Brewfile");
        std::fs::write(&path, "brew 'ripgrep'\ncask 'firefox'\n").unwrap();

        let (_, formulae, casks) = parse_brewfile(&path).unwrap();
        assert_eq!(formulae, vec!["ripgrep"]);
        assert_eq!(casks, vec!["firefox"]);
    }

    #[test]
    fn parse_apt_manifest_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("packages.txt");
        std::fs::write(
            &path,
            "# System packages\ncurl\nwget\n\ngit\n# Dev tools\nbuild-essential\n",
        )
        .unwrap();

        let pkgs = parse_apt_manifest(&path).unwrap();
        assert_eq!(pkgs, vec!["curl", "wget", "git", "build-essential"]);
    }

    #[test]
    fn parse_npm_package_json_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("package.json");
        std::fs::write(
            &path,
            r#"{
  "name": "my-project",
  "dependencies": {
    "express": "^4.18.0",
    "lodash": "^4.17.0"
  },
  "devDependencies": {
    "typescript": "^5.0.0",
    "eslint": "^8.0.0"
  }
}"#,
        )
        .unwrap();

        let pkgs = parse_npm_package_json(&path).unwrap();
        assert_eq!(pkgs.len(), 4);
        assert!(pkgs.contains(&"express".to_string()));
        assert!(pkgs.contains(&"lodash".to_string()));
        assert!(pkgs.contains(&"typescript".to_string()));
        assert!(pkgs.contains(&"eslint".to_string()));
    }

    #[test]
    fn parse_npm_package_json_no_deps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("package.json");
        std::fs::write(&path, r#"{"name": "empty"}"#).unwrap();

        let pkgs = parse_npm_package_json(&path).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_cargo_toml_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            r#"[package]
name = "my-project"
version = "0.1.0"

[dependencies]
serde = "1.0"
tokio = { version = "1", features = ["full"] }
clap = "4"
"#,
        )
        .unwrap();

        let pkgs = parse_cargo_toml(&path).unwrap();
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.contains(&"serde".to_string()));
        assert!(pkgs.contains(&"tokio".to_string()));
        assert!(pkgs.contains(&"clap".to_string()));
    }

    #[test]
    fn parse_cargo_toml_no_deps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            r#"[package]
name = "no-deps"
version = "0.1.0"
"#,
        )
        .unwrap();

        let pkgs = parse_cargo_toml(&path).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn resolve_manifest_packages_merges_with_inline() {
        let dir = tempfile::tempdir().unwrap();

        // Create a Brewfile
        std::fs::write(
            dir.path().join("Brewfile"),
            "tap \"homebrew/cask\"\nbrew \"ripgrep\"\ncask \"firefox\"\n",
        )
        .unwrap();

        // Create an apt manifest
        std::fs::write(dir.path().join("packages.txt"), "curl\nwget\n").unwrap();

        // Create a Cargo.toml
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();

        // Create a package.json
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"express": "^4"}}"#,
        )
        .unwrap();

        let mut packages = PackagesSpec {
            brew: Some(cfgd_core::config::BrewSpec {
                file: Some("Brewfile".into()),
                formulae: vec!["fd".into()],
                ..Default::default()
            }),
            apt: Some(cfgd_core::config::AptSpec {
                file: Some("packages.txt".into()),
                packages: vec!["git".into()],
            }),
            cargo: Some(cfgd_core::config::CargoSpec {
                file: Some("Cargo.toml".into()),
                packages: vec!["bat".into()],
            }),
            npm: Some(cfgd_core::config::NpmSpec {
                file: Some("package.json".into()),
                global: vec!["typescript".into()],
            }),
            ..Default::default()
        };

        resolve_manifest_packages(&mut packages, dir.path()).unwrap();

        // Brew: inline + Brewfile merged
        let brew = packages.brew.as_ref().unwrap();
        assert!(brew.taps.contains(&"homebrew/cask".to_string()));
        assert!(brew.formulae.contains(&"fd".to_string())); // inline
        assert!(brew.formulae.contains(&"ripgrep".to_string())); // from Brewfile
        assert!(brew.casks.contains(&"firefox".to_string())); // from Brewfile

        // Apt: inline + file merged
        let apt = packages.apt.as_ref().unwrap();
        assert!(apt.packages.contains(&"git".to_string())); // inline
        assert!(apt.packages.contains(&"curl".to_string())); // from file
        assert!(apt.packages.contains(&"wget".to_string())); // from file

        // Cargo: inline + Cargo.toml merged
        let cargo = packages.cargo.as_ref().unwrap();
        assert!(cargo.packages.contains(&"bat".to_string())); // inline
        assert!(cargo.packages.contains(&"serde".to_string())); // from Cargo.toml

        // Npm: inline + package.json merged
        let npm = packages.npm.as_ref().unwrap();
        assert!(npm.global.contains(&"typescript".to_string())); // inline
        assert!(npm.global.contains(&"express".to_string())); // from package.json
    }

    #[test]
    fn resolve_manifest_missing_file_skipped() {
        let dir = tempfile::tempdir().unwrap();

        let mut packages = PackagesSpec {
            brew: Some(cfgd_core::config::BrewSpec {
                file: Some("nonexistent-Brewfile".into()),
                formulae: vec!["fd".into()],
                ..Default::default()
            }),
            ..Default::default()
        };

        // Missing file should be silently skipped
        resolve_manifest_packages(&mut packages, dir.path()).unwrap();

        let brew = packages.brew.as_ref().unwrap();
        assert_eq!(brew.formulae, vec!["fd"]); // only inline
    }

    #[test]
    fn resolve_manifest_no_file_field_noop() {
        let dir = tempfile::tempdir().unwrap();

        let mut packages = PackagesSpec {
            brew: Some(cfgd_core::config::BrewSpec {
                file: None,
                formulae: vec!["fd".into()],
                ..Default::default()
            }),
            ..Default::default()
        };

        resolve_manifest_packages(&mut packages, dir.path()).unwrap();

        let brew = packages.brew.as_ref().unwrap();
        assert_eq!(brew.formulae, vec!["fd"]);
    }

    #[test]
    fn extract_brewfile_name_handles_variants() {
        assert_eq!(
            extract_brewfile_name(r#"brew "ripgrep""#),
            Some("ripgrep".to_string())
        );
        assert_eq!(
            extract_brewfile_name(r#"brew "bat", restart_service: :changed"#),
            Some("bat".to_string())
        );
        assert_eq!(
            extract_brewfile_name(r#"tap 'homebrew/cask'"#),
            Some("homebrew/cask".to_string())
        );
        assert_eq!(
            extract_brewfile_name(r#"cask "firefox""#),
            Some("firefox".to_string())
        );
    }

    #[test]
    fn add_and_remove_new_managers() {
        let mut packages = PackagesSpec::default();

        add_package("apk", "curl", &mut packages).unwrap();
        assert_eq!(packages.apk, vec!["curl"]);

        add_package("pacman", "vim", &mut packages).unwrap();
        assert_eq!(packages.pacman, vec!["vim"]);

        add_package("zypper", "gcc", &mut packages).unwrap();
        assert_eq!(packages.zypper, vec!["gcc"]);

        add_package("yum", "wget", &mut packages).unwrap();
        assert_eq!(packages.yum, vec!["wget"]);

        add_package("pkg", "bash", &mut packages).unwrap();
        assert_eq!(packages.pkg, vec!["bash"]);

        add_package("snap", "nvim", &mut packages).unwrap();
        assert_eq!(packages.snap.as_ref().unwrap().packages, vec!["nvim"]);

        add_package("flatpak", "org.gimp.GIMP", &mut packages).unwrap();
        assert_eq!(
            packages.flatpak.as_ref().unwrap().packages,
            vec!["org.gimp.GIMP"]
        );

        add_package("nix", "ripgrep", &mut packages).unwrap();
        assert_eq!(packages.nix, vec!["ripgrep"]);

        add_package("go", "golang.org/x/tools/gopls", &mut packages).unwrap();
        assert_eq!(packages.go, vec!["golang.org/x/tools/gopls"]);

        // Idempotent
        add_package("apk", "curl", &mut packages).unwrap();
        assert_eq!(packages.apk, vec!["curl"]);

        // Remove
        let removed = remove_package("apk", "curl", &mut packages).unwrap();
        assert!(removed);
        assert!(packages.apk.is_empty());

        let removed = remove_package("pacman", "vim", &mut packages).unwrap();
        assert!(removed);
        assert!(packages.pacman.is_empty());

        let removed = remove_package("snap", "nvim", &mut packages).unwrap();
        assert!(removed);

        let removed = remove_package("flatpak", "org.gimp.GIMP", &mut packages).unwrap();
        assert!(removed);

        let removed = remove_package("nix", "ripgrep", &mut packages).unwrap();
        assert!(removed);

        let removed = remove_package("go", "golang.org/x/tools/gopls", &mut packages).unwrap();
        assert!(removed);
    }

    #[test]
    fn plan_with_new_managers() {
        let apk = MockPackageManager::new("apk", true, vec!["curl"]);
        let pacman = MockPackageManager::new("pacman", true, vec![]);
        let snap = MockPackageManager::new("snap", false, vec![]).with_bootstrap();

        let profile = test_profile(PackagesSpec {
            apk: vec!["curl".into(), "git".into()],
            pacman: vec!["neovim".into()],
            snap: Some(cfgd_core::config::SnapSpec {
                packages: vec!["nvim".into()],
                classic: vec![],
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&apk, &pacman, &snap];
        let actions = plan_packages(&profile, &managers).unwrap();

        // apk: git is missing → Install
        assert!(actions.iter().any(|a| matches!(
            a,
            PackageAction::Install {
                manager,
                packages,
                ..
            } if manager == "apk" && packages.contains(&"git".to_string())
        )));

        // pacman: neovim missing → Install
        assert!(actions.iter().any(|a| matches!(
            a,
            PackageAction::Install {
                manager,
                packages,
                ..
            } if manager == "pacman" && packages.contains(&"neovim".to_string())
        )));

        // snap: unavailable but bootstrappable → Bootstrap + Install
        assert!(
            actions.iter().any(
                |a| matches!(a, PackageAction::Bootstrap { manager, .. } if manager == "snap")
            )
        );
    }

    #[test]
    fn desired_packages_for_new_managers() {
        let profile = test_profile(PackagesSpec {
            apk: vec!["curl".into()],
            pacman: vec!["vim".into()],
            zypper: vec!["gcc".into()],
            yum: vec!["wget".into()],
            pkg: vec!["bash".into()],
            snap: Some(cfgd_core::config::SnapSpec {
                packages: vec!["nvim".into()],
                classic: vec!["code".into()],
            }),
            flatpak: Some(cfgd_core::config::FlatpakSpec {
                packages: vec!["org.gimp.GIMP".into()],
                remote: None,
            }),
            nix: vec!["ripgrep".into()],
            go: vec!["golang.org/x/tools/gopls".into()],
            ..Default::default()
        });

        assert_eq!(
            cfgd_core::config::desired_packages_for("apk", &profile),
            vec!["curl"]
        );
        assert_eq!(
            cfgd_core::config::desired_packages_for("pacman", &profile),
            vec!["vim"]
        );
        assert_eq!(
            cfgd_core::config::desired_packages_for("zypper", &profile),
            vec!["gcc"]
        );
        assert_eq!(
            cfgd_core::config::desired_packages_for("yum", &profile),
            vec!["wget"]
        );
        assert_eq!(
            cfgd_core::config::desired_packages_for("pkg", &profile),
            vec!["bash"]
        );
        // snap merges packages + classic
        let snap_desired = cfgd_core::config::desired_packages_for("snap", &profile);
        assert!(snap_desired.contains(&"nvim".to_string()));
        assert!(snap_desired.contains(&"code".to_string()));

        assert_eq!(
            cfgd_core::config::desired_packages_for("flatpak", &profile),
            vec!["org.gimp.GIMP"]
        );
        assert_eq!(
            cfgd_core::config::desired_packages_for("nix", &profile),
            vec!["ripgrep"]
        );
        assert_eq!(
            cfgd_core::config::desired_packages_for("go", &profile),
            vec!["golang.org/x/tools/gopls"]
        );
    }

    #[test]
    fn yum_skipped_when_dnf_available() {
        // yum_manager().is_available() returns false when dnf is present
        // We can't directly test this without the actual system, but we can verify
        // the manager's name is correct
        let yum = yum_manager();
        assert_eq!(yum.name(), "yum");
        assert!(!yum.can_bootstrap());
    }

    #[test]
    fn scripted_manager_from_spec() {
        let spec = cfgd_core::config::CustomManagerSpec {
            name: "mypm".to_string(),
            check: "which mypm".to_string(),
            list_installed: "mypm list".to_string(),
            install: "mypm install {package}".to_string(),
            uninstall: "mypm remove {packages}".to_string(),
            update: Some("mypm update".to_string()),
            packages: vec!["foo".to_string(), "bar".to_string()],
        };
        let mgr = ScriptedManager::from_spec(&spec);
        assert_eq!(mgr.name(), "mypm");
        assert!(!mgr.can_bootstrap());
    }

    #[test]
    fn scripted_manager_install_uses_sh() {
        // ScriptedManager with a command that always succeeds
        let spec = cfgd_core::config::CustomManagerSpec {
            name: "testpm".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo installing {package}".to_string(),
            uninstall: "echo removing {package}".to_string(),
            update: None,
            packages: vec![],
        };
        let mgr = ScriptedManager::from_spec(&spec);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.install(&["pkg1".to_string()], &printer).unwrap();
    }

    #[test]
    fn scripted_manager_batch_mode() {
        let spec = cfgd_core::config::CustomManagerSpec {
            name: "batch".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo {packages}".to_string(),
            uninstall: "echo {packages}".to_string(),
            update: None,
            packages: vec![],
        };
        let mgr = ScriptedManager::from_spec(&spec);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.install(
            &["a".to_string(), "b".to_string(), "c".to_string()],
            &printer,
        )
        .unwrap();
    }

    #[test]
    fn scripted_manager_is_available() {
        let spec = cfgd_core::config::CustomManagerSpec {
            name: "avail".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        };
        let mgr = ScriptedManager::from_spec(&spec);
        assert!(mgr.is_available());

        let spec_unavail = cfgd_core::config::CustomManagerSpec {
            name: "unavail".to_string(),
            check: "false".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        };
        let mgr2 = ScriptedManager::from_spec(&spec_unavail);
        assert!(!mgr2.is_available());
    }

    #[test]
    fn scripted_manager_installed_packages() {
        let spec = cfgd_core::config::CustomManagerSpec {
            name: "listtest".to_string(),
            check: "true".to_string(),
            list_installed: "printf 'alpha\\nbeta\\ngamma\\n'".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        };
        let mgr = ScriptedManager::from_spec(&spec);
        let installed = mgr.installed_packages().unwrap();
        assert_eq!(installed.len(), 3);
        assert!(installed.contains("alpha"));
        assert!(installed.contains("beta"));
        assert!(installed.contains("gamma"));
    }

    #[test]
    fn scripted_manager_install_failure() {
        let spec = cfgd_core::config::CustomManagerSpec {
            name: "failpm".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "exit 1".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        };
        let mgr = ScriptedManager::from_spec(&spec);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = mgr.install(&["pkg".to_string()], &printer);
        assert!(result.is_err());
    }

    #[test]
    fn custom_managers_creates_from_specs() {
        let specs = vec![
            cfgd_core::config::CustomManagerSpec {
                name: "pm1".to_string(),
                check: "true".to_string(),
                list_installed: "echo".to_string(),
                install: "echo".to_string(),
                uninstall: "echo".to_string(),
                update: None,
                packages: vec![],
            },
            cfgd_core::config::CustomManagerSpec {
                name: "pm2".to_string(),
                check: "true".to_string(),
                list_installed: "echo".to_string(),
                install: "echo".to_string(),
                uninstall: "echo".to_string(),
                update: None,
                packages: vec![],
            },
        ];
        let managers = custom_managers(&specs);
        assert_eq!(managers.len(), 2);
        assert_eq!(managers[0].name(), "pm1");
        assert_eq!(managers[1].name(), "pm2");
    }

    #[test]
    fn scripted_manager_update_noop_when_no_cmd() {
        let spec = cfgd_core::config::CustomManagerSpec {
            name: "noup".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        };
        let mgr = ScriptedManager::from_spec(&spec);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.update(&printer).unwrap();
    }

    #[test]
    fn custom_manager_config_parsing() {
        let yaml = r#"
custom:
  - name: mise
    check: "command -v mise"
    list-installed: "mise list --installed --json | jq -r 'keys[]'"
    install: "mise install {package}"
    uninstall: "mise uninstall {package}"
    packages:
      - node@20
      - python@3.12
"#;
        let packages: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(packages.custom.len(), 1);
        let cm = &packages.custom[0];
        assert_eq!(cm.name, "mise");
        assert_eq!(cm.install, "mise install {package}");
        assert_eq!(cm.packages, vec!["node@20", "python@3.12"]);
        assert!(cm.update.is_none());
    }

    #[test]
    fn custom_manager_desired_packages() {
        let profile = test_profile(PackagesSpec {
            custom: vec![cfgd_core::config::CustomManagerSpec {
                name: "mypm".to_string(),
                check: "true".to_string(),
                list_installed: "echo".to_string(),
                install: "echo".to_string(),
                uninstall: "echo".to_string(),
                update: None,
                packages: vec!["toolA".to_string(), "toolB".to_string()],
            }],
            ..Default::default()
        });
        let desired = cfgd_core::config::desired_packages_for("mypm", &profile);
        assert_eq!(desired, vec!["toolA".to_string(), "toolB".to_string()]);
    }
}
