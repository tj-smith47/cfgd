use cfgd_core::output::{CommandOutput, Printer, Verbosity};

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
    let printer = cfgd_core::test_helpers::test_printer();
    // Should not panic
    print_caveats(&printer, &[]);
}

#[test]
fn print_caveats_non_empty() {
    let printer = cfgd_core::test_helpers::test_printer();
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
    let printer = cfgd_core::test_helpers::test_printer();
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
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
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
#[serial_test::serial]
fn brew_available_returns_bool() {
    // Exercises brew_available() production function. Serial-gated because
    // brew_shim tests in packages::brew::tests mutate CFGD_BREW_BIN.
    let _available = brew_available();
    // Just verifying it runs without panic
}

#[test]
#[serial_test::serial]
fn brew_cmd_returns_valid_command() {
    // Serial-gated for the same CFGD_BREW_BIN race.
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

// ---------------------------------------------------------------------------
// Pure-helper coverage for tool_seam_var, resolve_tool_with_fallbacks,
// path_with_brew, brew_path_dirs, print_caveats, sudo_cmd_with_seam,
// linux_system_manager_available, any_system_manager_available.
// ---------------------------------------------------------------------------

#[test]
fn tool_seam_var_uppercases_and_underscores() {
    assert_eq!(tool_seam_var("brew"), "CFGD_BREW_BIN");
    assert_eq!(tool_seam_var("brew-cask"), "CFGD_BREW_CASK_BIN");
    assert_eq!(tool_seam_var("npm"), "CFGD_NPM_BIN");
    assert_eq!(tool_seam_var("nix-env"), "CFGD_NIX_ENV_BIN");
}

#[test]
fn tool_seam_var_already_uppercase_is_idempotent() {
    // Already-uppercase inputs are passed through unchanged.
    assert_eq!(tool_seam_var("BREW"), "CFGD_BREW_BIN");
}

#[test]
#[serial_test::serial]
fn resolve_tool_with_fallbacks_uses_seam_when_set_to_real_file() {
    let dir = tempfile::tempdir().unwrap();
    let shim = dir.path().join("shim");
    std::fs::write(&shim, b"#!/bin/sh\nexit 0\n").unwrap();

    // Snapshot + restore so this test plays nicely with parallel suites.
    let prev = std::env::var_os("CFGD_NEVER_REAL_TOOL_BIN");
    // SAFETY: serial.
    unsafe {
        std::env::set_var("CFGD_NEVER_REAL_TOOL_BIN", &shim);
    }
    let resolved = resolve_tool_with_fallbacks("never-real-tool", &[]);
    // SAFETY: serial.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CFGD_NEVER_REAL_TOOL_BIN", v),
            None => std::env::remove_var("CFGD_NEVER_REAL_TOOL_BIN"),
        }
    }
    assert_eq!(resolved.as_deref(), Some(shim.as_path()));
}

#[test]
#[serial_test::serial]
fn resolve_tool_with_fallbacks_walks_fallback_paths_when_seam_and_path_miss() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    let real = dir.path().join("real-tool");
    std::fs::write(&real, b"#!/bin/sh\nexit 0\n").unwrap();

    // Use a definitely-missing tool name so command_available returns false.
    let prev = std::env::var_os("CFGD_DEFINITELY_NOT_INSTALLED_BIN");
    // SAFETY: serial.
    unsafe {
        std::env::remove_var("CFGD_DEFINITELY_NOT_INSTALLED_BIN");
    }
    let resolved =
        resolve_tool_with_fallbacks("definitely-not-installed", &[missing, real.clone()]);
    // SAFETY: serial.
    unsafe {
        if let Some(v) = prev {
            std::env::set_var("CFGD_DEFINITELY_NOT_INSTALLED_BIN", v);
        }
    }
    assert_eq!(resolved.as_deref(), Some(real.as_path()));
}

#[test]
#[serial_test::serial]
fn resolve_tool_with_fallbacks_returns_none_when_nothing_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let missing1 = dir.path().join("nope1");
    let missing2 = dir.path().join("nope2");

    let prev = std::env::var_os("CFGD_DEFINITELY_NOT_INSTALLED_BIN");
    // SAFETY: serial.
    unsafe {
        std::env::remove_var("CFGD_DEFINITELY_NOT_INSTALLED_BIN");
    }
    let resolved = resolve_tool_with_fallbacks("definitely-not-installed", &[missing1, missing2]);
    // SAFETY: serial.
    unsafe {
        if let Some(v) = prev {
            std::env::set_var("CFGD_DEFINITELY_NOT_INSTALLED_BIN", v);
        }
    }
    assert!(resolved.is_none());
}

