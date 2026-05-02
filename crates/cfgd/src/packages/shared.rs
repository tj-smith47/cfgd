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

/// Locate a package-manager binary. First checks `$PATH` via `command_available`;
/// on miss, walks each entry in `fallbacks` and returns the first that exists.
/// Returns `None` if nothing is found — matches the `find_X() -> Option<PathBuf>`
/// shape that cargo/pipx/go managers had open-coded.
pub(super) fn resolve_tool_with_fallbacks(name: &str, fallbacks: &[PathBuf]) -> Option<PathBuf> {
    if command_available(name) {
        return Some(PathBuf::from(name));
    }
    fallbacks.iter().find(|p| p.exists()).cloned()
}

/// Build a `Command` for `name`, using `resolver` for the binary path and
/// falling back to a plain `Command::new(name)` when `resolver` returns `None`.
/// Mirrors the `X_cmd()` pattern that cargo/pipx/go had open-coded.
pub(super) fn tool_cmd_with_resolver<F>(name: &str, resolver: F) -> Command
where
    F: FnOnce() -> Option<PathBuf>,
{
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

/// Check if brew is available, including linuxbrew fallback on Linux.
pub(super) fn brew_available() -> bool {
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
pub(super) fn brew_cmd() -> Command {
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
mod tests {
    use cfgd_core::output::{CommandOutput, Printer};

    use super::*;

    fn test_cmd_output(stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            status: std::process::ExitStatus::default(),
            duration: std::time::Duration::from_secs(0),
        }
    }

    #[test]
    fn strip_version_suffix_removes_version() {
        assert_eq!(strip_version_suffix("curl-7.88.1"), "curl");
    }

    #[test]
    fn strip_version_suffix_no_version() {
        assert_eq!(strip_version_suffix("curl"), "curl");
    }

    #[test]
    fn strip_version_suffix_hyphen_no_digit() {
        assert_eq!(strip_version_suffix("lib-utils"), "lib-utils");
    }

    #[test]
    fn strip_arch_suffix_removes_arch() {
        assert_eq!(strip_arch_suffix("vim.x86_64"), "vim");
    }

    #[test]
    fn strip_arch_suffix_noarch() {
        assert_eq!(strip_arch_suffix("vim.noarch"), "vim");
    }

    #[test]
    fn strip_arch_suffix_no_dot() {
        assert_eq!(strip_arch_suffix("vim"), "vim");
    }

    #[test]
    fn stderr_lossy_converts() {
        use std::process::Output;
        let output = Output {
            status: std::process::ExitStatus::default(),
            stdout: vec![],
            stderr: b"error message".to_vec(),
        };
        assert_eq!(cfgd_core::stderr_lossy_trimmed(&output), "error message");
    }

    #[test]
    fn extract_caveats_brew_section() {
        let output = test_cmd_output(
            "==> Installing ripgrep\n==> Caveats\nAdd to PATH: /opt/homebrew/bin\nRestart terminal.\n==> Summary\nDone.",
            "",
        );
        let notes = extract_caveats("brew", &output);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("Add to PATH"));
        assert!(notes[0].message.contains("Restart terminal."));
    }

    #[test]
    fn extract_caveats_brew_no_caveats() {
        let output = test_cmd_output("==> Installing ripgrep\n==> Summary\nDone.", "");
        let notes = extract_caveats("brew", &output);
        assert!(notes.is_empty());
    }

    #[test]
    fn extract_caveats_npm_warnings() {
        let output = test_cmd_output(
            "",
            "npm warn deprecated foo@1.0\nnpm WARN peer dep missing\n",
        );
        let notes = extract_caveats("npm", &output);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn extract_caveats_pip_warnings() {
        let output = test_cmd_output("WARNING: pip is out of date\nInstalled black\n", "");
        let notes = extract_caveats("pip", &output);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("pip is out of date"));
    }

    #[test]
    fn extract_caveats_generic_manager() {
        let output = test_cmd_output(
            "",
            "warning: package foo replaced bar\nnote: restart required\n",
        );
        let notes = extract_caveats("pacman", &output);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn extract_caveats_empty_output() {
        let output = test_cmd_output("", "");
        let notes = extract_caveats("brew", &output);
        assert!(notes.is_empty());
    }

    #[test]
    fn strip_sudo_if_root_returns_slice_unchanged_when_no_sudo() {
        let cmd: &[&str] = &["apt-get", "install", "-y"];
        let result = strip_sudo_if_root(cmd);
        assert_eq!(result, &["apt-get", "install", "-y"]);
    }

    #[test]
    fn strip_sudo_if_root_empty_slice() {
        let cmd: &[&str] = &[];
        let result = strip_sudo_if_root(cmd);
        assert!(result.is_empty());
    }

    #[test]
    fn extract_caveats_brew_multiple_caveat_sections() {
        let output = test_cmd_output(
            "==> Caveats\nFirst caveat\n==> Installing dep\n==> Caveats\nSecond caveat\n",
            "",
        );
        let notes = extract_caveats("brew", &output);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].message.contains("First caveat"));
        assert!(notes[1].message.contains("Second caveat"));
    }

    #[test]
    fn extract_caveats_brew_cask_works_same_as_brew() {
        let output = test_cmd_output("==> Caveats\nRestart to complete install.\n==> Done\n", "");
        let notes = extract_caveats("brew-cask", &output);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].manager, "brew-cask");
        assert!(notes[0].message.contains("Restart to complete install"));
    }

    #[test]
    fn extract_caveats_brew_caveats_at_end_of_output() {
        // Caveats section at the very end with no following "==> " section
        let output = test_cmd_output(
            "==> Installing ripgrep\n==> Caveats\nAdd brew to PATH.\n",
            "",
        );
        let notes = extract_caveats("brew", &output);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("Add brew to PATH"));
    }

    #[test]
    fn extract_caveats_npm_no_warnings() {
        let output = test_cmd_output("added 5 packages\n", "");
        let notes = extract_caveats("npm", &output);
        assert!(notes.is_empty());
    }

    #[test]
    fn extract_caveats_pnpm_warnings() {
        let output = test_cmd_output("", "npm warn deprecated some-pkg@1.0\n");
        let notes = extract_caveats("pnpm", &output);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].manager, "pnpm");
    }

    #[test]
    fn extract_caveats_pipx_warnings() {
        let output = test_cmd_output(
            "WARNING: virtual environment exists\nInstalled httpie\n",
            "",
        );
        let notes = extract_caveats("pipx", &output);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("virtual environment"));
    }

    #[test]
    fn extract_caveats_generic_no_warnings() {
        let output = test_cmd_output("success", "all good\n");
        let notes = extract_caveats("unknown-mgr", &output);
        assert!(notes.is_empty());
    }

    #[test]
    fn extract_caveats_generic_note_in_stderr() {
        let output = test_cmd_output("", "note: some important info\n");
        let notes = extract_caveats("zypper", &output);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("some important info"));
    }

    #[test]
    fn extract_caveats_generic_caveat_in_stderr() {
        let output = test_cmd_output("", "caveat: restart required\n");
        let notes = extract_caveats("apk", &output);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn strip_version_suffix_multiple_version_like_hyphens() {
        // "lib-xml2-2.10.3" → should strip from last hyphen before digit
        assert_eq!(strip_version_suffix("lib-xml2-2.10.3"), "lib-xml2");
    }

    #[test]
    fn strip_version_suffix_only_digit_after_hyphen() {
        assert_eq!(strip_version_suffix("pkg-1"), "pkg");
    }

    #[test]
    fn strip_version_suffix_empty_string() {
        assert_eq!(strip_version_suffix(""), "");
    }

    #[test]
    fn strip_version_suffix_trailing_hyphen_no_digit() {
        assert_eq!(strip_version_suffix("pkg-"), "pkg-");
    }

    #[test]
    fn strip_arch_suffix_multiple_dots() {
        // rsplit_once splits on the last dot
        assert_eq!(strip_arch_suffix("some.package.x86_64"), "some.package");
    }

    #[test]
    fn strip_arch_suffix_empty_string() {
        assert_eq!(strip_arch_suffix(""), "");
    }

    #[test]
    fn post_install_note_fields() {
        let note = PostInstallNote {
            manager: "brew".to_string(),
            message: "test message".to_string(),
        };
        assert_eq!(note.manager, "brew");
        assert_eq!(note.message, "test message");
    }

    #[test]
    fn print_caveats_empty_is_noop() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        // Should not panic
        print_caveats(&printer, &[]);
    }

    #[test]
    fn print_caveats_non_empty() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let notes = vec![PostInstallNote {
            manager: "brew".to_string(),
            message: "Add to PATH".to_string(),
        }];
        // Should not panic
        print_caveats(&printer, &notes);
    }

    #[test]
    fn extract_caveats_brew_empty_caveat_section() {
        // Caveats section immediately followed by another section with no content
        let output = test_cmd_output("==> Caveats\n==> Summary\nDone.", "");
        let notes = extract_caveats("brew", &output);
        assert!(notes.is_empty());
    }

    #[test]
    fn extract_caveats_brew_caveat_in_stderr() {
        // Brew caveats appear in stdout, but test that stderr is also scanned
        let output = test_cmd_output("", "==> Caveats\nSet up PATH.\n");
        let notes = extract_caveats("brew", &output);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("Set up PATH"));
    }

    #[test]
    fn extract_caveats_generic_warning_case_insensitive() {
        let output = test_cmd_output("", "WARNING: important\nWarning: also important\n");
        let notes = extract_caveats("zypper", &output);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn extract_caveats_generic_only_checks_stderr() {
        // Generic manager only scans stderr, not stdout
        let output = test_cmd_output("warning: this is in stdout\n", "");
        let notes = extract_caveats("unknown-mgr", &output);
        assert!(notes.is_empty());
    }

    #[test]
    fn extract_caveats_npm_in_stdout() {
        // npm warnings can appear in stdout too (combined is checked)
        let output = test_cmd_output("npm warn old package\n", "");
        let notes = extract_caveats("npm", &output);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn extract_caveats_pip_in_stderr() {
        let output = test_cmd_output("", "WARNING: pip upgrade available\n");
        let notes = extract_caveats("pip", &output);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn print_caveats_multiple_notes() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let notes = vec![
            PostInstallNote {
                manager: "brew".to_string(),
                message: "First note".to_string(),
            },
            PostInstallNote {
                manager: "npm".to_string(),
                message: "Second note".to_string(),
            },
            PostInstallNote {
                manager: "pip".to_string(),
                message: "Third note".to_string(),
            },
        ];
        // Should not panic
        print_caveats(&printer, &notes);
    }

    #[test]
    fn strip_sudo_if_root_single_element_sudo() {
        let cmd: &[&str] = &["sudo"];
        let result = strip_sudo_if_root(cmd);
        // When running as non-root in tests, sudo stays
        if cfgd_core::is_root() {
            assert!(result.is_empty());
        } else {
            assert_eq!(result, &["sudo"]);
        }
    }

    #[test]
    fn strip_sudo_if_root_non_sudo_first() {
        let cmd: &[&str] = &["apt-get", "install", "sudo"];
        let result = strip_sudo_if_root(cmd);
        // "sudo" is not the first element, so unchanged
        assert_eq!(result, &["apt-get", "install", "sudo"]);
    }

    #[test]
    fn print_caveats_outputs_subheader_and_warnings() {
        let (printer, buf) = Printer::for_test();
        let notes = vec![
            PostInstallNote {
                manager: "brew".to_string(),
                message: "Add /opt/homebrew/bin to PATH".to_string(),
            },
            PostInstallNote {
                manager: "npm".to_string(),
                message: "npm warn deprecated request@2.88.2".to_string(),
            },
        ];
        print_caveats(&printer, &notes);
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Post-install notes"),
            "missing subheader, got: {}",
            *output
        );
        assert!(
            output.contains("[brew] Add /opt/homebrew/bin to PATH"),
            "missing brew caveat, got: {}",
            *output
        );
        assert!(
            output.contains("[npm] npm warn deprecated request@2.88.2"),
            "missing npm caveat, got: {}",
            *output
        );
    }

    #[test]
    fn print_caveats_empty_produces_no_output() {
        let (printer, buf) = Printer::for_test();
        print_caveats(&printer, &[]);
        let output = buf.lock().unwrap();
        assert!(output.is_empty(), "expected no output, got: {}", *output);
    }

    #[test]
    fn extract_caveats_brew_caveats_in_both_stdout_and_stderr() {
        let output = test_cmd_output(
            "==> Caveats\nStdout caveat here\n==> Done\n",
            "==> Caveats\nStderr caveat here\n",
        );
        let notes = extract_caveats("brew", &output);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n.message.contains("Stdout caveat")));
        assert!(notes.iter().any(|n| n.message.contains("Stderr caveat")));
    }

    #[test]
    fn extract_caveats_pip_in_both_streams() {
        let output = test_cmd_output("WARNING: outdated pip\n", "WARNING: venv path conflict\n");
        let notes = extract_caveats("pip", &output);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn extract_caveats_npm_warn_uppercase_and_lowercase() {
        let output = test_cmd_output("npm warn old-dep\nnpm WARN peer issue\n", "");
        let notes = extract_caveats("npm", &output);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|n| n.manager == "npm"));
    }

    #[test]
    fn extract_caveats_brew_multiline_caveat_content() {
        let output = test_cmd_output(
            "==> Caveats\nLine 1 of caveat\nLine 2 of caveat\nLine 3 of caveat\n==> Summary\n",
            "",
        );
        let notes = extract_caveats("brew", &output);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("Line 1"));
        assert!(notes[0].message.contains("Line 2"));
        assert!(notes[0].message.contains("Line 3"));
    }

    #[test]
    fn parse_version_field_ignores_version_substring_in_other_keys() {
        // "AppVersion:" should not be confused with "Version:"
        let output = "AppVersion: 2.0.0\nVersion: 3.0.0\n";
        assert_eq!(
            parse_version_field(output),
            Some("3.0.0".to_string()),
            "should match exact 'Version:' prefix, not substrings"
        );
    }

    #[test]
    fn parse_version_field_trims_surrounding_whitespace() {
        let output = "  Version:   3.2.1  \n";
        assert_eq!(parse_version_field(output), Some("3.2.1".to_string()));
    }

    #[test]
    fn parse_version_field_skips_non_version_lines() {
        let output = "Name: something\nDescription: a package\n";
        assert_eq!(parse_version_field(output), None);
    }

    #[test]
    fn parse_version_field_first_match_wins() {
        let output = "Version: 1.0.0\nVersion: 2.0.0\n";
        assert_eq!(parse_version_field(output), Some("1.0.0".to_string()));
    }

    #[test]
    fn parse_version_field_empty_string() {
        assert_eq!(parse_version_field(""), None);
    }

    #[test]
    fn parse_version_field_version_at_start() {
        let output = "Version: 5.0\nOther: data\n";
        assert_eq!(parse_version_field(output), Some("5.0".to_string()));
    }

    #[test]
    fn parse_version_field_with_extra_spaces_in_value() {
        let output = "Version:    1.2.3.4    \n";
        assert_eq!(parse_version_field(output), Some("1.2.3.4".to_string()));
    }

    #[test]
    fn extract_caveats_generic_mixed_case_warning() {
        let output = test_cmd_output("", "Warning: something deprecated\n");
        let notes = extract_caveats("apk", &output);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn extract_caveats_generic_caveat_keyword_in_context() {
        let output = test_cmd_output("", "There is a caveat you should know about\n");
        let notes = extract_caveats("pacman", &output);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn extract_caveats_generic_ignores_normal_lines() {
        let output = test_cmd_output("", "Downloading packages...\nInstalling...\nComplete!\n");
        let notes = extract_caveats("dnf", &output);
        assert!(notes.is_empty());
    }

    #[test]
    fn strip_version_suffix_nix_env_format() {
        // nix-env -q output format: "nixpkgs.ripgrep-14.1.0"
        assert_eq!(
            strip_version_suffix("nixpkgs.ripgrep-14.1.0"),
            "nixpkgs.ripgrep"
        );
    }

    #[test]
    fn strip_version_suffix_single_digit_version() {
        assert_eq!(strip_version_suffix("tool-3"), "tool");
    }

    #[test]
    fn strip_version_suffix_letter_after_hyphen() {
        // Letter after hyphen — not a version
        assert_eq!(strip_version_suffix("my-tool"), "my-tool");
    }

    #[test]
    fn strip_arch_suffix_i686() {
        assert_eq!(strip_arch_suffix("glibc.i686"), "glibc");
    }

    #[test]
    fn strip_arch_suffix_aarch64() {
        assert_eq!(strip_arch_suffix("kernel.aarch64"), "kernel");
    }

    #[test]
    fn strip_arch_suffix_with_dots_in_name() {
        // Chocolatey-style package: "git.install" — only last dot is stripped
        assert_eq!(strip_arch_suffix("git.install"), "git");
    }

    #[test]
    fn extract_caveats_brew_caveats_only_blank_lines() {
        let output = test_cmd_output("==> Caveats\n\n\n==> Summary\n", "");
        let notes = extract_caveats("brew", &output);
        // Blank lines are captured, joined, then trimmed — result is empty string
        // but caveat_lines is non-empty so a PostInstallNote with empty message is produced
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].message.is_empty(),
            "message should be empty after trim"
        );
    }

    #[test]
    fn sudo_cmd_builds_correct_command_structure() {
        // sudo_cmd should prepend sudo when not root, or run directly when root
        let cmd = sudo_cmd("apt-get");
        let prog = format!("{:?}", cmd.get_program());
        if cfgd_core::is_root() {
            assert!(
                prog.contains("apt-get"),
                "as root, program should be apt-get, got: {}",
                prog
            );
        } else {
            assert!(
                prog.contains("sudo"),
                "as non-root, program should be sudo, got: {}",
                prog
            );
        }
    }

    #[test]
    fn sudo_cmd_non_root_has_program_as_first_arg() {
        let cmd = sudo_cmd("dnf");
        if !cfgd_core::is_root() {
            let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
            assert!(!args.is_empty(), "sudo_cmd should pass program name as arg");
            assert_eq!(args[0], "dnf");
        }
    }

    #[test]
    fn strip_sudo_if_root_with_sudo_prefix() {
        let cmd: &[&str] = &["sudo", "apt-get", "install", "-y"];
        let result = strip_sudo_if_root(cmd);
        if cfgd_core::is_root() {
            // As root, sudo is stripped
            assert_eq!(result, &["apt-get", "install", "-y"]);
        } else {
            // As non-root, unchanged
            assert_eq!(result, &["sudo", "apt-get", "install", "-y"]);
        }
    }

    #[test]
    fn run_pkg_cmd_install_error_maps_to_install_failed() {
        let result = run_pkg_cmd(
            "test-mgr",
            Command::new("sh").args(["-c", "echo install-err >&2; exit 1"]),
            "install",
        );
        let err = result.unwrap_err();
        assert!(
            matches!(&err, PackageError::InstallFailed { manager, message }
                if manager == "test-mgr" && message.contains("install-err")),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn run_pkg_cmd_uninstall_error_maps_to_uninstall_failed() {
        let result = run_pkg_cmd(
            "test-mgr",
            Command::new("sh").args(["-c", "echo rm-err >&2; exit 1"]),
            "uninstall",
        );
        let err = result.unwrap_err();
        assert!(
            matches!(&err, PackageError::UninstallFailed { manager, message }
                if manager == "test-mgr" && message.contains("rm-err")),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn run_pkg_cmd_list_error_maps_to_list_failed() {
        let result = run_pkg_cmd(
            "test-mgr",
            Command::new("sh").args(["-c", "echo list-err >&2; exit 1"]),
            "list",
        );
        let err = result.unwrap_err();
        assert!(
            matches!(&err, PackageError::ListFailed { manager, message }
                if manager == "test-mgr" && message.contains("list-err")),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn run_pkg_cmd_unknown_error_kind_maps_to_install_failed() {
        // The default match arm maps unknown error kinds to InstallFailed
        let result = run_pkg_cmd(
            "test-mgr",
            Command::new("sh").args(["-c", "echo unknown-err >&2; exit 1"]),
            "update",
        );
        let err = result.unwrap_err();
        assert!(
            matches!(&err, PackageError::InstallFailed { .. }),
            "unknown error kind should map to InstallFailed, got: {:?}",
            err
        );
    }

    #[test]
    fn run_pkg_cmd_success_returns_output() {
        let result = run_pkg_cmd(
            "test-mgr",
            Command::new("sh").args(["-c", "echo hello"]),
            "list",
        );
        let output = result.unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn run_pkg_cmd_msg_includes_prefix_in_error() {
        let result = run_pkg_cmd_msg(
            "test-mgr",
            Command::new("sh").args(["-c", "echo detail >&2; exit 1"]),
            "install",
            "my-package",
        );
        let err = result.unwrap_err();
        assert!(
            matches!(&err, PackageError::InstallFailed { message, .. }
                if message.contains("my-package") && message.contains("detail")),
            "expected prefix and stderr in error, got: {:?}",
            err
        );
    }

    #[test]
    fn run_pkg_cmd_msg_empty_prefix_not_prepended() {
        let result = run_pkg_cmd_msg(
            "test-mgr",
            Command::new("sh").args(["-c", "echo only-stderr >&2; exit 1"]),
            "install",
            "",
        );
        let err = result.unwrap_err();
        // Empty prefix should not be prepended (the Some("") + is_empty check)
        assert!(
            matches!(&err, PackageError::InstallFailed { message, .. }
                if !message.starts_with(':') && message.contains("only-stderr")),
            "empty prefix should not prepend, got: {:?}",
            err
        );
    }

    #[test]
    fn run_pkg_cmd_command_not_found_maps_to_command_failed() {
        let result = run_pkg_cmd(
            "test-mgr",
            &mut Command::new("/nonexistent/binary/path/that/does/not/exist"),
            "install",
        );
        let err = result.unwrap_err();
        assert!(
            matches!(&err, PackageError::CommandFailed { manager, .. }
                if manager == "test-mgr"),
            "expected CommandFailed, got: {:?}",
            err
        );
    }

    #[test]
    fn brew_available_returns_bool() {
        // Exercises brew_available() production function
        let _available = brew_available();
        // Just verifying it runs without panic
    }

    #[test]
    fn brew_cmd_returns_valid_command() {
        let cmd = brew_cmd();
        let prog = format!("{:?}", cmd.get_program());
        // Should be "brew", the linuxbrew path, or "sudo" (when root + linuxbrew)
        assert!(
            prog.contains("brew") || prog.contains("sudo"),
            "brew_cmd should return brew or sudo command, got: {}",
            prog
        );
    }

    #[test]
    fn brew_path_returns_option() {
        // Exercise the OnceLock-cached path
        let _path = brew_path();
        // Second call tests the cached path
        let _path2 = brew_path();
    }
}
