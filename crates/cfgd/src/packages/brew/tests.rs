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
    let printer = cfgd_core::test_helpers::test_printer_v2();
    mgr.bootstrap(&printer).unwrap();
}

#[test]
fn brew_cask_manager_name_and_bootstrap() {
    let mgr = BrewCaskManager;
    assert_eq!(mgr.name(), "brew-cask");
    assert!(!mgr.can_bootstrap());
    let printer = cfgd_core::test_helpers::test_printer_v2();
    mgr.bootstrap(&printer).unwrap();
}

#[test]
fn brew_tap_manager_update_is_noop() {
    let mgr = BrewTapManager;
    let printer = cfgd_core::test_helpers::test_printer_v2();
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
    let printer = cfgd_core::test_helpers::test_printer_v2();
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
    let printer = cfgd_core::test_helpers::test_printer_v2();
    // brew-cask update is a no-op, should always succeed
    mgr.update(&printer).unwrap();
}

#[test]
fn brew_tap_available_version_always_none() {
    let mgr = BrewTapManager;
    let result = mgr.available_version("homebrew/core").unwrap();
    assert!(result.is_none());
}

// --- parse_brew_list_set ---

#[test]
fn parse_brew_list_set_collects_distinct_lines() {
    let stdout = "git\nneovim\nripgrep\n";
    let set = parse_brew_list_set(stdout);
    assert_eq!(set.len(), 3);
    assert!(set.contains("git"));
    assert!(set.contains("neovim"));
    assert!(set.contains("ripgrep"));
}

#[test]
fn parse_brew_list_set_drops_blank_and_whitespace_lines() {
    let stdout = "\ngit\n   \n\nneovim\n   \t  \n";
    let set = parse_brew_list_set(stdout);
    assert_eq!(set.len(), 2, "blank/whitespace lines must be dropped");
    assert!(set.contains("git"));
    assert!(set.contains("neovim"));
}

#[test]
fn parse_brew_list_set_trims_each_line() {
    let stdout = "  git  \n\thomebrew/core\nuser/tap\n";
    let set = parse_brew_list_set(stdout);
    assert_eq!(set.len(), 3);
    // Surrounding whitespace must be stripped so the set lookup matches the
    // raw name brew users see — otherwise diffing installed vs desired would
    // wrongly report drift on whitespace alone.
    assert!(set.contains("git"));
    assert!(set.contains("homebrew/core"));
}

#[test]
fn parse_brew_list_set_dedupes_duplicates() {
    // brew list never repeats, but if it ever did, HashSet semantics dedupe.
    let stdout = "git\ngit\ngit\n";
    let set = parse_brew_list_set(stdout);
    assert_eq!(set.len(), 1);
}

#[test]
fn parse_brew_list_set_empty_input_returns_empty_set() {
    assert!(parse_brew_list_set("").is_empty());
}

// --- parse_brew_info_version ---

#[test]
fn parse_brew_info_version_extracts_formula_stable() {
    let stdout =
        r#"{"formulae":[{"name":"git","versions":{"stable":"2.43.0","head":"HEAD"}}],"casks":[]}"#;
    let v = parse_brew_info_version(stdout, "/formulae/0/versions/stable", "brew").unwrap();
    assert_eq!(v.as_deref(), Some("2.43.0"));
}

#[test]
fn parse_brew_info_version_extracts_cask_version() {
    let stdout = r#"{"formulae":[],"casks":[{"token":"firefox","version":"123.0.1"}]}"#;
    let v = parse_brew_info_version(stdout, "/casks/0/version", "brew-cask").unwrap();
    assert_eq!(v.as_deref(), Some("123.0.1"));
}

#[test]
fn parse_brew_info_version_returns_none_when_pointer_missing() {
    // Empty arrays — brew returns this when the package is unknown.
    let stdout = r#"{"formulae":[],"casks":[]}"#;
    let v = parse_brew_info_version(stdout, "/formulae/0/versions/stable", "brew").unwrap();
    assert!(v.is_none(), "missing pointer must yield None, not error");
}

#[test]
fn parse_brew_info_version_returns_none_when_value_is_null() {
    // Field present but JSON null — must not panic, must not return "null".
    let stdout = r#"{"formulae":[{"versions":{"stable":null}}]}"#;
    let v = parse_brew_info_version(stdout, "/formulae/0/versions/stable", "brew").unwrap();
    assert!(v.is_none());
}

#[test]
fn parse_brew_info_version_returns_none_when_value_is_non_string() {
    // Some brew versions surface integers — must not stringify them silently.
    let stdout = r#"{"formulae":[{"versions":{"stable":42}}]}"#;
    let v = parse_brew_info_version(stdout, "/formulae/0/versions/stable", "brew").unwrap();
    assert!(v.is_none());
}

