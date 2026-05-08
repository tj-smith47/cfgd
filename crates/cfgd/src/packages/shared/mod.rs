//! Shared helpers used across package manager implementations.
//!
//! Process execution wrappers (`run_pkg_cmd*`), sudo helpers, brew detection +
//! invocation, generic system bootstrap routines, post-install caveat extraction,
//! and small string-trimming helpers for package name normalization.

use std::path::PathBuf;
use std::process::{Command, Output};

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::{CommandOutput, Printer};

/// Compute the canonical env-var seam name for a package-manager binary.
/// Pattern: `CFGD_<NAME>_BIN`, with hyphens turned into underscores so
/// `brew-cask` maps to `CFGD_BREW_CASK_BIN`. Used by tests via ToolShim.
pub(super) fn tool_seam_var(name: &str) -> String {
    format!("CFGD_{}_BIN", name.to_uppercase().replace('-', "_"))
}

/// Locate a package-manager binary. First checks the `CFGD_<NAME>_BIN` env-var
/// seam (tests inject a ToolShim path here); then `$PATH` via
/// `command_available`; on miss, walks each entry in `fallbacks` and returns
/// the first that exists. Returns `None` if nothing is found — matches the
/// `find_X() -> Option<PathBuf>` shape that cargo/pipx/go managers had
/// open-coded.
pub(super) fn resolve_tool_with_fallbacks(name: &str, fallbacks: &[PathBuf]) -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(tool_seam_var(name)) {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }
    if command_available(name) {
        return Some(PathBuf::from(name));
    }
    fallbacks.iter().find(|p| p.exists()).cloned()
}

/// Build a `Command` for `name`, using `resolver` for the binary path and
/// falling back to a plain `Command::new(name)` when `resolver` returns `None`.
/// Honors the `CFGD_<NAME>_BIN` env-var seam first, short-circuiting the
/// resolver entirely (tests don't want resolver-side filesystem checks
/// running). Mirrors the `X_cmd()` pattern that cargo/pipx/go had open-coded.
pub(super) fn tool_cmd_with_resolver<F>(name: &str, resolver: F) -> Command
where
    F: FnOnce() -> Option<PathBuf>,
{
    if let Ok(custom) = std::env::var(tool_seam_var(name)) {
        return Command::new(custom);
    }
    Command::new(resolver().unwrap_or_else(|| PathBuf::from(name)))
}

/// Important post-install messages extracted from package manager output.
pub(super) struct PostInstallNote {
    pub(super) manager: String,
    pub(super) message: String,
}

/// Extract caveats/warnings from package manager output.
pub(super) fn extract_caveats(manager: &str, output: &CommandOutput) -> Vec<PostInstallNote> {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let mut notes = Vec::new();

    match manager {
        "brew" | "brew-cask" => {
            // Homebrew prints "==> Caveats" followed by caveat text until next "==> " or end
            let mut in_caveats = false;
            let mut caveat_lines = Vec::new();
            for line in combined.lines() {
                if line.starts_with("==> Caveats") {
                    in_caveats = true;
                    caveat_lines.clear();
                    continue;
                }
                if in_caveats {
                    if line.starts_with("==> ") {
                        if !caveat_lines.is_empty() {
                            notes.push(PostInstallNote {
                                manager: manager.to_string(),
                                message: caveat_lines.join("\n").trim().to_string(),
                            });
                        }
                        in_caveats = false;
                    } else {
                        caveat_lines.push(line.to_string());
                    }
                }
            }
            if in_caveats && !caveat_lines.is_empty() {
                notes.push(PostInstallNote {
                    manager: manager.to_string(),
                    message: caveat_lines.join("\n").trim().to_string(),
                });
            }
        }
        "npm" | "pnpm" => {
            for line in combined.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("npm warn") || trimmed.starts_with("npm WARN") {
                    notes.push(PostInstallNote {
                        manager: manager.to_string(),
                        message: trimmed.to_string(),
                    });
                }
            }
        }
        "pip" | "pipx" => {
            for line in combined.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("WARNING:") {
                    notes.push(PostInstallNote {
                        manager: manager.to_string(),
                        message: trimmed.to_string(),
                    });
                }
            }
        }
        _ => {
            // Generic: capture any line containing warning/caveat/note from stderr
            for line in output.stderr.lines() {
                let trimmed = line.trim();
                let lower = trimmed.to_lowercase();
                if lower.contains("warning:") || lower.contains("caveat") || lower.contains("note:")
                {
                    notes.push(PostInstallNote {
                        manager: manager.to_string(),
                        message: trimmed.to_string(),
                    });
                }
            }
        }
    }
    notes
}

