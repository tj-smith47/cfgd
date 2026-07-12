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
    for line in stdout.lines() {
        if !line.starts_with(char::is_whitespace) {
            continue;
        }
        let trimmed = line.trim().trim_end_matches(':');
        if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Some(trimmed.to_string());
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
    // Real apk policy output: header line + indented "<version>:" + indented "lib/apk/db/installed".
    let stdout = "\
busybox policy:
  1.36.1-r29:
    lib/apk/db/installed
    https://dl-cdn.alpinelinux.org/alpine/v3.19/main
";
    assert_eq!(
        parse_apk_policy_version(stdout),
        Some("1.36.1-r29".to_string())
    );
}

#[test]
fn apk_policy_version_compound_name() {
    let stdout = "\
alpine-baselayout policy:
  3.4.3-r2:
    lib/apk/db/installed
";
    assert_eq!(
        parse_apk_policy_version(stdout),
        Some("3.4.3-r2".to_string())
    );
}

#[test]
fn apk_policy_version_no_version() {
    let stdout = "nonexistent policy:\n";
    assert_eq!(parse_apk_policy_version(stdout), None);
}

#[test]
fn apk_policy_version_empty() {
    let stdout = "";
    assert_eq!(parse_apk_policy_version(stdout), None);
}

#[test]
fn apk_policy_version_takes_first_indented_version() {
    // Multiple repos may list multiple versions; parser returns the first encountered.
    let stdout = "\
curl policy:
  7.88.1-r0:
    https://dl-cdn.alpinelinux.org/alpine/v3.17/main
  8.5.0-r0:
    https://dl-cdn.alpinelinux.org/alpine/v3.19/main
";
    assert_eq!(
        parse_apk_policy_version(stdout),
        Some("7.88.1-r0".to_string())
    );
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
    let stdout = "Repository      : extra\nName            : vim\nVersion         : 9.0.2167-1\n";
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

#[cfg(unix)]
mod shim_tests {
    use serial_test::serial;

    use cfgd_core::errors::{CfgdError, PackageError};
    use cfgd_core::test_helpers::{EnvVarGuard, ToolShim};

    use super::super::{
        APK_BIN_ENV, APT_CACHE_BIN_ENV, DNF_BIN_ENV, DPKG_QUERY_BIN_ENV, PACMAN_BIN_ENV,
        PKG_BIN_ENV, RPM_BIN_ENV, YUM_BIN_ENV, ZYPPER_BIN_ENV,
    };
    use super::{
        list_apt_with_versions, list_dnf_with_versions, pkg_version_meets_minimum,
        query_version_apk, query_version_apt, query_version_info, query_version_pkg,
    };

    const MISSING_BIN: &str = "/var/empty/cfgd-test-no-such-binary";

    #[test]
    #[serial]
    fn query_version_apt_returns_candidate_via_shim() {
        let _shim = ToolShim::install(
            APT_CACHE_BIN_ENV,
            0,
            "curl:\n  Installed: 7.88.1-10+deb12u1\n  Candidate: 7.88.1-10+deb12u5\n",
            "",
        );
        let version = query_version_apt("apt", "curl").expect("query should succeed");
        assert_eq!(version, Some("7.88.1".to_string()));
    }

    #[test]
    #[serial]
    fn query_version_apt_returns_none_on_non_zero_exit() {
        let _shim = ToolShim::install(APT_CACHE_BIN_ENV, 1, "", "apt-cache failed");
        let version = query_version_apt("apt", "curl").expect("query should not error");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_apt_returns_none_when_candidate_is_none() {
        let _shim = ToolShim::install(
            APT_CACHE_BIN_ENV,
            0,
            "nonexistent:\n  Candidate: (none)\n",
            "",
        );
        let version = query_version_apt("apt", "nonexistent").expect("query should succeed");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_apt_returns_none_when_no_candidate_line() {
        let _shim = ToolShim::install(APT_CACHE_BIN_ENV, 0, "curl:\n  Installed: 7.88.1\n", "");
        let version = query_version_apt("apt", "curl").expect("query should succeed");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_apt_invokes_shim_with_policy_then_package_arg_order() {
        let shim = ToolShim::install(APT_CACHE_BIN_ENV, 0, "curl:\n  Candidate: 1.0\n", "");
        query_version_apt("apt", "curl").expect("query should succeed");
        let log = shim.argv_log();
        assert!(
            log.contains("policy curl"),
            "argv should be `policy curl` in that order; got: {log}"
        );
    }

    #[test]
    #[serial]
    fn query_version_apt_maps_spawn_failure_to_command_failed() {
        let _guard = EnvVarGuard::set(APT_CACHE_BIN_ENV, MISSING_BIN);
        let err = query_version_apt("apt", "curl").unwrap_err();
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::CommandFailed { ref manager, .. }) if manager == "apt"
            ),
            "expected CommandFailed{{manager:\"apt\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn query_version_apk_returns_version_via_shim() {
        let _shim = ToolShim::install(
            APK_BIN_ENV,
            0,
            "curl policy:\n  8.5.0-r0:\n    lib/apk/db/installed\n",
            "",
        );
        let version = query_version_apk("apk", "curl").expect("query should succeed");
        assert_eq!(version, Some("8.5.0-r0".to_string()));
    }

    #[test]
    #[serial]
    fn query_version_apk_returns_none_on_non_zero_exit() {
        let _shim = ToolShim::install(APK_BIN_ENV, 2, "", "apk policy failed");
        let version = query_version_apk("apk", "curl").expect("query should not error");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_apk_returns_none_when_only_header_line() {
        let _shim = ToolShim::install(APK_BIN_ENV, 0, "nonexistent policy:\n", "");
        let version = query_version_apk("apk", "nonexistent").expect("query should succeed");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_apk_maps_spawn_failure_to_command_failed() {
        let _guard = EnvVarGuard::set(APK_BIN_ENV, MISSING_BIN);
        let err = query_version_apk("apk", "curl").unwrap_err();
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::CommandFailed { ref manager, .. }) if manager == "apk"
            ),
            "expected CommandFailed{{manager:\"apk\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn query_version_pkg_returns_version_on_exact_name_match() {
        // `pkg rquery '%n\t%v'` output: name<TAB>version, one line per match.
        let _shim = ToolShim::install(PKG_BIN_ENV, 0, "curl\t8.5.0\n", "");
        let version = query_version_pkg("pkg", "curl").expect("query should succeed");
        assert_eq!(version, Some("8.5.0".to_string()));
    }

    #[test]
    #[serial]
    fn query_version_pkg_returns_none_on_no_match() {
        // A pattern expanding to a sibling package must not leak its version.
        let _shim = ToolShim::install(PKG_BIN_ENV, 0, "wget\t1.21.4\n", "");
        let version = query_version_pkg("pkg", "curl").expect("query should succeed");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_pkg_returns_none_on_non_zero_exit() {
        let _shim = ToolShim::install(PKG_BIN_ENV, 70, "", "pkg rquery failed");
        let version = query_version_pkg("pkg", "curl").expect("query should not error");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_pkg_maps_spawn_failure_to_command_failed() {
        let _guard = EnvVarGuard::set(PKG_BIN_ENV, MISSING_BIN);
        let err = query_version_pkg("pkg", "curl").unwrap_err();
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::CommandFailed { ref manager, .. }) if manager == "pkg"
            ),
            "expected CommandFailed{{manager:\"pkg\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn pkg_version_meets_minimum_true_when_greater_or_equal() {
        // `pkg version -t <available> <min>` prints `>` when available exceeds the floor.
        let _shim = ToolShim::install(PKG_BIN_ENV, 0, ">\n", "");
        assert!(pkg_version_meets_minimum("1.2.0,1", "1.0").expect("compare should succeed"));
    }

    #[test]
    #[serial]
    fn pkg_version_meets_minimum_true_on_exact_equal() {
        let _shim = ToolShim::install(PKG_BIN_ENV, 0, "=\n", "");
        assert!(pkg_version_meets_minimum("1.0", "1.0").expect("compare should succeed"));
    }

    #[test]
    #[serial]
    fn pkg_version_meets_minimum_false_when_below_floor() {
        let _shim = ToolShim::install(PKG_BIN_ENV, 0, "<\n", "");
        assert!(!pkg_version_meets_minimum("0.9", "1.0").expect("compare should succeed"));
    }

    #[test]
    #[serial]
    fn pkg_version_meets_minimum_false_on_non_zero_exit() {
        // A malformed comparison fails closed (not satisfied), never a false pass.
        let _shim = ToolShim::install(PKG_BIN_ENV, 1, "", "pkg version failed");
        assert!(!pkg_version_meets_minimum("bogus", "1.0").expect("should not error"));
    }

    #[test]
    #[serial]
    fn pkg_version_meets_minimum_maps_spawn_failure_to_command_failed() {
        let _guard = EnvVarGuard::set(PKG_BIN_ENV, MISSING_BIN);
        let err = pkg_version_meets_minimum("1.0", "1.0").unwrap_err();
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::CommandFailed { ref manager, .. }) if manager == "pkg"
            ),
            "expected CommandFailed{{manager:\"pkg\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn query_version_info_pacman_returns_version_and_invokes_shim_with_si_then_pkg() {
        let shim = ToolShim::install(
            PACMAN_BIN_ENV,
            0,
            "Repository      : extra\nName            : vim\nVersion         : 9.0.2167-1\n",
            "",
        );
        let version = query_version_info("pacman", "vim").expect("query should succeed");
        assert_eq!(version, Some("9.0.2167-1".to_string()));
        let log = shim.argv_log();
        assert!(
            log.contains("-Si vim"),
            "pacman argv should be `-Si vim` in that order; got: {log}"
        );
    }

    #[test]
    #[serial]
    fn query_version_info_dnf_returns_version_and_invokes_shim_with_info_then_pkg() {
        let shim = ToolShim::install(
            DNF_BIN_ENV,
            0,
            "Name        : curl\nVersion     : 8.5.0\nRelease     : 1.fc39\n",
            "",
        );
        let version = query_version_info("dnf", "curl").expect("query should succeed");
        assert_eq!(version, Some("8.5.0".to_string()));
        let log = shim.argv_log();
        assert!(
            log.contains("info curl"),
            "dnf argv should be `info curl` in that order; got: {log}"
        );
    }

    #[test]
    #[serial]
    fn query_version_info_yum_returns_version_via_shim() {
        let _shim = ToolShim::install(
            YUM_BIN_ENV,
            0,
            "Name        : bash\nVersion     : 4.4.20\n",
            "",
        );
        let version = query_version_info("yum", "bash").expect("query should succeed");
        assert_eq!(version, Some("4.4.20".to_string()));
    }

    #[test]
    #[serial]
    fn query_version_info_zypper_returns_version_via_shim() {
        let _shim = ToolShim::install(
            ZYPPER_BIN_ENV,
            0,
            "Information for package wget:\nVersion        : 1.20.3\n",
            "",
        );
        let version = query_version_info("zypper", "wget").expect("query should succeed");
        assert_eq!(version, Some("1.20.3".to_string()));
    }

    #[test]
    #[serial]
    fn query_version_info_returns_none_on_non_zero_exit() {
        let _shim = ToolShim::install(DNF_BIN_ENV, 1, "", "dnf info failed");
        let version = query_version_info("dnf", "curl").expect("query should not error");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_info_returns_none_when_version_field_missing() {
        let _shim = ToolShim::install(DNF_BIN_ENV, 0, "Name: curl\nRelease: 1.fc39\n", "");
        let version = query_version_info("dnf", "curl").expect("query should succeed");
        assert_eq!(version, None);
    }

    #[test]
    #[serial]
    fn query_version_info_maps_spawn_failure_to_command_failed() {
        let _guard = EnvVarGuard::set(DNF_BIN_ENV, MISSING_BIN);
        let err = query_version_info("dnf", "curl").unwrap_err();
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::CommandFailed { ref manager, .. }) if manager == "dnf"
            ),
            "expected CommandFailed{{manager:\"dnf\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn list_apt_with_versions_parses_dpkg_query_output() {
        let _shim = ToolShim::install(
            DPKG_QUERY_BIN_ENV,
            0,
            "curl\t7.88.1\nwget\t1.21.3\ngit\t2.39.0\n",
            "",
        );
        let pkgs = list_apt_with_versions("apt").expect("list should succeed");
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
    #[serial]
    fn list_apt_with_versions_surfaces_stderr_on_non_zero_exit() {
        let _shim = ToolShim::install(DPKG_QUERY_BIN_ENV, 1, "", "dpkg-query failed");
        let err = list_apt_with_versions("apt").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("dpkg-query failed"),
            "error should include stderr; got: {msg}"
        );
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::ListFailed { ref manager, .. }) if manager == "apt"
            ),
            "expected ListFailed{{manager:\"apt\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn list_apt_with_versions_maps_spawn_failure_to_command_failed() {
        let _guard = EnvVarGuard::set(DPKG_QUERY_BIN_ENV, MISSING_BIN);
        let err = list_apt_with_versions("apt").unwrap_err();
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::CommandFailed { ref manager, .. }) if manager == "apt"
            ),
            "expected CommandFailed{{manager:\"apt\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn list_dnf_with_versions_parses_rpm_output() {
        let _shim = ToolShim::install(
            RPM_BIN_ENV,
            0,
            "bash\t5.1.16\ncoreutils\t8.32\nglibc\t2.35\n",
            "",
        );
        let pkgs = list_dnf_with_versions("dnf").expect("list should succeed");
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "bash" && p.version == "5.1.16")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "coreutils" && p.version == "8.32")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "glibc" && p.version == "2.35")
        );
    }

    #[test]
    #[serial]
    fn list_dnf_with_versions_surfaces_stderr_on_non_zero_exit() {
        let _shim = ToolShim::install(RPM_BIN_ENV, 1, "", "rpm query failed");
        let err = list_dnf_with_versions("dnf").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("rpm query failed"),
            "error should include stderr; got: {msg}"
        );
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::ListFailed { ref manager, .. }) if manager == "dnf"
            ),
            "expected ListFailed{{manager:\"dnf\"}}; got: {err:?}"
        );
    }

    #[test]
    #[serial]
    fn list_dnf_with_versions_maps_spawn_failure_to_command_failed() {
        let _guard = EnvVarGuard::set(RPM_BIN_ENV, MISSING_BIN);
        let err = list_dnf_with_versions("dnf").unwrap_err();
        assert!(
            matches!(
                err,
                CfgdError::Package(PackageError::CommandFailed { ref manager, .. }) if manager == "dnf"
            ),
            "expected CommandFailed{{manager:\"dnf\"}}; got: {err:?}"
        );
    }
}
