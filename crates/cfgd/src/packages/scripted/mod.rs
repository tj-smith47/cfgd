//! User-defined "scripted" package manager backed by shell command templates.

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{run_pkg_cmd, run_pkg_cmd_msg};

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
                let escaped = cfgd_core::shell_escape_value(pkg);
                let cmd = template.replace("{package}", &escaped);
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
            let escaped_pkgs: Vec<String> = packages
                .iter()
                .map(|p| cfgd_core::shell_escape_value(p))
                .collect();
            let joined = escaped_pkgs.join(" ");
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

#[cfg(test)]
mod tests;