/// Print collected post-install notes to the user.
pub(super) fn print_caveats(printer: &Printer, notes: &[PostInstallNote]) {
    if notes.is_empty() {
        return;
    }
    printer.newline();
    printer.subheader("Post-install notes");
    for note in notes {
        printer.warning(&format!("[{}] {}", note.manager, note.message));
    }
}

/// Run a command, mapping IO errors to PackageError::CommandFailed and non-zero
/// exit to the appropriate PackageError variant based on `error_kind`.
/// `error_kind` should be one of: "install", "uninstall", "list", "update".
/// For "list", returns ListFailed. For "update", returns InstallFailed (matching
/// existing convention). An optional `msg_prefix` is prepended to the error message.
pub(super) fn run_pkg_cmd(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
) -> std::result::Result<Output, PackageError> {
    run_pkg_cmd_prefixed(manager, cmd, error_kind, None)
}

/// Like `run_pkg_cmd` but prepends a custom prefix to the error message.
pub(super) fn run_pkg_cmd_msg(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
    msg_prefix: &str,
) -> std::result::Result<Output, PackageError> {
    run_pkg_cmd_prefixed(manager, cmd, error_kind, Some(msg_prefix))
}

/// Timeout for package manager operations (10 minutes — installs can be slow).
const PKG_CMD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

