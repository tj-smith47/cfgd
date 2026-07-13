use super::*;
use crate::config::EnvVar;

fn fake_env_var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: value.to_string(),
    }
}

fn fake_config_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/config")
}

// build_module_script_env: module spec.env vars appear in the output.
#[test]
fn module_env_vars_propagated_to_script_env() {
    let module_env = vec![
        fake_env_var("PATH", "/custom/bin"),
        fake_env_var("GOPATH", "/foo"),
    ];
    let env = build_module_script_env(
        &fake_config_dir(),
        "workstation",
        ReconcileContext::Apply,
        &ScriptPhase::PostApply,
        Some("nvim"),
        None,
        &module_env,
    );

    let lookup =
        |key: &str| -> Option<&str> { env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()) };

    assert_eq!(lookup("PATH"), Some("/custom/bin"));
    assert_eq!(lookup("GOPATH"), Some("/foo"));
    // Runtime metadata is still present.
    assert_eq!(lookup("CFGD_MODULE_NAME"), Some("nvim"));
    assert_eq!(lookup("CFGD_PROFILE"), Some("workstation"));
    assert_eq!(lookup("CFGD_PHASE"), Some("postApply"));
}

// build_module_script_env: `$VAR`/`${VAR}` in declared values are expanded
// against the process env + earlier vars, so `PATH: ...:$PATH` resolves to a
// real PATH instead of landing literally and breaking interpreter spawn.
#[test]
#[serial_test::serial]
fn module_env_values_expand_dollar_refs() {
    let _g = crate::test_helpers::EnvVarGuard::set("S84_EXPAND_BASE", "/opt/base");
    let module_env = vec![
        fake_env_var("PATH", "/custom/bin:$S84_EXPAND_BASE"),
        // fold-left: a later var resolves one declared earlier in spec.env.
        fake_env_var("FOO", "/x"),
        fake_env_var("BAR", "$FOO/y"),
        // an unset reference expands to empty, like a shell.
        fake_env_var("BAZ", "a${S84_DEFINITELY_UNSET}b"),
    ];
    let env = build_module_script_env(
        &fake_config_dir(),
        "workstation",
        ReconcileContext::Apply,
        &ScriptPhase::PostApply,
        Some("nvim"),
        None,
        &module_env,
    );
    let lookup = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    assert_eq!(lookup("PATH"), Some("/custom/bin:/opt/base"));
    assert_eq!(lookup("BAR"), Some("/x/y"));
    assert_eq!(lookup("BAZ"), Some("ab"));
}

// build_module_script_env: a leading `~` in a declared value expands to the
// user's home BEFORE `$VAR` expansion. Without this, `CLIFT_DIR=~/.local/...`
// would be injected literally and every consumer of the path would break.
#[test]
#[serial_test::serial]
fn module_env_values_expand_leading_tilde() {
    let home = tempfile::tempdir().unwrap();
    crate::with_test_home(home.path(), || {
        let module_env = vec![
            fake_env_var("CLIFT_DIR", "~/.local/share/clift"),
            // tilde after a `:` (PATH-style) also expands, leading segment too.
            fake_env_var("MIXED", "~/bin:/usr/bin:~/x"),
        ];
        let env = build_module_script_env(
            &fake_config_dir(),
            "workstation",
            ReconcileContext::Apply,
            &ScriptPhase::PostApply,
            Some("clift"),
            None,
            &module_env,
        );
        let lookup = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        // Env-file/injected values fold home to posix (the production contract):
        // build the expectation through the same central API so Windows matches.
        let h = home.path().posix().to_string();
        assert_eq!(
            lookup("CLIFT_DIR"),
            Some(format!("{h}/.local/share/clift").as_str())
        );
        assert_eq!(
            lookup("MIXED"),
            Some(format!("{h}/bin:/usr/bin:{h}/x").as_str())
        );
    });
}

// script_default_workdir: the home directory is the default CWD for every
// lifecycle script — NOT the config source tree — so a relative write can't
// pollute the user's GitOps repo (finding E).
#[test]
#[serial_test::serial]
fn script_default_workdir_is_home_not_config() {
    let home = tempfile::tempdir().unwrap();
    crate::with_test_home(home.path(), || {
        let wd = script_default_workdir(&fake_config_dir());
        assert_eq!(wd, home.path());
        assert_ne!(wd, fake_config_dir());
    });
}

