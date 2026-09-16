#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! The retired `cfgd status` spellings, driven through the real binary.
//!
//! A removed flag clap no longer declares gets the bare "unexpected argument",
//! which tells a script's author nothing about where the display went. Both
//! spellings stay declared and hidden, and the refusal names the command that
//! does the job now. Only the real binary shows stderr and the exit code
//! exactly as the script that still passes the flag sees them.

use assert_cmd::Command;

fn refusal(flag: &str) -> (Option<i32>, String) {
    let out = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["status", flag])
        .output()
        .unwrap_or_else(|e| panic!("run cfgd status {flag}: {e}"));
    (
        out.status.code(),
        String::from_utf8(out.stderr).expect("utf-8 stderr"),
    )
}

#[test]
fn status_show_scripts_is_refused_with_the_command_that_lists_scripts() {
    for flag in ["-s", "--show-scripts"] {
        let (code, stderr) = refusal(flag);
        assert_eq!(
            code,
            Some(1),
            "`cfgd status {flag}` must be refused, not run: {stderr}"
        );
        assert!(
            stderr.contains("cfgd module show"),
            "`cfgd status {flag}` must name the command that lists scripts: {stderr}"
        );
    }
}

#[test]
fn status_show_all_is_refused_with_the_wide_output_flag() {
    for flag in ["-a", "--show-all"] {
        let (code, stderr) = refusal(flag);
        assert_eq!(
            code,
            Some(1),
            "`cfgd status {flag}` must be refused, not run: {stderr}"
        );
        assert!(
            stderr.contains("cfgd status -o wide"),
            "`cfgd status {flag}` must name the view that replaced it: {stderr}"
        );
    }
}

#[test]
fn a_retired_status_flag_is_hidden_from_the_help() {
    let out = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["status", "--help"])
        .output()
        .expect("run cfgd status --help");
    let help = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for flag in ["--show-scripts", "--show-all"] {
        assert!(
            !help.contains(flag),
            "`{flag}` is retired, so it must not be offered in the help: {help}"
        );
    }
}