fn run_pkg_cmd_prefixed(
    manager: &str,
    cmd: &mut Command,
    error_kind: &str,
    msg_prefix: Option<&str>,
) -> std::result::Result<Output, PackageError> {
    // Ensure stdout/stderr are captured for timeout-based execution
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let output = cfgd_core::command_output_with_timeout(cmd, PKG_CMD_TIMEOUT).map_err(|e| {
        PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        }
    })?;
    if !output.status.success() {
        let stderr = cfgd_core::stderr_lossy_trimmed(&output);
        let message = match msg_prefix {
            Some(prefix) if !prefix.is_empty() => format!("{}: {}", prefix, stderr),
            _ => stderr,
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

/// Run a package manager command with live progress display via Printer.
/// Use for long-running operations (install, uninstall, update, bootstrap).
/// Maps spawn errors to PackageError::CommandFailed and non-zero exit to
/// the appropriate variant based on `error_kind`.
pub(super) fn run_pkg_cmd_live(
    printer: &Printer,
    manager: &str,
    cmd: &mut Command,
    label: &str,
    error_kind: &str,
) -> std::result::Result<CommandOutput, PackageError> {
    let output = printer
        .run_with_output(cmd, label)
        .map_err(|e| PackageError::CommandFailed {
            manager: manager.into(),
            source: e,
        })?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        return Err(match error_kind {
            "install" => PackageError::InstallFailed {
                manager: manager.into(),
                message: format!("exit code {}", code),
            },
            "uninstall" => PackageError::UninstallFailed {
                manager: manager.into(),
                message: format!("exit code {}", code),
            },
            _ => PackageError::InstallFailed {
                manager: manager.into(),
                message: format!("exit code {}", code),
            },
        });
    }
    // Extract and print any post-install caveats
    if error_kind == "install" {
        let notes = extract_caveats(manager, &output);
        print_caveats(printer, &notes);
    }
    Ok(output)
}

const LINUXBREW_PATH: &str = "/home/linuxbrew/.linuxbrew/bin/brew";

/// Env-var seam for the `brew` binary path. Production reads no env var.
/// Tests set this to a `cfgd_core::test_helpers::ToolShim` script path,
/// short-circuiting the linuxbrew detection logic so install/uninstall/etc
/// flows can be exercised without a real Homebrew installation.
const BREW_BIN_ENV: &str = "CFGD_BREW_BIN";

/// Check if brew is available, including linuxbrew fallback on Linux.
/// Honors `CFGD_BREW_BIN` for tests.
pub(super) fn brew_available() -> bool {
    if std::env::var(BREW_BIN_ENV).is_ok_and(|v| std::path::Path::new(&v).is_file()) {
        return true;
    }
    if command_available("brew") {
        return true;
    }
    cfg!(target_os = "linux") && std::path::Path::new(LINUXBREW_PATH).exists()
}

/// True when a Linux system package manager (apt, dnf, or zypper) is on PATH.
/// Used by Linux-only managers (snap, flatpak) to decide bootstrappability.
#[cfg(target_os = "linux")]
pub(super) fn linux_system_manager_available() -> bool {
    command_available("apt") || command_available("dnf") || command_available("zypper")
}

/// True when any cross-platform system package manager is available.
/// Covers brew (macOS/Linux), apt/dnf (Linux), and winget/choco/scoop (Windows).
pub(super) fn any_system_manager_available() -> bool {
    brew_available()
        || command_available("apt")
        || command_available("dnf")
        || command_available("winget")
        || command_available("choco")
        || command_available("scoop")
}

/// Return the brew bin/sbin directories for the current platform.
/// Mirrors `BrewManager::path_dirs`; kept here so `path_with_brew` doesn't need
/// to depend on the brew submodule.
pub(super) fn brew_path_dirs() -> Vec<String> {
    if cfg!(target_os = "linux") {
        vec![
            "/home/linuxbrew/.linuxbrew/bin".to_string(),
            "/home/linuxbrew/.linuxbrew/sbin".to_string(),
        ]
    } else if cfg!(target_os = "macos") {
        // Apple Silicon vs Intel
        if std::path::Path::new("/opt/homebrew/bin").exists() {
            vec![
                "/opt/homebrew/bin".to_string(),
                "/opt/homebrew/sbin".to_string(),
            ]
        } else {
            vec!["/usr/local/bin".to_string(), "/usr/local/sbin".to_string()]
        }
    } else {
        Vec::new()
    }
}

/// After brew bootstrap, add brew's bin directories to the current process PATH
/// so that brew-installed binaries (and post-apply scripts that use them) work
/// immediately without requiring a new shell session.
/// Build a PATH string that includes brew's bin directories.
fn path_with_brew() -> Option<String> {
    let dirs = brew_path_dirs();
    if dirs.is_empty() {
        return None;
    }

    if let Ok(current_path) = std::env::var("PATH")
        && !current_path.contains(&dirs[0])
    {
        let prefix = dirs.join(":");
        return Some(format!("{}:{}", prefix, current_path));
    }
    None
}

/// The brew-augmented PATH, cached at first call.
pub(super) fn brew_path() -> Option<&'static str> {
    use std::sync::OnceLock;
    static BREW_PATH: OnceLock<Option<String>> = OnceLock::new();
    BREW_PATH.get_or_init(path_with_brew).as_deref()
}

/// Build a Command for brew, handling linuxbrew paths.
/// On Linux as root, detects the owner of the brew installation and runs via
/// `sudo -u <owner>` since brew refuses to run as root.
/// On Linux as non-root, uses LINUXBREW_PATH directly if brew is not in PATH.
///
/// Honors `CFGD_BREW_BIN` for tests: when set, short-circuits all detection
/// and runs the shim directly. The shim is responsible for any sudo / PATH
/// setup the test cares about.
pub(super) fn brew_cmd() -> Command {
    if let Ok(custom) = std::env::var(BREW_BIN_ENV) {
        return Command::new(custom);
    }
    if cfg!(target_os = "linux") && std::path::Path::new(LINUXBREW_PATH).exists() {
        if cfgd_core::is_root() {
            if let Some(owner) = brew_owner() {
                let mut cmd = Command::new("sudo");
                cmd.args(["-u", &owner, LINUXBREW_PATH]);
                // cwd must be readable by the brew user — /root is 700
                cmd.current_dir("/tmp");
                return cmd;
            }
            let mut cmd = Command::new(LINUXBREW_PATH);
            cmd.current_dir("/tmp");
            return cmd;
        }
        if !command_available("brew") {
            return Command::new(LINUXBREW_PATH);
        }
    }
    let mut cmd = Command::new("brew");
    // Augment PATH for brew lookups without modifying the global environment
    if let Some(augmented_path) = brew_path() {
        cmd.env("PATH", augmented_path);
    }
    cmd
}