// execute_script: a per-script `workdir:` overrides the caller-provided CWD;
// a relative write lands in the override dir, not the default.
#[cfg(unix)]
#[test]
fn execute_script_workdir_override_absolute() {
    let printer = crate::test_helpers::test_printer();
    let default_dir = tempfile::tempdir().unwrap();
    let override_dir = tempfile::tempdir().unwrap();
    let entry = ScriptEntry::Full {
        run: "touch ran.marker".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
        workdir: Some(override_dir.path().display().to_string()),
    };
    execute_script(
        &entry,
        default_dir.path(),
        default_dir.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect("script with workdir override must run");
    assert!(
        override_dir.path().join("ran.marker").exists(),
        "relative write must land in the workdir override"
    );
    assert!(
        !default_dir.path().join("ran.marker").exists(),
        "relative write must NOT land in the caller-provided default dir"
    );
}

// execute_script: `workdir:` expands a leading `~` and `$VAR`/`${VAR}` against
// the script environment (here `$CFGD_MODULE_DIR`).
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn execute_script_workdir_override_expands_tilde_and_vars() {
    let printer = crate::test_helpers::test_printer();
    let home = tempfile::tempdir().unwrap();
    let module_dir = tempfile::tempdir().unwrap();
    crate::with_test_home(home.path(), || {
        // `~` form → home.
        let entry_home = ScriptEntry::Full {
            run: "touch from_tilde".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
            workdir: Some("~".into()),
        };
        execute_script(
            &entry_home,
            module_dir.path(),
            module_dir.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("workdir ~ must run");
        assert!(home.path().join("from_tilde").exists());

        // `$CFGD_MODULE_DIR` form → the module dir from the script env.
        let entry_var = ScriptEntry::Full {
            run: "touch from_var".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
            workdir: Some("$CFGD_MODULE_DIR".into()),
        };
        let env = vec![(
            "CFGD_MODULE_DIR".to_string(),
            module_dir.path().display().to_string(),
        )];
        execute_script(
            &entry_var,
            home.path(),
            home.path(),
            &env,
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("workdir $CFGD_MODULE_DIR must run");
        assert!(module_dir.path().join("from_var").exists());
    });
}

// CFGD_* env var names are rejected at parse time (EnvVar deserialization),
// so they never reach build_module_script_env. This test verifies the
// parse-time guard works via the validate_env_var_user_name function.
#[test]
fn cfgd_prefix_rejected_at_parse_time() {
    let yaml = r#"
- name: CFGD_MODULE_NAME
  value: spoofed
"#;
    let err = serde_yaml::from_str::<Vec<crate::config::EnvVar>>(yaml)
        .expect_err("CFGD_* names must be rejected during deserialization");
    let msg = format!("{err}");
    assert!(
        msg.contains("reserved"),
        "error should mention 'reserved': {msg}"
    );
    assert!(
        msg.contains("CFGD_"),
        "error should mention the CFGD_ prefix: {msg}"
    );
}

// execute_script: a missing working_dir surfaces a structured error naming
// both the script and the path, instead of the cryptic `io error: No such
// file or directory (os error 2)` from `cmd.spawn()`.
#[test]
fn execute_script_rejects_missing_working_dir() {
    let printer = crate::test_helpers::test_printer();
    let entry = ScriptEntry::Simple("true".into());
    let missing = std::path::PathBuf::from("/nonexistent/path/does/not/exist");

    let err = execute_script(
        &entry,
        &missing,
        &missing,
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect_err("missing working_dir must error");

    match err {
        CfgdError::Config(ConfigError::Invalid { message }) => {
            assert!(
                message.contains("working directory does not exist"),
                "message should describe the missing-dir failure mode: {message}"
            );
            assert!(
                message.contains("/nonexistent/path/does/not/exist"),
                "message should name the offending path: {message}"
            );
            assert!(
                message.contains("'true'"),
                "message should name the script (run_str): {message}"
            );
        }
        other => panic!("expected ConfigError::Invalid, got: {other:?}"),
    }
}

// execute_script: an existing path that is NOT a directory (a regular file)
// surfaces a distinct error mentioning what was found and the path.
#[test]
fn execute_script_rejects_non_directory_working_dir() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("not-a-dir");
    std::fs::write(&file_path, b"hello").unwrap();
    let entry = ScriptEntry::Simple("true".into());

    let err = execute_script(
        &entry,
        &file_path,
        &file_path,
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect_err("non-directory working_dir must error");

    match err {
        CfgdError::Config(ConfigError::Invalid { message }) => {
            assert!(
                message.contains("not a directory"),
                "message should distinguish the non-dir failure mode: {message}"
            );
            assert!(
                message.contains(&crate::to_posix_string(&file_path)),
                "message should name the offending path: {message}"
            );
        }
        other => panic!("expected ConfigError::Invalid, got: {other:?}"),
    }
}

// execute_script: validation does not regress the happy path — a valid
// working_dir with a trivial inline command still succeeds.
#[test]
fn execute_script_runs_with_valid_working_dir() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let entry = ScriptEntry::Simple("true".into());

    let (label, changed, _captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect("valid working_dir + `true` must succeed");

    assert!(changed, "scripts always report changed=true");
    assert!(
        label.contains("true"),
        "label should reference the script's run_str: {label}"
    );
}

// build_module_script_env: empty module env produces the same output as
// build_script_env (no regressions for modules without spec.env).
#[test]
fn empty_module_env_matches_base_build_script_env() {
    let base = build_script_env(
        &fake_config_dir(),
        "workstation",
        ReconcileContext::Apply,
        &ScriptPhase::PreApply,
        Some("mymod"),
        None,
    );
    let with_empty = build_module_script_env(
        &fake_config_dir(),
        "workstation",
        ReconcileContext::Apply,
        &ScriptPhase::PreApply,
        Some("mymod"),
        None,
        &[],
    );
    assert_eq!(base, with_empty);
}

// bash on windows GHA resolves to WSL bash, which errors with "Windows
// Subsystem for Linux has no installed distributions". Gate to unix.
#[cfg(unix)]
#[test]
fn shell_bash_runs_inline_with_bash() {
    if !crate::command_available("bash") {
        // The bash-specific interpreter path can only be exercised where
        // bash exists; FreeBSD's base ships only POSIX sh, so skip rather
        // than fail on a host that legitimately lacks bash.
        return;
    }
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let entry = ScriptEntry::Full {
        workdir: None,
        run: "echo hello".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Bash,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    };

    let (label, changed, captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect("bash inline script must succeed");

    assert!(changed);
    assert!(label.contains("echo hello"));
    assert_eq!(captured.as_deref(), Some("hello"));
}

#[test]
fn shell_field_rejected_on_file_scripts() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("myscript.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let entry = ScriptEntry::Full {
        workdir: None,
        run: "myscript.sh".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Bash,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    };

    let err = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect_err("shell field on file script must be rejected");

    match err {
        CfgdError::Config(ConfigError::Invalid { message }) => {
            assert!(
                message.contains("shell field cannot be set on file-shebang scripts"),
                "unexpected message: {message}"
            );
            assert!(
                message.contains("myscript.sh"),
                "message should name the script file: {message}"
            );
        }
        other => panic!("expected ConfigError::Invalid, got: {other:?}"),
    }
}

#[test]
fn bash_inline_prepends_env_source() {
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join(".cfgd.env");
    std::fs::write(&env_file, "export TEST_VAR=hello\n").unwrap();

    let cmd = build_inline_command(
        ScriptShell::Bash,
        "echo $TEST_VAR",
        tmp.path(),
        Some(&env_file),
    );
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let combined = args.join(" ");
    assert!(
        combined.contains("shopt -s expand_aliases"),
        "bash preamble should enable alias expansion: {combined}"
    );
    assert!(
        combined.contains("source"),
        "bash preamble should source the env file: {combined}"
    );
    assert!(
        combined.contains("2>/dev/null"),
        "source should suppress errors: {combined}"
    );
    assert!(
        combined.contains("echo $TEST_VAR"),
        "original command must be preserved: {combined}"
    );
}

#[test]
fn zsh_inline_prepends_env_source() {
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join(".cfgd.env");
    std::fs::write(&env_file, "export TEST_VAR=hello\n").unwrap();

    let cmd = build_inline_command(
        ScriptShell::Zsh,
        "echo $TEST_VAR",
        tmp.path(),
        Some(&env_file),
    );
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let combined = args.join(" ");
    assert!(
        combined.contains("setopt aliases"),
        "zsh preamble should enable alias expansion: {combined}"
    );
    assert!(
        combined.contains("source"),
        "zsh preamble should source the env file: {combined}"
    );
    assert!(
        combined.contains("2>/dev/null"),
        "source should suppress errors: {combined}"
    );
    assert!(
        combined.contains("echo $TEST_VAR"),
        "original command must be preserved: {combined}"
    );
}

#[test]
fn sh_inline_ignores_cfgd_env_path() {
    let tmp = tempfile::tempdir().unwrap();
    let env_file = tmp.path().join(".cfgd.env");
    std::fs::write(&env_file, "export TEST_VAR=hello\n").unwrap();

    let cmd = build_inline_command(ScriptShell::Sh, "echo hello", tmp.path(), Some(&env_file));
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let combined = args.join(" ");
    assert!(
        !combined.contains("source"),
        "sh should not source the env file: {combined}"
    );
    assert_eq!(
        args,
        vec!["-c", "echo hello"],
        "sh command should be unchanged"
    );
}

#[test]
fn bash_inline_no_env_file_skips_preamble() {
    let tmp = tempfile::tempdir().unwrap();

    let cmd = build_inline_command(ScriptShell::Bash, "echo hello", tmp.path(), None);
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert_eq!(args, vec!["-c", "echo hello"], "no env file → no preamble");
}

// Auto-detection picks the file's shebang-implied interpreter (`sh`),
// unavailable on Windows GHA. Gate to unix.
#[cfg(unix)]
#[test]
fn shell_auto_on_file_scripts_allowed() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("ok.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho ok\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let entry = ScriptEntry::Full {
        workdir: None,
        run: "ok.sh".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    };

    let result = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    );
    assert!(result.is_ok(), "Auto shell on file scripts must be allowed");
}

// --shell override: passing Some(Bash) on a Simple inline script wraps the
// command in bash, independent of the entry's own shell field (Auto here).
#[cfg(unix)]
#[test]
fn execute_script_uses_shell_override_for_inline_command() {
    if !crate::command_available("bash") {
        // Proving the override routes through bash requires bash on PATH;
        // FreeBSD's base has only POSIX sh, so skip where bash is absent.
        return;
    }
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    // BASH_VERSION is exported by bash but not by sh/dash; if the override
    // wired through, the variable resolves and gets echoed. If not, the
    // empty expansion shows the override was dropped on the floor.
    let entry = ScriptEntry::Simple("echo \"${BASH_VERSION:-no-bash}\"".into());

    let (_, _, captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        Some(ScriptShell::Bash),
        None,
    )
    .expect("inline script with bash override must succeed");

    let out = captured.expect("bash should echo a version string");
    assert!(
        out != "no-bash" && !out.is_empty(),
        "override did not route through bash (got {out:?})"
    );
}

// --shell override: passing Some(Bash) on a file-shebang script is silently
// ignored. The shebang owns interpreter choice — wrapping the file in
// `bash -c "/path/to/file"` would either double-interpret it or break exec
// semantics. The script runs directly; no error surfaces.
#[cfg(unix)]
#[test]
fn execute_script_override_ignored_on_file_shebang() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("ok.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho file-shebang\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entry = ScriptEntry::Simple("ok.sh".into());

    let (_, _, captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        Some(ScriptShell::Bash),
        None,
    )
    .expect("override on file-shebang script must not error");

    assert_eq!(
        captured.as_deref(),
        Some("file-shebang"),
        "file script must run via its own shebang, not via bash wrapper"
    );
}

// --shell override: an entry that explicitly sets `shell:` on a file path
// still errors (that's a user config bug, independent of the override).
#[test]
fn execute_script_entry_shell_on_file_script_still_errors_with_override() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("buggy.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entry = ScriptEntry::Full {
        workdir: None,
        run: "buggy.sh".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Zsh,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    };

    let err = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        Some(ScriptShell::Bash),
        None,
    )
    .expect_err("entry-level shell on a file script must still be rejected");

    match err {
        CfgdError::Config(ConfigError::Invalid { message }) => {
            assert!(
                message.contains("shell field cannot be set on file-shebang scripts"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected ConfigError::Invalid, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Idempotency guards: creates / onlyIf / unless
// -----------------------------------------------------------------------

// A guarded entry whose body writes a sentinel marker into the tempdir.
// After execution, marker presence proves the body ran; absence proves a
// skip. The marker name is fixed; the guard fields are the variable.
#[cfg(unix)]
fn guarded_entry(
    only_if: Option<&str>,
    unless: Option<&str>,
    creates: Option<&str>,
) -> ScriptEntry {
    ScriptEntry::Full {
        workdir: None,
        run: "touch ran.marker".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: only_if.map(String::from),
        unless: unless.map(String::from),
        creates: creates.map(String::from),
        interactive: false,
    }
}

#[cfg(unix)]
fn run_guarded(entry: &ScriptEntry, working_dir: &std::path::Path) -> (bool, bool) {
    let printer = crate::test_helpers::test_printer();
    let (_label, changed, _captured) = execute_script(
        entry,
        working_dir,
        working_dir,
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect("guarded script must not error");
    let ran = working_dir.join("ran.marker").exists();
    (changed, ran)
}

#[cfg(unix)]
#[test]
fn guard_creates_existing_skips() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("present"), b"x").unwrap();
    let entry = guarded_entry(None, None, Some("present"));
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(!changed, "existing creates path must report changed=false");
    assert!(!ran, "body must not run when creates path exists");
}

#[cfg(unix)]
#[test]
fn guard_creates_missing_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(None, None, Some("absent"));
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(changed, "missing creates path must report changed=true");
    assert!(ran, "body must run when creates path is absent");
}

#[cfg(unix)]
#[test]
fn guard_only_if_true_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(Some("true"), None, None);
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(changed, "onlyIf zero-exit must permit running");
    assert!(ran, "body must run when onlyIf succeeds");
}

#[cfg(unix)]
#[test]
fn guard_only_if_false_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(Some("false"), None, None);
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(!changed, "onlyIf non-zero-exit must skip");
    assert!(!ran, "body must not run when onlyIf fails");
}

#[cfg(unix)]
#[test]
fn guard_unless_true_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(None, Some("true"), None);
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(!changed, "unless zero-exit must skip");
    assert!(!ran, "body must not run when unless succeeds");
}