#[test]
fn brew_path_dirs_is_non_empty_on_linux_or_macos() {
    let dirs = brew_path_dirs();
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        assert!(!dirs.is_empty(), "expected brew dirs, got: {dirs:?}");
        // Every entry should be an absolute path.
        for d in &dirs {
            assert!(d.starts_with('/'), "non-absolute brew path dir: {d}");
        }
    } else {
        assert!(dirs.is_empty(), "non-linux/non-macos should be empty");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn brew_path_dirs_linux_uses_linuxbrew_paths() {
    let dirs = brew_path_dirs();
    assert!(dirs.iter().any(|d| d.contains("linuxbrew")));
    assert!(dirs.iter().any(|d| d.ends_with("/bin")));
    assert!(dirs.iter().any(|d| d.ends_with("/sbin")));
}

#[test]
fn print_caveats_no_op_when_notes_empty() {
    let (printer, output) = Printer::for_test();
    print_caveats(&printer, &[]);
    let captured = output.lock().unwrap().clone();
    assert!(
        captured.is_empty(),
        "print_caveats should emit nothing for empty notes, got: {captured}"
    );
}

#[test]
fn print_caveats_emits_subheader_then_warning_per_note() {
    let (printer, output) = Printer::for_test_at(Verbosity::Normal);
    let notes = vec![
        PostInstallNote {
            manager: "brew".to_string(),
            message: "run /usr/local/opt/foo/postinstall.sh".to_string(),
        },
        PostInstallNote {
            manager: "npm".to_string(),
            message: "deprecated package warning".to_string(),
        },
    ];
    print_caveats(&printer, &notes);

    let captured = output.lock().unwrap().clone();
    // Subheader
    assert!(
        captured.contains("Post-install notes"),
        "expected subheader, got: {captured}"
    );
    // Each note appears as a warning prefixed with [manager]
    assert!(
        captured.contains("[brew]") && captured.contains("postinstall.sh"),
        "expected brew note, got: {captured}"
    );
    assert!(
        captured.contains("[npm]") && captured.contains("deprecated"),
        "expected npm note, got: {captured}"
    );
}

#[test]
#[serial_test::serial]
fn sudo_cmd_with_seam_uses_seam_path_when_set() {
    let dir = tempfile::tempdir().unwrap();
    let shim = dir.path().join("shim");
    std::fs::write(&shim, b"#!/bin/sh\nexit 0\n").unwrap();

    let prev = std::env::var_os("CFGD_FAKE_SUDO_TOOL_BIN");
    // SAFETY: serial.
    unsafe {
        std::env::set_var("CFGD_FAKE_SUDO_TOOL_BIN", &shim);
    }
    let cmd = sudo_cmd_with_seam("fake-sudo-tool");
    // SAFETY: serial.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CFGD_FAKE_SUDO_TOOL_BIN", v),
            None => std::env::remove_var("CFGD_FAKE_SUDO_TOOL_BIN"),
        }
    }
    let prog = format!("{:?}", cmd.get_program());
    assert!(prog.contains("shim"), "expected seam path, got: {prog}");
    // No "sudo" wrapper when seam is set
    assert!(!prog.starts_with("\"sudo\""), "got: {prog}");
}

#[test]
#[serial_test::serial]
fn sudo_cmd_with_seam_falls_back_to_sudo_cmd_when_unset() {
    let prev = std::env::var_os("CFGD_OTHER_FAKE_TOOL_BIN");
    // SAFETY: serial.
    unsafe {
        std::env::remove_var("CFGD_OTHER_FAKE_TOOL_BIN");
    }
    let cmd = sudo_cmd_with_seam("other-fake-tool");
    // SAFETY: serial.
    unsafe {
        if let Some(v) = prev {
            std::env::set_var("CFGD_OTHER_FAKE_TOOL_BIN", v);
        }
    }
    let prog = format!("{:?}", cmd.get_program());
    // When not root, sudo_cmd prefixes with "sudo"; when root, runs program directly.
    if cfgd_core::is_root() {
        assert!(
            prog.contains("other-fake-tool"),
            "as root expected program directly, got: {prog}"
        );
    } else {
        assert!(prog.contains("sudo"), "expected sudo prefix, got: {prog}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_system_manager_available_returns_bool_without_panic() {
    let _b = linux_system_manager_available();
}

#[test]
fn any_system_manager_available_returns_bool_without_panic() {
    let _b = any_system_manager_available();
}
