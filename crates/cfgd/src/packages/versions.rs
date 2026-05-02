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

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate the version extraction from apt-cache policy output as done in query_version_apt.
    fn parse_apt_candidate_version(stdout: &str) -> Option<String> {
        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Candidate:") {
                let version = rest.trim();
                if version == "(none)" {
                    return None;
                }
                let version = version
                    .split_once(':')
                    .map_or(version, |(_, v)| v)
                    .split_once('-')
                    .map_or_else(
                        || version.split_once(':').map_or(version, |(_, v)| v),
                        |(v, _)| v,
                    );
                return Some(version.to_string());
            }
        }
        None
    }

    /// Simulate the version extraction from apk policy output as done in query_version_apk.
    fn parse_apk_policy_version(stdout: &str) -> Option<String> {
        if let Some(first_line) = stdout.lines().next() {
            let trimmed = first_line.trim().trim_end_matches(':');
            let bytes = trimmed.as_bytes();
            for i in (0..bytes.len()).rev() {
                if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                    return Some(trimmed[i + 1..].to_string());
                }
            }
        }
        None
    }

    /// Simulate the version extraction and name matching from pkg search output
    /// as done in query_version_pkg.
    fn parse_pkg_search_version(stdout: &str, package: &str) -> Option<String> {
        for line in stdout.lines() {
            let name_ver = line.split_whitespace().next().unwrap_or("");
            let bytes = name_ver.as_bytes();
            for i in (0..bytes.len()).rev() {
                if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                    let name = &name_ver[..i];
                    if name == package {
                        return Some(name_ver[i + 1..].to_string());
                    }
                    break;
                }
            }
        }
        None
    }

    /// Simulate the "Version:" field parsing as done in query_version_info.
    fn parse_version_from_info_output(stdout: &str) -> Option<String> {
        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Version")
                && let Some(version) = rest.trim_start().strip_prefix(':')
            {
                return Some(version.trim().to_string());
            }
        }
        None
    }

    #[test]
    fn test_parse_apt_versions_basic() {
        let output = "curl\t7.88.1\nwget\t1.21.3\ngit\t2.39.0\n";
        let pkgs = parse_apt_versions(output);
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "curl" && p.version == "7.88.1")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "wget" && p.version == "1.21.3")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "git" && p.version == "2.39.0")
        );
    }

    #[test]
    fn test_parse_apt_versions_missing_version() {
        let output = "curl\t7.88.1\nbadpkg\t\n";
        let pkgs = parse_apt_versions(output);
        assert_eq!(pkgs.len(), 2);
        let bad = pkgs.iter().find(|p| p.name == "badpkg").unwrap();
        assert_eq!(bad.version, "unknown");
    }

    #[test]
    fn test_parse_apt_versions_empty() {
        let pkgs = parse_apt_versions("");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_parse_rpm_versions_basic() {
        let output = "bash\t5.1.16\ncoreutils\t8.32\nglibc\t2.35\n";
        let pkgs = parse_rpm_versions(output);
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "bash" && p.version == "5.1.16")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "coreutils" && p.version == "8.32")
        );
    }

    #[test]
    fn test_parse_rpm_versions_empty() {
        let pkgs = parse_rpm_versions("");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_apt_aliases_fd() {
        let aliases = apt_aliases("fd");
        assert_eq!(aliases, vec!["fd-find"]);
    }

    #[test]
    fn test_apt_aliases_bat() {
        let aliases = apt_aliases("bat");
        assert_eq!(aliases, vec!["batcat"]);
    }

    #[test]
    fn test_apt_aliases_nvim() {
        let aliases = apt_aliases("nvim");
        assert_eq!(aliases, vec!["neovim"]);
    }

    #[test]
    fn test_apt_aliases_rg() {
        let aliases = apt_aliases("rg");
        assert_eq!(aliases, vec!["ripgrep"]);
    }

    #[test]
    fn test_apt_aliases_unknown() {
        let aliases = apt_aliases("git");
        assert!(aliases.is_empty());
    }

    #[test]
    fn test_dnf_aliases_fd() {
        let aliases = dnf_aliases("fd");
        assert_eq!(aliases, vec!["fd-find"]);
    }

    #[test]
    fn test_dnf_aliases_nvim() {
        let aliases = dnf_aliases("nvim");
        assert_eq!(aliases, vec!["neovim"]);
    }

    #[test]
    fn test_dnf_aliases_unknown() {
        let aliases = dnf_aliases("curl");
        assert!(aliases.is_empty());
    }

    #[test]
    fn parse_tab_separated_versions_single_column() {
        // Line with no tab separator
        let output = "curl\n";
        let pkgs = parse_tab_separated_versions(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "curl");
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_tab_separated_versions_empty_name() {
        // Tab-only line → empty name → filtered out
        let output = "\t1.0\n";
        let pkgs = parse_tab_separated_versions(output);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_tab_separated_versions_empty_input() {
        let pkgs = parse_tab_separated_versions("");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_tab_separated_versions_multiple_tabs() {
        // Only splits on first tab
        let output = "curl\t7.88.1\textra\n";
        let pkgs = parse_tab_separated_versions(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "curl");
        assert_eq!(pkgs[0].version, "7.88.1\textra");
    }

    #[test]
    fn parse_tab_separated_versions_many_packages() {
        let output = "a\t1.0\nb\t2.0\nc\t3.0\nd\t4.0\ne\t5.0\n";
        let pkgs = parse_tab_separated_versions(output);
        assert_eq!(pkgs.len(), 5);
        assert!(pkgs.iter().any(|p| p.name == "a" && p.version == "1.0"));
        assert!(pkgs.iter().any(|p| p.name == "e" && p.version == "5.0"));
    }

    #[test]
    fn parse_tab_separated_versions_preserves_epoch_in_version() {
        // apt/rpm versions can have epoch: "2:8.2.4328"
        let output = "vim\t2:8.2.4328\ngit\t1:2.39.0\n";
        let pkgs = parse_tab_separated_versions(output);
        assert_eq!(pkgs.len(), 2);
        let vim = pkgs.iter().find(|p| p.name == "vim").unwrap();
        assert_eq!(vim.version, "2:8.2.4328");
        let git = pkgs.iter().find(|p| p.name == "git").unwrap();
        assert_eq!(git.version, "1:2.39.0");
    }

    #[test]
    fn apt_candidate_version_simple() {
        let stdout = "curl:\n  Installed: 7.88.1-10+deb12u1\n  Candidate: 7.88.1-10+deb12u5\n";
        assert_eq!(
            parse_apt_candidate_version(stdout),
            Some("7.88.1".to_string())
        );
    }

    #[test]
    fn apt_candidate_version_with_epoch() {
        let stdout = "vim:\n  Installed: 2:8.2.4328-1\n  Candidate: 2:8.2.4328-2\n";
        // epoch "2:" stripped, then revision "-2" stripped → "8.2.4328"
        assert_eq!(
            parse_apt_candidate_version(stdout),
            Some("8.2.4328".to_string())
        );
    }

    #[test]
    fn apt_candidate_version_plain() {
        let stdout = "nano:\n  Candidate: 7.2\n";
        assert_eq!(parse_apt_candidate_version(stdout), Some("7.2".to_string()));
    }

    #[test]
    fn apt_candidate_version_none() {
        let stdout = "nonexistent:\n  Candidate: (none)\n";
        assert_eq!(parse_apt_candidate_version(stdout), None);
    }

    #[test]
    fn apt_candidate_version_missing_line() {
        let stdout = "curl:\n  Installed: 7.88.1\n";
        assert_eq!(parse_apt_candidate_version(stdout), None);
    }

    #[test]
    fn apt_candidate_version_revision_only_no_epoch() {
        let stdout = "git:\n  Candidate: 2.39.2-1ubuntu1\n";
        assert_eq!(
            parse_apt_candidate_version(stdout),
            Some("2.39.2".to_string())
        );
    }

    #[test]
    fn apk_policy_version_basic() {
        let stdout = "curl-8.5.0-r0:\n  lib/apk/db/installed\n";
        assert_eq!(
            parse_apk_policy_version(stdout),
            Some("8.5.0-r0".to_string())
        );
    }

    #[test]
    fn apk_policy_version_compound_name() {
        let stdout = "alpine-baselayout-3.4.3-r2:\n";
        assert_eq!(
            parse_apk_policy_version(stdout),
            Some("3.4.3-r2".to_string())
        );
    }

    #[test]
    fn apk_policy_version_no_version() {
        let stdout = "busybox:\n";
        assert_eq!(parse_apk_policy_version(stdout), None);
    }

    #[test]
    fn apk_policy_version_empty() {
        let stdout = "";
        assert_eq!(parse_apk_policy_version(stdout), None);
    }

    #[test]
    fn pkg_search_version_matches_exact_name() {
        let stdout = "curl-8.5.0                     Command line tool for transferring data\n";
        assert_eq!(
            parse_pkg_search_version(stdout, "curl"),
            Some("8.5.0".to_string())
        );
    }

    #[test]
    fn pkg_search_version_ignores_partial_name_match() {
        // "curl-lite" is not "curl"
        let stdout = "curl-lite-1.0.0    Lightweight curl\ncurl-8.5.0    Full curl\n";
        assert_eq!(
            parse_pkg_search_version(stdout, "curl"),
            Some("8.5.0".to_string())
        );
    }

    #[test]
    fn pkg_search_version_no_match() {
        let stdout = "wget-1.21.4    GNU Wget\n";
        assert_eq!(parse_pkg_search_version(stdout, "curl"), None);
    }

    #[test]
    fn pkg_search_version_empty_output() {
        assert_eq!(parse_pkg_search_version("", "curl"), None);
    }

    #[test]
    fn info_version_basic() {
        let stdout = "Name        : curl\nVersion     : 8.5.0\nRelease     : 1.fc39\n";
        assert_eq!(
            parse_version_from_info_output(stdout),
            Some("8.5.0".to_string())
        );
    }

    #[test]
    fn info_version_pacman_style() {
        // pacman -Si uses "Version         :" format
        let stdout =
            "Repository      : extra\nName            : vim\nVersion         : 9.0.2167-1\n";
        assert_eq!(
            parse_version_from_info_output(stdout),
            Some("9.0.2167-1".to_string())
        );
    }

    #[test]
    fn info_version_no_version_field() {
        let stdout = "Name: curl\nRelease: 1.fc39\n";
        assert_eq!(parse_version_from_info_output(stdout), None);
    }

    #[test]
    fn info_version_colon_immediately_after() {
        let stdout = "Version:1.2.3\n";
        assert_eq!(
            parse_version_from_info_output(stdout),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn parse_tab_separated_versions_trims_names() {
        let output = " curl \t 7.88.1 \n";
        let pkgs = parse_tab_separated_versions(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "curl");
        assert_eq!(pkgs[0].version, "7.88.1");
    }

    #[test]
    fn apt_aliases_returns_correct_mappings() {
        // Table-driven test covering all known mappings
        let cases = vec![
            ("fd", vec!["fd-find"]),
            ("rg", vec!["ripgrep"]),
            ("bat", vec!["batcat"]),
            ("nvim", vec!["neovim"]),
            ("curl", vec![]),
            ("git", vec![]),
            ("vim", vec![]),
        ];
        for (input, expected) in cases {
            let actual: Vec<String> = apt_aliases(input);
            assert_eq!(actual, expected, "apt_aliases({}) mismatch", input);
        }
    }

    #[test]
    fn dnf_aliases_returns_correct_mappings() {
        let cases = vec![
            ("fd", vec!["fd-find"]),
            ("nvim", vec!["neovim"]),
            ("bat", vec![]), // dnf doesn't alias bat
            ("rg", vec![]),  // dnf doesn't alias rg
            ("curl", vec![]),
        ];
        for (input, expected) in cases {
            let actual: Vec<String> = dnf_aliases(input);
            assert_eq!(actual, expected, "dnf_aliases({}) mismatch", input);
        }
    }
}