#[cfg(unix)]
#[test]
fn guard_unless_false_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(None, Some("false"), None);
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(changed, "unless non-zero-exit must permit running");
    assert!(ran, "body must run when unless fails");
}

// All guards permit running only when every one says "run".
#[cfg(unix)]
#[test]
fn guard_combined_all_pass_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(Some("true"), Some("false"), Some("absent"));
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(changed);
    assert!(ran, "body must run when all guards permit");
}

// A single skipping guard short-circuits even when others would permit.
#[cfg(unix)]
#[test]
fn guard_combined_one_blocks_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(Some("true"), Some("true"), Some("absent"));
    let (changed, ran) = run_guarded(&entry, tmp.path());
    assert!(!changed, "any blocking guard must skip");
    assert!(!ran, "body must not run when unless already holds");
}

// creates resolves a leading `~` via expand_tilde; a relative path resolves
// against working_dir.
#[cfg(unix)]
#[test]
fn creates_path_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let rel = resolve_creates_path("sub/file", tmp.path());
    assert_eq!(rel, tmp.path().join("sub/file"));

    let abs = resolve_creates_path("/etc/hosts", tmp.path());
    assert_eq!(abs, std::path::PathBuf::from("/etc/hosts"));

    let home = tempfile::tempdir().unwrap();
    crate::with_test_home(home.path(), || {
        let tilde = resolve_creates_path("~/thing", tmp.path());
        assert_eq!(tilde, home.path().join("thing"));
    });
}

