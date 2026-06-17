//! User-defined "scripted" package manager backed by shell command templates.

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::PackageManager;

use super::shared::{run_pkg_cmd, run_pkg_cmd_msg};

/// Build the OS-native shell invocation for a user-authored package-manager
/// command string: `sh -c <cmd>` on Unix, `cmd.exe /C <cmd>` on Windows.
///
/// Scripted-manager templates (`check` / `list` / `install` / `uninstall` /
/// `update`) are free-form shell commands the user writes for their platform;
/// hardcoding `sh` left every scripted manager dead on Windows (no `sh` on
/// PATH). This mirrors the `ScriptShell::Auto` default the lifecycle-script
/// runner already uses, so both surfaces resolve the same native shell.
fn shell_command(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd.exe");
        c.args(["/C", cmd]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    }
}

pub struct ScriptedManager {
    pub(super) mgr_name: String,
    pub(super) check_cmd: String,
    pub(super) list_cmd: String,
    pub(super) install_cmd: String,
    pub(super) uninstall_cmd: String,
    pub(super) update_cmd: Option<String>,
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

    /// Build a `ScriptedManager` carrying only an uninstall template, for the
    /// orphaned-package GC path: the manager block has left the config, so only
    /// the persisted uninstall command survives. The other script fields are left
    /// empty and are never invoked on this instance. `mgr_name` is kept so error
    /// messages still name the manager.
    pub fn from_uninstall_only(name: &str, uninstall_cmd: String) -> Self {
        Self {
            mgr_name: name.to_string(),
            check_cmd: String::new(),
            list_cmd: String::new(),
            install_cmd: String::new(),
            uninstall_cmd,
            update_cmd: None,
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
        let invocations = build_template_invocations(template, packages);
        if template.contains("{package}") {
            // One-at-a-time mode — each package emitted as its own command,
            // surfaced as a per-package error so the caller can see which
            // member of the batch failed.
            for (cmd, pkg) in invocations.iter().zip(packages) {
                printer.status_simple(Role::Info, cmd.as_str());
                run_pkg_cmd_msg(&self.mgr_name, &mut shell_command(cmd), error_kind, pkg)?;
            }
        } else if let Some(cmd) = invocations.first() {
            // Batch mode — build_template_invocations emits a single command.
            printer.status_simple(Role::Info, cmd.as_str());
            run_pkg_cmd(&self.mgr_name, &mut shell_command(cmd), error_kind)?;
        }
        Ok(())
    }
}

/// Build the command string(s) ScriptedManager passes to the platform shell
/// (`sh -c` on Unix, `cmd.exe /C` on Windows via [`shell_command`]) to invoke a
/// custom package-manager template. Pure helper — split out so the substitution
/// + shell-escaping contract is testable without spawning a shell.
///
/// Two modes:
/// - **`{package}` placeholder**: one-at-a-time. Returns one command per
///   package, with `{package}` substituted by the shell-escaped package name.
/// - **`{packages}` placeholder or no placeholder**: batch. Returns a single
///   command with the shell-escaped, space-joined package list either spliced
///   into `{packages}` or appended after the template.
pub(super) fn build_template_invocations(template: &str, packages: &[String]) -> Vec<String> {
    if packages.is_empty() {
        return Vec::new();
    }
    if template.contains("{package}") {
        return packages
            .iter()
            .map(|pkg| {
                let escaped = cfgd_core::shell_escape_value(pkg);
                template.replace("{package}", &escaped)
            })
            .collect();
    }
    let escaped: Vec<String> = packages
        .iter()
        .map(|p| cfgd_core::shell_escape_value(p))
        .collect();
    let joined = escaped.join(" ");
    let cmd = if template.contains("{packages}") {
        template.replace("{packages}", &joined)
    } else {
        format!("{} {}", template, joined)
    };
    vec![cmd]
}

impl PackageManager for ScriptedManager {
    fn name(&self) -> &str {
        &self.mgr_name
    }

    fn is_available(&self) -> bool {
        shell_command(&self.check_cmd)
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
        let output = run_pkg_cmd(&self.mgr_name, &mut shell_command(&self.list_cmd), "list")?;
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
            printer.status_simple(Role::Info, cmd.as_str());
            run_pkg_cmd_msg(
                &self.mgr_name,
                &mut shell_command(cmd),
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

    fn persisted_uninstall(&self) -> Option<String> {
        Some(self.uninstall_cmd.clone())
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

#[cfg(test)]
mod tests;