/// Detect the user who owns the brew installation.
fn brew_owner() -> Option<String> {
    let output = Command::new("stat")
        .args(["-c", "%U", LINUXBREW_PATH])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let owner = cfgd_core::stdout_lossy_trimmed(&output);
    if owner.is_empty() || owner == "root" {
        None
    } else {
        Some(owner)
    }
}

/// Try to install a package via common system package managers (apt, then dnf, then zypper).
/// Returns `Ok(())` on first success, or a `BootstrapFailed` error if all attempts fail.
pub(super) fn bootstrap_via_system_manager(
    printer: &Printer,
    target_pkg: &str,
    manager_name: &str,
) -> Result<()> {
    for cmd_name in ["apt-get", "dnf", "zypper"] {
        if command_available(cmd_name) {
            let result = printer
                .run_with_output(
                    sudo_cmd(cmd_name).args(["install", "-y", target_pkg]),
                    &format!("Installing {} via {}", target_pkg, cmd_name),
                )
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: manager_name.into(),
                    message: format!("{} install failed: {}", cmd_name, e),
                })?;
            if result.status.success() {
                return Ok(());
            }
        }
    }
    Err(PackageError::BootstrapFailed {
        manager: manager_name.into(),
        message: format!("failed to install {} via apt, dnf, or zypper", target_pkg),
    }
    .into())
}

/// Try to install packages via brew first, then fall back to system package managers.
/// `brew_pkg` is the brew formula name, `apt_pkgs`/`dnf_pkgs` are the system package names.
/// Returns `Ok(true)` if installed, `Ok(false)` if no method succeeded (caller should
/// try alternative), or `Err` on command execution failure.
pub(super) fn bootstrap_via_brew_then_system(
    printer: &Printer,
    manager_name: &str,
    brew_pkg: &str,
    system_pkgs: &[&str],
) -> Result<bool> {
    if brew_available() {
        let result = printer
            .run_with_output(
                brew_cmd().args(["install", brew_pkg]),
                &format!("Installing {} via brew", brew_pkg),
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: manager_name.into(),
                message: format!("brew install {} failed: {}", brew_pkg, e),
            })?;
        if result.status.success() {
            return Ok(true);
        }
    }

    for cmd_name in ["apt-get", "dnf"] {
        if command_available(cmd_name) {
            let result = printer
                .run_with_output(
                    sudo_cmd(cmd_name).args(["install", "-y"]).args(system_pkgs),
                    &format!("Installing {} via {}", manager_name, cmd_name),
                )
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: manager_name.into(),
                    message: format!("{} install failed: {}", cmd_name, e),
                })?;
            if result.status.success() {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Strip trailing "-VERSION" from package names where version starts with a digit.
/// Used by apk, pkg, and nix-env which output "name-version" format.
pub(super) fn strip_version_suffix(name: &str) -> String {
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
pub(super) fn strip_arch_suffix(name: &str) -> String {
    name.rsplit_once('.').map_or(name, |(n, _)| n).to_string()
}

/// Strip leading `"sudo"` from a command slice when already running as root.
/// Returns the effective command slice (unchanged if not root or no sudo prefix).
pub(super) fn strip_sudo_if_root<'a>(cmd: &'a [&'a str]) -> &'a [&'a str] {
    if cmd.first() == Some(&"sudo") && cfgd_core::is_root() {
        &cmd[1..]
    } else {
        cmd
    }
}

/// Build a Command that prepends `sudo` only when not already running as root.
pub(super) fn sudo_cmd(program: &str) -> Command {
    if cfgd_core::is_root() {
        Command::new(program)
    } else {
        let mut cmd = Command::new("sudo");
        cmd.arg(program);
        cmd
    }
}

/// Parse a "Version: X.Y.Z" line from command output.
/// Used by flatpak, winget, and scoop version queries.
pub(super) fn parse_version_field(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Version:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests;