// A guard command whose interpreter cannot spawn is a real error, distinct
// from a non-zero exit (the normal condition signal). Skip when pwsh is
// actually installed (the spawn would then succeed).
#[test]
fn guard_spawn_failure_errors() {
    if crate::command_available("pwsh") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let result = run_guard_command(
        "irrelevant",
        ScriptShell::Pwsh,
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
    );
    assert!(
        result.is_err(),
        "a guard whose interpreter cannot spawn must error, not skip silently"
    );
}

// A guard command that hangs past its timeout is a hard error — not a
// silently coerced "skip"/"run" condition signal. Uses the parameterized
// timeout seam so the test stays fast.
#[cfg(unix)]
#[test]
fn guard_timeout_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let result = run_guard_command(
        "sleep 5",
        ScriptShell::Auto,
        tmp.path(),
        &[],
        std::time::Duration::from_millis(100),
    );
    match result {
        Err(CfgdError::Config(ConfigError::Invalid { message })) => {
            assert!(
                message.contains("timed out"),
                "timeout error should say so: {message}"
            );
            assert!(
                message.contains("sleep 5"),
                "timeout error should name the guard command: {message}"
            );
        }
        other => panic!("a hung guard must error with a timeout message, got: {other:?}"),
    }
}

// End-to-end: a guarded script whose onlyIf/unless command hangs past the
// (parameterized) default timeout must make execute_script return Err, not
// silently skip or run the body.
#[cfg(unix)]
#[test]
fn execute_script_guard_timeout_returns_err() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let sentinel = tmp.path().join("body-ran");
    let entry = ScriptEntry::Full {
        workdir: None,
        run: format!("touch {}", sentinel.display()),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: Some("sleep 5".to_string()),
        unless: None,
        creates: None,
        interactive: false,
    };

    let result = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_millis(100),
        &printer,
        None,
        None,
    );

    match result {
        Err(CfgdError::Config(ConfigError::Invalid { message })) => {
            assert!(
                message.contains("timed out"),
                "unexpected message: {message}"
            );
        }
        other => panic!("a hung guard must propagate as Err, got: {other:?}"),
    }
    assert!(
        !sentinel.exists(),
        "body must not run when a guard timed out"
    );
}

