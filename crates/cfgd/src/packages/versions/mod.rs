//! Version-query helpers used by `SimpleManager`.
//!
//! `query_version_*` functions answer "what version is available?" by shelling
//! out to the underlying package manager. `list_*_with_versions` enumerate
//! installed packages with versions for `installed_packages_with_versions`.
//! `*_aliases` map canonical package names to their distro-specific aliases.
//!
//! Every shell-out IN THIS MODULE routes through `cfgd_core::tool_cmd(env_var,
//! default)` so the `CFGD_*_BIN` test-shim seams can drive the binaries without
//! a real package manager installed. `query_version_info` dispatches the seam
//! per `manager` arg (pacman / dnf / yum / zypper) so each manager has an
//! independent override knob. The install/uninstall/list paths in
//! `packages::simple::mod` still shell out via raw `Command::new` and are not
//! yet seamed.

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::tool_cmd;

use super::shared::run_pkg_cmd;

pub(super) const APT_CACHE_BIN_ENV: &str = "CFGD_APT_CACHE_BIN";
pub(super) const APK_BIN_ENV: &str = "CFGD_APK_BIN";
pub(super) const PKG_BIN_ENV: &str = "CFGD_PKG_BIN";
pub(super) const PACMAN_BIN_ENV: &str = "CFGD_PACMAN_BIN";
pub(super) const DNF_BIN_ENV: &str = "CFGD_DNF_BIN";
pub(super) const YUM_BIN_ENV: &str = "CFGD_YUM_BIN";
pub(super) const ZYPPER_BIN_ENV: &str = "CFGD_ZYPPER_BIN";
pub(super) const DPKG_QUERY_BIN_ENV: &str = "CFGD_DPKG_QUERY_BIN";
pub(super) const RPM_BIN_ENV: &str = "CFGD_RPM_BIN";

/// Map an `info`-style manager name to its env-var seam. Unknown managers
/// debug-assert (catches typos in tests) and log a warning, then fall through
/// to an empty seam so production keeps working via PATH lookup of `default`.
fn info_bin_env(manager: &str) -> &'static str {
    match manager {
        "pacman" => PACMAN_BIN_ENV,
        "dnf" => DNF_BIN_ENV,
        "yum" => YUM_BIN_ENV,
        "zypper" => ZYPPER_BIN_ENV,
        other => {
            debug_assert!(
                false,
                "query_version_info called with unknown manager {other:?}; CFGD_*_BIN seam silently bypassed"
            );
            tracing::warn!(
                manager = other,
                "query_version_info: no CFGD_*_BIN seam registered; falling through to PATH"
            );
            ""
        }
    }
}

/// Query version via `<cmd> info <pkg>` and parse "Version:" field.
/// Used by dnf, yum, pacman (-Si), zypper.
pub(super) fn query_version_info(manager: &str, package: &str) -> Result<Option<String>> {
    let (cmd, args): (&str, &[&str]) = match manager {
        "pacman" => ("pacman", &["-Si"]),
        _ => (manager, &["info"]),
    };
    let output = tool_cmd(info_bin_env(manager), cmd)
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

pub(super) fn query_version_apt(manager: &str, package: &str) -> Result<Option<String>> {
    let output = tool_cmd(APT_CACHE_BIN_ENV, "apt-cache")
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

pub(super) fn query_version_apk(manager: &str, package: &str) -> Result<Option<String>> {
    let output = tool_cmd(APK_BIN_ENV, "apk")
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
    // apk policy <pkg> output:
    //   <pkg> policy:        (column 0)
    //     <version>:          (indented)
    //       <repo-url>        (deeper indent)
    // Return the first indented line whose trimmed/colon-stripped value starts with a digit.
    for line in stdout.lines() {
        if !line.starts_with(char::is_whitespace) {
            continue;
        }
        let trimmed = line.trim().trim_end_matches(':');
        if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Ok(Some(trimmed.to_string()));
        }
    }
    Ok(None)
}

pub(super) fn query_version_pkg(manager: &str, package: &str) -> Result<Option<String>> {
    let output = tool_cmd(PKG_BIN_ENV, "pkg")
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

/// Parse `dpkg-query -W -f='${Package}\t${Version}\n'` output into PackageInfo.
/// Parse tab-separated `NAME\tVERSION` output into PackageInfo.
/// Used by both apt (dpkg-query) and rpm (rpm -qa --queryformat) parsers.
pub(super) fn parse_tab_separated_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let name = parts.next()?.trim();
            let version = parts.next().unwrap_or("unknown").trim();
            if name.is_empty() {
                return None;
            }
            Some(cfgd_core::providers::PackageInfo {
                name: name.to_string(),
                version: if version.is_empty() {
                    "unknown".to_string()
                } else {
                    version.to_string()
                },
            })
        })
        .collect()
}

pub(super) fn parse_apt_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    parse_tab_separated_versions(stdout)
}

pub(super) fn parse_rpm_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    parse_tab_separated_versions(stdout)
}

pub(super) fn list_apt_with_versions(
    manager: &str,
) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
    let output = run_pkg_cmd(
        manager,
        tool_cmd(DPKG_QUERY_BIN_ENV, "dpkg-query").args(["-W", "-f=${Package}\t${Version}\n"]),
        "list",
    )?;
    Ok(parse_apt_versions(&String::from_utf8_lossy(&output.stdout)))
}

pub(super) fn list_dnf_with_versions(
    manager: &str,
) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
    let output = run_pkg_cmd(
        manager,
        tool_cmd(RPM_BIN_ENV, "rpm").args([
            "--query",
            "--all",
            "--queryformat",
            "%{NAME}\t%{VERSION}\n",
        ]),
        "list",
    )?;
    Ok(parse_rpm_versions(&String::from_utf8_lossy(&output.stdout)))
}

pub(super) fn apt_aliases(canonical_name: &str) -> Vec<String> {
    match canonical_name {
        "fd" => vec!["fd-find".to_string()],
        "rg" => vec!["ripgrep".to_string()],
        "bat" => vec!["batcat".to_string()],
        "nvim" => vec!["neovim".to_string()],
        _ => vec![],
    }
}

pub(super) fn dnf_aliases(canonical_name: &str) -> Vec<String> {
    match canonical_name {
        "fd" => vec!["fd-find".to_string()],
        "nvim" => vec!["neovim".to_string()],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests;