#[test]
fn parse_brew_info_version_errors_on_invalid_json() {
    let err = parse_brew_info_version("not json at all", "/anything", "brew")
        .expect_err("invalid JSON must surface as PackageError::ListFailed");
    let msg = err.to_string();
    assert!(
        msg.contains("brew") && msg.contains("failed to parse brew info output"),
        "error must name the brew manager + parse-failure context, got: {msg}"
    );
}

#[test]
fn parse_brew_info_version_errors_attribute_correct_manager() {
    // brew-cask path should attribute the error to brew-cask, not brew.
    let err = parse_brew_info_version("garbage", "/casks/0/version", "brew-cask")
        .expect_err("invalid JSON must error");
    let msg = err.to_string();
    assert!(
        msg.contains("brew-cask"),
        "error must attribute to brew-cask manager, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// PackageManager-impl tests via the CFGD_BREW_BIN ToolShim. Drive every
// install / uninstall / update / list / available_version branch without a
// real Homebrew install on the runner.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod brew_shim {
    use super::*;
    use cfgd_core::providers::PackageManager;
    use cfgd_core::test_helpers::{ToolShim, test_printer_v2};
    use serial_test::serial;

    const SHIM_ENV: &str = "CFGD_BREW_BIN";

    // --- BrewManager (formulae) ---

    #[test]
    #[serial]
    fn brew_install_passes_install_subcommand_with_each_package() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewManager
            .install(&["git".into(), "vim".into()], &p)
            .expect("install Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains("install git vim"),
            "argv must include install + packages: {argv}"
        );
    }

    #[test]
    #[serial]
    fn brew_install_skips_command_when_package_list_empty() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewManager.install(&[], &p).expect("empty install Ok");
        assert_eq!(
            shim.invocation_count(),
            0,
            "empty package list must not spawn brew at all"
        );
    }

    #[test]
    #[serial]
    fn brew_uninstall_passes_uninstall_subcommand_with_each_package() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewManager
            .uninstall(&["git".into()], &p)
            .expect("uninstall Ok");
        assert!(shim.argv_log().contains("uninstall git"));
    }

    #[test]
    #[serial]
    fn brew_uninstall_skips_command_when_package_list_empty() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewManager.uninstall(&[], &p).expect("empty uninstall Ok");
        assert_eq!(shim.invocation_count(), 0);
    }

    #[test]
    #[serial]
    fn brew_update_runs_update_subcommand() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewManager.update(&p).expect("update Ok");
        assert!(shim.argv_log().contains("update"));
    }

    #[test]
    #[serial]
    fn brew_install_translates_nonzero_exit_into_install_failed() {
        let _shim = ToolShim::install(SHIM_ENV, 1, "", "Error: package not found");
        let p = test_printer_v2();
        let err = BrewManager
            .install(&["nonexistent".into()], &p)
            .expect_err("non-zero → Err");
        let msg = err.to_string();
        assert!(
            msg.contains("brew") && msg.contains("install"),
            "error must be tagged as a brew install failure: {msg}"
        );
    }

    #[test]
    #[serial]
    fn brew_installed_packages_parses_newline_list_into_set() {
        let _shim = ToolShim::install(SHIM_ENV, 0, "git\nvim\n\nripgrep\n", "");
        let installed = BrewManager
            .installed_packages()
            .expect("installed_packages Ok");
        assert_eq!(installed.len(), 3, "3 non-empty lines: {:?}", installed);
        assert!(installed.contains("git"));
        assert!(installed.contains("vim"));
        assert!(installed.contains("ripgrep"));
    }

    #[test]
    #[serial]
    fn brew_available_version_extracts_stable_from_json_pointer() {
        let json = r#"{"formulae":[{"versions":{"stable":"2.40.1"}}]}"#;
        let _shim = ToolShim::install(SHIM_ENV, 0, json, "");
        let v = BrewManager
            .available_version("git")
            .expect("available_version Ok");
        assert_eq!(v.as_deref(), Some("2.40.1"));
    }

    #[test]
    #[serial]
    fn brew_available_version_returns_none_on_nonzero_exit() {
        let _shim = ToolShim::install(SHIM_ENV, 1, "", "no such formula");
        let v = BrewManager
            .available_version("nonexistent")
            .expect("non-zero exit returns Ok(None), not Err");
        assert_eq!(v, None);
    }

    #[test]
    #[serial]
    fn brew_installed_packages_with_versions_uses_last_token_as_version() {
        // brew list --versions output: "name v1 v2" → version = last token.
        let _shim = ToolShim::install(SHIM_ENV, 0, "git 2.40.1\nvim 9.0.1234\n", "");
        let pkgs = BrewManager.installed_packages_with_versions().expect("Ok");
        let git = pkgs.iter().find(|p| p.name == "git").expect("git present");
        assert_eq!(git.version, "2.40.1");
    }

    // --- BrewTapManager ---

    #[test]
    #[serial]
    fn brew_tap_install_runs_one_tap_subcommand_per_entry() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewTapManager
            .install(&["org/foo".into(), "org/bar".into()], &p)
            .expect("Ok");
        assert_eq!(shim.invocation_count(), 2, "one brew invocation per tap");
        let argv = shim.argv_log();
        assert!(argv.contains("tap org/foo"));
        assert!(argv.contains("tap org/bar"));
    }

    #[test]
    #[serial]
    fn brew_tap_uninstall_runs_one_untap_subcommand_per_entry() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewTapManager
            .uninstall(&["org/foo".into()], &p)
            .expect("Ok");
        assert!(shim.argv_log().contains("untap org/foo"));
    }

    #[test]
    #[serial]
    fn brew_tap_update_is_noop_no_command_spawned() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewTapManager.update(&p).expect("Ok");
        assert_eq!(
            shim.invocation_count(),
            0,
            "tap update is documented no-op (taps are repos, not versioned packages)"
        );
    }

    #[test]
    #[serial]
    fn brew_tap_available_version_always_returns_none() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let v = BrewTapManager.available_version("anything").expect("Ok");
        assert_eq!(v, None, "taps don't have versions");
        assert_eq!(
            shim.invocation_count(),
            0,
            "available_version on a tap must not spawn brew"
        );
    }

    // --- BrewCaskManager ---

    #[test]
    #[serial]
    fn brew_cask_install_passes_cask_flag_with_packages() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewCaskManager
            .install(&["firefox".into(), "vlc".into()], &p)
            .expect("Ok");
        let argv = shim.argv_log();
        assert!(argv.contains("install --cask firefox vlc"), "argv: {argv}");
    }

    #[test]
    #[serial]
    fn brew_cask_uninstall_passes_cask_flag_with_packages() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewCaskManager
            .uninstall(&["firefox".into()], &p)
            .expect("Ok");
        assert!(shim.argv_log().contains("uninstall --cask firefox"));
    }

    #[test]
    #[serial]
    fn brew_cask_install_skips_command_when_empty() {
        let shim = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer_v2();
        BrewCaskManager.install(&[], &p).expect("Ok");
        assert_eq!(shim.invocation_count(), 0);
    }

    #[test]
    #[serial]
    fn brew_cask_available_version_extracts_from_casks_json_pointer() {
        let json = r#"{"casks":[{"version":"117.0.1"}]}"#;
        let _shim = ToolShim::install(SHIM_ENV, 0, json, "");
        let v = BrewCaskManager.available_version("firefox").expect("Ok");
        assert_eq!(v.as_deref(), Some("117.0.1"));
    }
}