// -----------------------------------------------------------------------
// Interactive scripts (TTY-or-skip-with-warn)
// -----------------------------------------------------------------------

#[test]
fn interactive_disposition_branches() {
    assert_eq!(
        interactive_disposition(false, true),
        InteractiveDisposition::NotInteractive
    );
    assert_eq!(
        interactive_disposition(false, false),
        InteractiveDisposition::NotInteractive
    );
    assert_eq!(
        interactive_disposition(true, true),
        InteractiveDisposition::Run
    );
    assert_eq!(
        interactive_disposition(true, false),
        InteractiveDisposition::SkipNoTty
    );
}

// The test process has no TTY, so an interactive script must be SKIPPED:
// changed=false, the body does not run (sentinel absent), and a Warn line
// names the script and the missing-TTY reason.
#[cfg(all(unix, feature = "test-helpers"))]
#[test]
fn interactive_script_without_tty_skips_with_warn() {
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    let tmp = tempfile::tempdir().unwrap();
    let sentinel = tmp.path().join("body-ran");
    let entry = ScriptEntry::Full {
        workdir: None,
        run: format!("touch {}", sentinel.display()),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: true,
    };

    let (_label, changed, captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect("interactive skip must not error");
    printer.flush();

    assert!(
        !changed,
        "no-TTY interactive script must report changed=false"
    );
    assert!(captured.is_none(), "skip captures no output");
    assert!(
        !sentinel.exists(),
        "body must not run when an interactive script is skipped"
    );
    let out = crate::output::strip_ansi(&buf.lock().unwrap());
    assert!(
        out.contains("interactive script skipped") && out.contains("no TTY"),
        "skip line should name the missing-TTY reason: {out:?}"
    );
}

// A skip emits a Role::Skipped status line naming the guard and reason.
#[cfg(all(unix, feature = "test-helpers"))]
#[test]
fn guard_skip_emits_skipped_status_line() {
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    let tmp = tempfile::tempdir().unwrap();
    let entry = guarded_entry(None, Some("true"), None);
    let (_label, changed, _captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect("skip must not error");
    printer.flush();
    assert!(!changed);
    let out = crate::output::strip_ansi(&buf.lock().unwrap());
    assert!(
        out.contains("unless condition already holds"),
        "skip line should name the unless guard and reason: {out:?}"
    );
}

// -----------------------------------------------------------------------
// run_str resolution, spawn-failure mapping, exit-code error path
// -----------------------------------------------------------------------

// An absolute `run:` path that exists is executed directly as a file
// (scripts.rs:306-307, the non-relative branch), NOT joined against
// script_dir. `/bin/true` exits zero, so the script reports changed=true.
#[cfg(unix)]
#[test]
fn execute_script_absolute_run_path_runs_as_file() {
    let printer = crate::test_helpers::test_printer();
    let work = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let true_bin = if std::path::Path::new("/usr/bin/true").exists() {
        "/usr/bin/true"
    } else {
        "/bin/true"
    };
    let entry = ScriptEntry::Simple(true_bin.into());

    let (label, changed, _captured) = execute_script(
        &entry,
        // A script_dir that does NOT contain `true` — proves the absolute
        // path is used verbatim, never joined onto script_dir.
        script_dir.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect("absolute executable run path must run directly");
    assert!(changed, "a zero-exit file script reports changed=true");
    assert!(
        label.contains(true_bin),
        "label should reference the absolute run path: {label}"
    );
}

// A non-zero exit from the body surfaces a structured error naming the
// script and the exit code (scripts.rs:490-498). `/bin/false` exits 1.
#[cfg(unix)]
#[test]
fn execute_script_nonzero_exit_errors_with_exit_code() {
    let printer = crate::test_helpers::test_printer();
    let work = tempfile::tempdir().unwrap();
    // Inline command that prints to stdout then exits non-zero, so the
    // error message also folds in the captured output.
    let entry = ScriptEntry::Simple("echo boom; exit 7".into());

    let err = execute_script(
        &entry,
        work.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect_err("a non-zero exit must surface as Err");

    match err {
        CfgdError::Config(ConfigError::Invalid { message }) => {
            assert!(
                message.contains("failed (exit 7)"),
                "message should name the real exit code: {message}"
            );
            assert!(
                message.contains("echo boom; exit 7"),
                "message should name the failing script: {message}"
            );
            assert!(
                message.contains("boom"),
                "message should fold in the script's captured output: {message}"
            );
        }
        other => panic!("expected ConfigError::Invalid, got: {other:?}"),
    }
}

// A spawn ENOENT (the interpreter binary cannot be resolved) is remapped to
// a targeted message pointing at a PATH-dropping spec.env as the usual
// cause, instead of a bare `os error 2` (scripts.rs:410-421). Exercised via
// the Pwsh interpreter when pwsh is not installed; skipped otherwise (the
// spawn would then succeed).
#[cfg(unix)]
#[test]
fn execute_script_spawn_enoent_maps_to_interpreter_hint() {
    if crate::command_available("pwsh") {
        return;
    }
    let printer = crate::test_helpers::test_printer();
    let work = tempfile::tempdir().unwrap();
    let entry = ScriptEntry::Full {
        workdir: None,
        run: "Write-Output hi".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Pwsh,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    };

    let err = execute_script(
        &entry,
        work.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
    )
    .expect_err("a missing interpreter must surface as Err, not hang");

    match err {
        CfgdError::Config(ConfigError::Invalid { message }) => {
            assert!(
                message.contains("could not spawn the script interpreter"),
                "ENOENT spawn must be remapped to the interpreter hint: {message}"
            );
            assert!(
                message.contains("spec.env PATH"),
                "hint should point at a PATH-dropping spec.env: {message}"
            );
        }
        other => panic!("expected ConfigError::Invalid, got: {other:?}"),
    }
}

// build_inline_command(Pwsh, ...): pwsh is invoked with -NoProfile -Command
// and the raw command, with no env-file preamble (cfgd_env_path_for returns
// None for non-bash/zsh shells). Asserts the exact argv shape
// (scripts.rs:630-633).
#[test]
fn pwsh_inline_command_argv_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let cmd = build_inline_command(ScriptShell::Pwsh, "Get-Date", tmp.path(), None);
    assert_eq!(
        cmd.get_program().to_string_lossy(),
        "pwsh",
        "Pwsh shell must invoke the pwsh interpreter"
    );
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert_eq!(
        args,
        vec!["-NoProfile", "-Command", "Get-Date"],
        "pwsh argv must be -NoProfile -Command <cmd> with no preamble"
    );
}

// cfgd_env_path_for returns None for every non-bash/zsh shell even when a
// `~/.cfgd.env` exists — only bash/zsh get the source preamble
// (scripts.rs:653-660).
#[test]
#[serial_test::serial]
fn cfgd_env_path_for_only_bash_and_zsh() {
    let home = tempfile::tempdir().unwrap();
    crate::with_test_home(home.path(), || {
        // Create a real ~/.cfgd.env so the `exists()` check would pass.
        std::fs::write(home.path().join(".cfgd.env"), "export X=1\n").unwrap();

        assert!(
            cfgd_env_path_for(ScriptShell::Bash).is_some(),
            "bash must pick up an existing ~/.cfgd.env"
        );
        assert!(
            cfgd_env_path_for(ScriptShell::Zsh).is_some(),
            "zsh must pick up an existing ~/.cfgd.env"
        );
        assert!(
            cfgd_env_path_for(ScriptShell::Sh).is_none(),
            "sh must never source ~/.cfgd.env"
        );
        assert!(
            cfgd_env_path_for(ScriptShell::Auto).is_none(),
            "auto must never source ~/.cfgd.env"
        );
        assert!(
            cfgd_env_path_for(ScriptShell::Pwsh).is_none(),
            "pwsh must never source ~/.cfgd.env"
        );
    });
}

// resolve_script_workdir: a `$VAR` reference present in the script env is
// expanded; an unset reference expands to empty; a leading `~` expands to
// home AFTER var expansion (scripts.rs:134-142).
#[test]
#[serial_test::serial]
fn resolve_script_workdir_expands_vars_then_tilde() {
    let home = tempfile::tempdir().unwrap();
    crate::with_test_home(home.path(), || {
        let env = vec![("DEPLOY".to_string(), "/srv/app".to_string())];

        // $VAR present in env → expands to its value.
        assert_eq!(
            resolve_script_workdir("$DEPLOY/cfg", &env),
            std::path::PathBuf::from("/srv/app/cfg")
        );
        // ${VAR} braced form too.
        assert_eq!(
            resolve_script_workdir("${DEPLOY}", &env),
            std::path::PathBuf::from("/srv/app")
        );
        // Leading ~ expands to home.
        assert_eq!(
            resolve_script_workdir("~/sub", &env),
            home.path().join("sub")
        );
        // Unset reference expands to empty (shell-like).
        assert_eq!(
            resolve_script_workdir("/a/${NOPE}/b", &env),
            std::path::PathBuf::from("/a//b")
        );
    });
}

// build_script_env: the Reconcile context maps to the "reconcile" CFGD_CONTEXT
// value (the Apply arm is covered elsewhere), and module_dir is injected when
// provided. Asserts exact names+values and that module_name is omitted when
// None (scripts.rs:57-70).
#[test]
fn build_script_env_reconcile_context_and_module_dir() {
    let env = build_script_env(
        std::path::Path::new("/cfg"),
        "node",
        ReconcileContext::Reconcile,
        &ScriptPhase::OnDrift,
        None,
        Some(std::path::Path::new("/mods/x")),
    );
    let lookup = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());

    assert_eq!(lookup("CFGD_CONTEXT"), Some("reconcile"));
    assert_eq!(lookup("CFGD_PROFILE"), Some("node"));
    assert_eq!(
        lookup("CFGD_PHASE"),
        Some(ScriptPhase::OnDrift.display_name())
    );
    assert_eq!(lookup("CFGD_CONFIG_DIR"), Some("/cfg"));
    assert_eq!(lookup("CFGD_MODULE_DIR"), Some("/mods/x"));
    assert_eq!(
        lookup("CFGD_MODULE_NAME"),
        None,
        "module name must be omitted when None"
    );
}
