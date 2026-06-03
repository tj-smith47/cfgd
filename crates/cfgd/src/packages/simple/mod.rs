//! `SimpleManager` — a data-driven `PackageManager` covering apt, dnf, yum,
//! apk, pacman, zypper, and pkg.
//!
//! Each constructor (`apt_manager`, `dnf_manager`, ...) wires up the manager
//! name, list/install/uninstall/update commands, parser, and version-query
//! function. Behavioural overrides (yum-only-when-no-dnf, dnf check-update
//! exit-code-100) are encoded as struct fields, not subclasses.

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;
use cfgd_core::{command_available, command_available_with_seam, tool_cmd};

use super::parsers::{
    parse_apk_lines, parse_dnf_lines, parse_pkg_lines, parse_simple_lines, parse_yum_lines,
    parse_zypper_lines,
};
use super::shared::{run_pkg_cmd, run_pkg_cmd_live, strip_sudo_if_root};
use super::versions::{
    APK_BIN_ENV, APT_CACHE_BIN_ENV, DNF_BIN_ENV, DPKG_QUERY_BIN_ENV, PACMAN_BIN_ENV, PKG_BIN_ENV,
    RPM_BIN_ENV, YUM_BIN_ENV, ZYPPER_BIN_ENV, apt_aliases, dnf_aliases, list_apt_with_versions,
    list_dnf_with_versions, query_version_apk, query_version_apt, query_version_info,
    query_version_pkg,
};

pub(super) const APT_GET_BIN_ENV: &str = "CFGD_APT_GET_BIN";

/// Map a SimpleManager `mgr_name` to the `CFGD_*_BIN` env-var seam that targets
/// the SAME binary. Used so `is_available()` honors the same test-shim seam
/// `query_version_*` honors — without this, a test that shims CFGD_DNF_BIN
/// cannot make dnf_manager.is_available() return true on a host without real
/// dnf on PATH. Returns None when the manager binary differs from any seamed
/// query tool (e.g., apt's mgr_name is "apt" but the query backend is
/// apt-cache / dpkg-query — those use their own seams in versions/mod.rs).
fn mgr_seam_env(mgr_name: &str) -> Option<&'static str> {
    match mgr_name {
        "apk" => Some(APK_BIN_ENV),
        "dnf" => Some(DNF_BIN_ENV),
        "yum" => Some(YUM_BIN_ENV),
        "pacman" => Some(PACMAN_BIN_ENV),
        "zypper" => Some(ZYPPER_BIN_ENV),
        _ => None,
    }
}

/// Build a `Command` for a package-manager binary, routing through the same
/// `CFGD_*_BIN` seams the query helpers honor. Unknown binaries (most commonly
/// `"sudo"`) fall through to plain `Command::new`. This is the single entry
/// point for install / uninstall / update / list shell-outs in this module,
/// so a test that shims CFGD_DPKG_QUERY_BIN sees its shim drive both
/// `installed_packages` and `list_apt_with_versions`.
fn cmd_with_seam(prog: &str) -> Command {
    let env = match prog {
        "apt-cache" => APT_CACHE_BIN_ENV,
        "apt-get" => APT_GET_BIN_ENV,
        "apk" => APK_BIN_ENV,
        "dnf" => DNF_BIN_ENV,
        "yum" => YUM_BIN_ENV,
        "pacman" => PACMAN_BIN_ENV,
        "zypper" => ZYPPER_BIN_ENV,
        "pkg" => PKG_BIN_ENV,
        "dpkg-query" => DPKG_QUERY_BIN_ENV,
        "rpm" => RPM_BIN_ENV,
        _ => return Command::new(prog),
    };
    tool_cmd(env, prog)
}

/// Function pointer type for `installed_packages_with_versions` overrides.
type ListWithVersionsFn = fn(&str) -> Result<Vec<cfgd_core::providers::PackageInfo>>;

/// A data-driven package manager for system package managers that follow a
/// uniform pattern: list installed, install, uninstall, update.
/// Replaces individual structs for apt, dnf, yum, apk, pacman, zypper, pkg.
pub struct SimpleManager {
    pub(super) mgr_name: &'static str,
    pub(super) list_cmd: &'static [&'static str],
    pub(super) install_cmd: &'static [&'static str],
    pub(super) uninstall_cmd: &'static [&'static str],
    pub(super) update_cmd: Option<&'static [&'static str]>,
    /// When true, non-zero exit from the update command is ignored (dnf/yum
    /// check-update returns 100 when updates are available).
    pub(super) ignore_update_exit: bool,
    pub(super) parse_list: fn(&str) -> HashSet<String>,
    pub(super) query_version: fn(&str, &str) -> Result<Option<String>>,
    /// Custom availability check. When None, uses `command_available(mgr_name)`.
    pub(super) is_available_fn: Option<fn() -> bool>,
    /// Override for installed_packages_with_versions. When None, falls back to
    /// the default trait implementation (wraps installed_packages with "unknown").
    pub(super) list_with_versions: Option<ListWithVersionsFn>,
    /// Override for package_aliases. When None, returns empty vec (default).
    pub(super) aliases_fn: Option<fn(&str) -> Vec<String>>,
}