#[cfg(unix)]
mod bridge {
    use super::*;
    use cfgd_core::output::test_capture::{assert_snapshot_at, strip_ansi, strip_spinner_duration};
    use cfgd_core::output::{Doc, Printer as PrinterV2, Role};
    use cfgd_core::providers::PackageManager;
    use cfgd_core::test_helpers::ToolShim;
    use serial_test::serial;

    const SHIM_ENV: &str = "CFGD_BREW_BIN";

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/packages/brew/snapshots")
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    #[derive(serde::Serialize)]
    struct BrewInstallSummary {
        packages_installed: usize,
    }

    #[test]
    #[serial]
    fn snapshot_brew_install_clean() {
        let _shim = ToolShim::install(SHIM_ENV, 0, "", "");

        let (printer, cap) = PrinterV2::for_test_doc();
        BrewManager
            .install(&["git".to_string()], &printer)
            .expect("install ok");

        let summary = BrewInstallSummary {
            packages_installed: 1,
        };
        let doc = Doc::new()
            .status(Role::Ok, "brew packages installed")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let raw = strip_ansi(&cap.human());
        let captured = strip_spinner_duration(raw);

        assert!(
            captured.contains("\n\n"),
            "brew_install_clean missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "brew_install_clean has duplicate blank line:\n{captured}"
        );

        assert_snapshot("brew_install_clean.txt", &captured);
    }

    #[test]
    #[serial]
    fn snapshot_brew_install_with_warnings() {
        // Shim emits brew-style caveats so extract_caveats + print_caveats fire.
        // Caveat body must be a single line — renderer forbids embedded newlines.
        let caveat_stdout = "==> Installing git\n\
            ==> Caveats\n\
            Run xcode-select --install to complete setup.\n\
            ==> Summary\n\
            git installed.\n";
        let _shim = ToolShim::install(SHIM_ENV, 0, caveat_stdout, "");

        let (printer, cap) = PrinterV2::for_test_doc();
        BrewManager
            .install(&["git".to_string()], &printer)
            .expect("install ok with caveats");

        let summary = BrewInstallSummary {
            packages_installed: 1,
        };
        let doc = Doc::new()
            .status(Role::Warn, "brew packages installed with notes")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let raw = strip_ansi(&cap.human());
        let captured = strip_spinner_duration(raw);

        assert!(
            captured.contains("\n\n"),
            "brew_install_with_warnings missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "brew_install_with_warnings has duplicate blank line:\n{captured}"
        );

        assert_snapshot("brew_install_with_warnings.txt", &captured);
    }
}
