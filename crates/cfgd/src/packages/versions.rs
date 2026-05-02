//! Version-query helpers used by `SimpleManager`.
//!
//! `query_version_*` functions answer "what version is available?" by shelling
//! out to the underlying package manager. `list_*_with_versions` enumerate
//! installed packages with versions for `installed_packages_with_versions`.
//! `*_aliases` map canonical package names to their distro-specific aliases.

use std::process::Command;

use cfgd_core::errors::{PackageError, Result};

use super::shared::run_pkg_cmd;

/// Query version via `<cmd> info <pkg>` and parse "Version:" field.
/// Used by dnf, yum, pacman (-Si), zypper.
pub(super) fn query_version_info(manager: &str, package: &str) -> Result<Option<String>> {
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

pub(super) fn query_version_apt(manager: &str, package: &str) -> Result<Option<String>> {
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

pub(super) fn query_version_apk(manager: &str, package: &str) -> Result<Option<String>> {
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

pub(super) fn query_version_pkg(manager: &str, package: &str) -> Result<Option<String>> {
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
        Command::new("dpkg-query").args(["-W", "-f=${Package}\t${Version}\n"]),
        "list",
    )?;
    Ok(parse_apt_versions(&String::from_utf8_lossy(&output.stdout)))
}

pub(super) fn list_dnf_with_versions(
    manager: &str,
) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
    let output = run_pkg_cmd(
        manager,
        Command::new("rpm").args(["--query", "--all", "--queryformat", "%{NAME}\t%{VERSION}\n"]),
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
