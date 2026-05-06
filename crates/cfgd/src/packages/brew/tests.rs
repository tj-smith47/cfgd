use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::*;

#[test]
fn test_parse_brew_versions_basic() {
    let output = "git 2.43.0\nneovim 0.9.5\nripgrep 14.1.0\n";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 3);
    assert!(
        pkgs.iter()
            .any(|p| p.name == "git" && p.version == "2.43.0")
    );
    assert!(
        pkgs.iter()
            .any(|p| p.name == "neovim" && p.version == "0.9.5")
    );
    assert!(
        pkgs.iter()
            .any(|p| p.name == "ripgrep" && p.version == "14.1.0")
    );
}

#[test]
fn test_parse_brew_versions_multi_version() {
    // brew list --versions can show multiple versions for some packages
    let output = "python@3.11 3.11.0 3.11.1\nfd 9.0.0\n";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 2);
    // Multi-version: take the last token
    assert!(
        pkgs.iter()
            .any(|p| p.name == "python@3.11" && p.version == "3.11.1")
    );
    assert!(pkgs.iter().any(|p| p.name == "fd" && p.version == "9.0.0"));
}

#[test]
fn test_parse_brew_versions_empty() {
    let pkgs = parse_brew_versions("");
    assert!(pkgs.is_empty());
}

#[test]
fn test_parse_brew_versions_blank_lines() {
    let output = "\ngit 2.43.0\n\n";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, "git");
}

#[test]
fn parse_brew_versions_name_only_no_version() {
    // A line with only a name and no version token
    let output = "somepackage\n";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, "somepackage");
    assert_eq!(pkgs[0].version, "unknown");
}

#[test]
fn parse_brew_versions_whitespace_only() {
    let output = "   \n  \n\n";
    let pkgs = parse_brew_versions(output);
    assert!(pkgs.is_empty());
}

#[test]
fn brew_manager_name_and_bootstrap() {
    let mgr = BrewManager;
    assert_eq!(mgr.name(), "brew");
    assert!(mgr.can_bootstrap());
}

#[test]
fn brew_tap_manager_name_and_bootstrap() {
    let mgr = BrewTapManager;
    assert_eq!(mgr.name(), "brew-tap");
    assert!(!mgr.can_bootstrap());
    // bootstrap is a no-op
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.bootstrap(&printer).unwrap();
}

#[test]
fn brew_cask_manager_name_and_bootstrap() {
    let mgr = BrewCaskManager;
    assert_eq!(mgr.name(), "brew-cask");
    assert!(!mgr.can_bootstrap());
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.bootstrap(&printer).unwrap();
}

#[test]
fn brew_tap_manager_update_is_noop() {
    let mgr = BrewTapManager;
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.update(&printer).unwrap();
}

#[test]
fn brew_tap_manager_available_version_is_none() {
    let mgr = BrewTapManager;
    assert!(mgr.available_version("any").unwrap().is_none());
}

#[test]
fn brew_cask_manager_update_is_noop() {
    let mgr = BrewCaskManager;
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.update(&printer).unwrap();
}

#[test]
fn brew_manager_path_dirs_returns_vec() {
    let mgr = BrewManager;
    let dirs = mgr.path_dirs();
    // On Linux CI, should return linuxbrew paths
    // On macOS, should return /opt/homebrew or /usr/local paths
    // On Windows, should return empty
    if cfg!(target_os = "windows") {
        assert!(dirs.is_empty());
    } else if cfg!(target_os = "linux") {
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].contains("linuxbrew"));
    }
}

#[test]
fn parse_brew_versions_with_leading_trailing_whitespace() {
    let output = "  git 2.43.0  \n  fd 9.0.0  \n";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 2);
    assert!(pkgs.iter().any(|p| p.name == "git"));
    assert!(pkgs.iter().any(|p| p.name == "fd"));
}

#[test]
fn parse_brew_versions_single_package_no_trailing_newline() {
    let output = "ripgrep 14.1.0";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, "ripgrep");
    assert_eq!(pkgs[0].version, "14.1.0");
}

#[test]
fn brew_manager_path_dirs_non_empty_on_unix() {
    if cfg!(unix) {
        let mgr = BrewManager;
        let dirs = mgr.path_dirs();
        assert!(!dirs.is_empty());
    }
}

#[test]
fn parse_brew_versions_returns_sorted_stable_output() {
    let output = "zsh 5.9\nabc 1.0\nmno 2.0\n";
    let pkgs = parse_brew_versions(output);
    // Should maintain input order
    assert_eq!(pkgs[0].name, "zsh");
    assert_eq!(pkgs[1].name, "abc");
    assert_eq!(pkgs[2].name, "mno");
}

#[test]
fn parse_brew_versions_scoped_package_names() {
    let output = "python@3.11 3.11.7\nnode@20 20.10.0\nopenjdk@17 17.0.9\n";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 3);
    assert!(
        pkgs.iter()
            .any(|p| p.name == "python@3.11" && p.version == "3.11.7")
    );
    assert!(
        pkgs.iter()
            .any(|p| p.name == "node@20" && p.version == "20.10.0")
    );
    assert!(
        pkgs.iter()
            .any(|p| p.name == "openjdk@17" && p.version == "17.0.9")
    );
}

#[test]
fn parse_brew_versions_with_tab_separator() {
    // brew normally uses spaces, but test robustness
    let output = "git\t2.43.0\n";
    let pkgs = parse_brew_versions(output);
    // splitn(2, ' ') won't split on tab, so tab is part of the name
    // This documents the actual behavior: no version extracted
    assert_eq!(pkgs.len(), 1);
    // The whole "git\t2.43.0" is the name since split on space fails
    assert_eq!(pkgs[0].version, "unknown");
}

#[test]
fn parse_brew_versions_three_versions_takes_last() {
    let output = "python@3.12 3.12.0 3.12.1 3.12.2\n";
    let pkgs = parse_brew_versions(output);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, "python@3.12");
    // split_whitespace().last() should give "3.12.2"
    assert_eq!(pkgs[0].version, "3.12.2");
}

#[test]
fn brew_manager_is_available_checks_brew() {
    let mgr = BrewManager;
    let available = mgr.is_available();
    assert_eq!(available, brew_available());
}

#[test]
fn brew_tap_manager_is_available_checks_brew() {
    let mgr = BrewTapManager;
    let available = mgr.is_available();
    assert_eq!(available, brew_available());
}

#[test]
fn brew_cask_manager_is_available_checks_brew() {
    let mgr = BrewCaskManager;
    let available = mgr.is_available();
    assert_eq!(available, brew_available());
}

#[test]
fn brew_cask_update_returns_ok() {
    let mgr = BrewCaskManager;
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    // brew-cask update is a no-op, should always succeed
    mgr.update(&printer).unwrap();
}

#[test]
fn brew_tap_available_version_always_none() {
    let mgr = BrewTapManager;
    let result = mgr.available_version("homebrew/core").unwrap();
    assert!(result.is_none());
}