impl SimpleManager {
    pub(super) fn display_cmd(&self, cmd_parts: &[&str], packages: &[String]) -> String {
        let effective = strip_sudo_if_root(cmd_parts);
        let mut parts: Vec<&str> = effective.to_vec();
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
        } else if let Some(env) = mgr_seam_env(self.mgr_name) {
            command_available_with_seam(env, self.mgr_name)
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
        let output = run_pkg_cmd(self.mgr_name, cmd_with_seam(prog).args(args), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok((self.parse_list)(&stdout))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let effective = strip_sudo_if_root(self.install_cmd);
        let label = self.display_cmd(self.install_cmd, packages);
        let (prog, args) = effective.split_first().unwrap_or((&"true", &[]));
        run_pkg_cmd_live(
            printer,
            self.mgr_name,
            cmd_with_seam(prog).args(args).args(packages),
            &label,
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let effective = strip_sudo_if_root(self.uninstall_cmd);
        let label = self.display_cmd(self.uninstall_cmd, packages);
        let (prog, args) = effective.split_first().unwrap_or((&"true", &[]));
        run_pkg_cmd_live(
            printer,
            self.mgr_name,
            cmd_with_seam(prog).args(args).args(packages),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        let Some(update_parts) = self.update_cmd else {
            return Ok(());
        };
        let effective = strip_sudo_if_root(update_parts);
        let label = self.display_cmd(update_parts, &[]);
        let (prog, args) = effective.split_first().unwrap_or((&"true", &[]));
        if self.ignore_update_exit {
            // dnf/yum check-update returns 100 when updates are available
            let _ = printer
                .run(cmd_with_seam(prog).args(args), &label)
                .map_err(|e| PackageError::CommandFailed {
                    manager: self.mgr_name.into(),
                    source: e,
                })?;
        } else {
            run_pkg_cmd_live(
                printer,
                self.mgr_name,
                cmd_with_seam(prog).args(args),
                &label,
                "update",
            )?;
        }
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        (self.query_version)(self.mgr_name, package)
    }

    fn installed_packages_with_versions(&self) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        if let Some(f) = self.list_with_versions {
            f(self.mgr_name)
        } else {
            // Default: wrap installed_packages with "unknown"
            Ok(self
                .installed_packages()?
                .into_iter()
                .map(|name| cfgd_core::providers::PackageInfo {
                    name,
                    version: "unknown".into(),
                })
                .collect())
        }
    }

    fn package_aliases(&self, canonical_name: &str) -> Result<Vec<String>> {
        if let Some(f) = self.aliases_fn {
            Ok(f(canonical_name))
        } else {
            Ok(vec![])
        }
    }
}

// --- SimpleManager constructors ---

pub(super) fn apt_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "apt",
        list_cmd: &["dpkg-query", "-W", "-f", "${Package}\n"],
        install_cmd: &["sudo", "apt-get", "install", "-y"],
        uninstall_cmd: &["sudo", "apt-get", "remove", "-y"],
        update_cmd: Some(&["sudo", "apt-get", "update"]),
        ignore_update_exit: false,
        parse_list: parse_simple_lines,
        query_version: query_version_apt,
        is_available_fn: None,
        list_with_versions: Some(list_apt_with_versions),
        aliases_fn: Some(apt_aliases),
    }
}

pub(super) fn dnf_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "dnf",
        // `--installed` (flag) not `installed` (positional): dnf5 (Fedora 41+)
        // reads a positional `installed` as a package spec and exits 1; the flag
        // form is accepted by both dnf4 and dnf5.
        list_cmd: &["dnf", "list", "--installed", "--quiet"],
        install_cmd: &["sudo", "dnf", "install", "-y"],
        uninstall_cmd: &["sudo", "dnf", "remove", "-y"],
        update_cmd: Some(&["sudo", "dnf", "check-update"]),
        ignore_update_exit: true,
        parse_list: parse_dnf_lines,
        query_version: query_version_info,
        is_available_fn: None,
        list_with_versions: Some(list_dnf_with_versions),
        aliases_fn: Some(dnf_aliases),
    }
}

pub(super) fn yum_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "yum",
        list_cmd: &["yum", "list", "installed", "--quiet"],
        install_cmd: &["sudo", "yum", "install", "-y"],
        uninstall_cmd: &["sudo", "yum", "remove", "-y"],
        update_cmd: Some(&["sudo", "yum", "check-update"]),
        ignore_update_exit: true,
        parse_list: parse_yum_lines,
        query_version: query_version_info,
        is_available_fn: Some(|| {
            !command_available_with_seam(DNF_BIN_ENV, "dnf")
                && command_available_with_seam(YUM_BIN_ENV, "yum")
        }),
        list_with_versions: Some(list_dnf_with_versions),
        aliases_fn: Some(dnf_aliases),
    }
}

pub(super) fn apk_manager() -> SimpleManager {
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
        list_with_versions: None,
        aliases_fn: None,
    }
}

pub(super) fn pacman_manager() -> SimpleManager {
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
        list_with_versions: None,
        aliases_fn: None,
    }
}

pub(super) fn zypper_manager() -> SimpleManager {
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
        list_with_versions: None,
        aliases_fn: None,
    }
}

pub(super) fn pkg_manager() -> SimpleManager {
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
        list_with_versions: None,
        aliases_fn: None,
    }
}

#[cfg(test)]
mod tests;
