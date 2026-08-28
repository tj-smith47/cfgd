use super::*;
use crate::config::EnvVar;

fn fake_env_var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: value.to_string(),
        platforms: vec![],
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
        &ScriptEnvContext {
            config_dir: &fake_config_dir(),
            profile_name: "workstation",
            context: ReconcileContext::Apply,
            phase: &ScriptPhase::PostApply,
            module_name: Some("nvim"),
            module_dir: None,
            path_dirs: &[],
        },
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
        &ScriptEnvContext {
            config_dir: &fake_config_dir(),
            profile_name: "workstation",
            context: ReconcileContext::Apply,
            phase: &ScriptPhase::PostApply,
            module_name: Some("nvim"),
            module_dir: None,
            path_dirs: &[],
        },
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
            &ScriptEnvContext {
                config_dir: &fake_config_dir(),
                profile_name: "workstation",
                context: ReconcileContext::Apply,
                phase: &ScriptPhase::PostApply,
                module_name: Some("clift"),
                module_dir: None,
                path_dirs: &[],
            },
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
// pollute the user's GitOps repo.
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
    let entry = ScriptEntry::Full(ScriptCommand {
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
    });
    execute_script(
        &entry,
        default_dir.path(),
        default_dir.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
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
        let entry_home = ScriptEntry::Full(ScriptCommand {
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
        });
        execute_script(
            &entry_home,
            module_dir.path(),
            module_dir.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
            ScriptReport::default(),
        )
        .expect("workdir ~ must run");
        assert!(home.path().join("from_tilde").exists());

        // `$CFGD_MODULE_DIR` form → the module dir from the script env.
        let entry_var = ScriptEntry::Full(ScriptCommand {
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
        });
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
            ScriptReport::default(),
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
        ScriptReport::default(),
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
        ScriptReport::default(),
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
        ScriptReport::default(),
    )
    .expect("valid working_dir + `true` must succeed");

    assert!(changed, "scripts always report changed=true");
    assert!(
        label.contains("true"),
        "label should reference the script's run_str: {label}"
    );
}

// `execute_script`'s return value is the persisted
// `ActionResult.description` for onChange callers, which
// `parse_resource_from_description` parses back into a managed-resource
// id — it must be byte-identical to the raw run_str, never condensed via
// `condense_script_label`, or a multi-line inline script's id reshapes
// and orphans every already-recorded state row.
#[test]
fn execute_script_return_value_preserves_raw_multiline_body() {
    let printer = crate::test_helpers::test_printer();
    let tmp = tempfile::tempdir().unwrap();
    let entry = ScriptEntry::Simple("true\ntrue".into());

    let (desc, changed, _captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .expect("multi-line inline script must succeed");

    assert!(changed);
    assert_eq!(
        desc,
        format!("Running script: {}", entry.run_str()),
        "returned description must be the raw run_str, not the condensed label"
    );
    assert!(
        desc.contains('\n'),
        "raw multi-line body must be preserved byte-identical: {desc:?}"
    );
}

// A caller that opens a pseudo-phase sizes its alignment column from
// `hook_script_subject` BEFORE any script runs; `execute_script` composes the
// status line's own copy as each one finishes. If the two ever stop agreeing,
// every hook line in the group pads against a width measured off a different
// string.
#[test]
fn hook_status_line_matches_the_precomputed_hook_subject() {
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    let tmp = tempfile::tempdir().unwrap();
    // A long body, so the condensing half of the derivation is exercised too.
    // `echo` and nothing else: a comment marker would not be one under
    // `cmd.exe /C`, and the non-zero exit that follows would fail the call
    // rather than the assertion.
    let body = format!("echo {}", "x".repeat(120));
    let entry = ScriptEntry::Simple(body.clone());

    execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(30),
        &printer,
        None,
        None,
        ScriptReport {
            subject: super::ScriptSubject::Hook("onDrift"),
            non_fatal: true,
            ..ScriptReport::default()
        },
    )
    .expect("`echo` must succeed on every shell ScriptShell::Auto dispatches to");
    drop(printer);

    let out = crate::test_helpers::captured_text(&buf);
    let expected = crate::reconciler::hook_script_subject("onDrift", &body).to_string();
    assert!(
        out.contains(&expected),
        "the rendered status must carry the same subject the width was derived from\n\
         expected: {expected:?}\ngot:\n{out}"
    );
}

// build_module_script_env: empty module env produces the same output as
// build_script_env (no regressions for modules without spec.env).
fn joined(dirs: &[&str]) -> String {
    std::env::join_paths(dirs)
        .expect("test fixture dirs are joinable")
        .to_string_lossy()
        .into_owned()
}

fn path_of(env: &[(String, String)]) -> Option<&str> {
    env.iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.as_str())
}

// No bootstrapped manager means the script env is byte-identical to what it was
// before this feature existed: no PATH entry appears at all.
#[test]
#[serial_test::serial]
fn no_bootstrapped_dirs_leaves_path_absent() {
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let _g = crate::test_helpers::EnvVarGuard::set("PATH", &joined(&["/usr/bin"]));
    let mut env: Vec<(String, String)> = Vec::new();
    super::prepend_bootstrapped_path_dirs(&mut env, &[]);
    assert!(env.is_empty());
}

// The bootstrapped prefix lands AHEAD of the inherited PATH, matching what
// `generate_env_file_content` writes for the login shell that follows.
#[test]
#[serial_test::serial]
fn bootstrapped_dirs_prepend_to_inherited_path() {
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let inherited = joined(&["/usr/bin", "/bin"]);
    let _g = crate::test_helpers::EnvVarGuard::set("PATH", &inherited);
    let mut env: Vec<(String, String)> = Vec::new();
    super::prepend_bootstrapped_path_dirs(
        &mut env,
        &["/home/linuxbrew/.linuxbrew/bin".to_string()],
    );
    assert_eq!(
        path_of(&env),
        Some(joined(&["/home/linuxbrew/.linuxbrew/bin", "/usr/bin", "/bin"]).as_str())
    );
}

// A prefix already on PATH must not be duplicated, and if every recorded
// directory is already there the PATH entry is not written at all — a
// re-apply on a converged machine changes nothing.
#[test]
#[serial_test::serial]
fn already_present_dir_is_not_duplicated() {
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let inherited = joined(&["/opt/brew/bin", "/usr/bin"]);
    let _g = crate::test_helpers::EnvVarGuard::set("PATH", &inherited);
    let mut env: Vec<(String, String)> = Vec::new();
    super::prepend_bootstrapped_path_dirs(&mut env, &["/opt/brew/bin".to_string()]);
    assert!(env.is_empty());

    let mut env: Vec<(String, String)> = Vec::new();
    super::prepend_bootstrapped_path_dirs(
        &mut env,
        &["/opt/brew/bin".to_string(), "/opt/npm/bin".to_string()],
    );
    assert_eq!(
        path_of(&env),
        Some(joined(&["/opt/npm/bin", "/opt/brew/bin", "/usr/bin"]).as_str())
    );
}

// A PATH already in the env vec — the value a caller assembled, not the one this
// process inherited — is what the bootstrapped directories extend.
#[test]
#[serial_test::serial]
fn existing_env_path_entry_is_the_base() {
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let _g = crate::test_helpers::EnvVarGuard::set("PATH", &joined(&["/inherited"]));
    let mut env = vec![("PATH".to_string(), joined(&["/caller/bin"]))];
    super::prepend_bootstrapped_path_dirs(&mut env, &["/opt/brew/bin".to_string()]);
    assert_eq!(env.len(), 1, "the PATH slot is replaced, not appended to");
    assert_eq!(
        path_of(&env),
        Some(joined(&["/opt/brew/bin", "/caller/bin"]).as_str())
    );
}

// A recorded directory containing the platform separator cannot be joined back
// into a PATH; the inherited value survives untouched rather than splitting into
// two bogus entries.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn unjoinable_dir_leaves_path_untouched() {
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let _g = crate::test_helpers::EnvVarGuard::set("PATH", &joined(&["/usr/bin"]));
    let mut env = vec![("PATH".to_string(), "/caller/bin".to_string())];
    super::prepend_bootstrapped_path_dirs(&mut env, &["/opt/a:b/bin".to_string()]);
    assert_eq!(path_of(&env), Some("/caller/bin"));
}

// The dirs reach a module script through build_module_script_env, and a module's
// own `PATH: ...:$PATH` expands against the merged value rather than dropping it.
#[test]
#[serial_test::serial]
fn module_env_path_expands_against_bootstrapped_dirs() {
    let _path_excl = crate::test_helpers::path_env_mutation_guard();
    let _g = crate::test_helpers::EnvVarGuard::set("PATH", &joined(&["/usr/bin"]));
    // Separator via join_paths, not a literal `:` — the expansion splices the
    // merged PATH in with the platform's own separator, so a hardcoded colon
    // makes the fixture disagree with itself on Windows.
    let module_env = vec![fake_env_var("PATH", &joined(&["/mod/bin", "$PATH"]))];
    let env = build_module_script_env(
        &ScriptEnvContext {
            config_dir: &fake_config_dir(),
            profile_name: "workstation",
            context: ReconcileContext::Apply,
            phase: &ScriptPhase::PostApply,
            module_name: Some("nvim"),
            module_dir: None,
            path_dirs: &["/home/linuxbrew/.linuxbrew/bin".to_string()],
        },
        &module_env,
    );
    assert_eq!(
        path_of(&env),
        Some(joined(&["/mod/bin", "/home/linuxbrew/.linuxbrew/bin", "/usr/bin"]).as_str())
    );
}

#[test]
fn empty_module_env_matches_base_build_script_env() {
    let base = build_script_env(&ScriptEnvContext {
        config_dir: &fake_config_dir(),
        profile_name: "workstation",
        context: ReconcileContext::Apply,
        phase: &ScriptPhase::PreApply,
        module_name: Some("mymod"),
        module_dir: None,
        path_dirs: &[],
    });
    let with_empty = build_module_script_env(
        &ScriptEnvContext {
            config_dir: &fake_config_dir(),
            profile_name: "workstation",
            context: ReconcileContext::Apply,
            phase: &ScriptPhase::PreApply,
            module_name: Some("mymod"),
            module_dir: None,
            path_dirs: &[],
        },
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
    let entry = ScriptEntry::Full(ScriptCommand {
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
    });

    let (label, changed, captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
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

    let entry = ScriptEntry::Full(ScriptCommand {
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
    });

    let err = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
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
        true,
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
        true,
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

    let cmd = build_inline_command(
        ScriptShell::Sh,
        "echo hello",
        tmp.path(),
        Some(&env_file),
        true,
    );
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

    let cmd = build_inline_command(ScriptShell::Bash, "echo hello", tmp.path(), None, true);
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert_eq!(args, vec!["-c", "echo hello"], "no env file → no preamble");
}

// set_process_group=true (every non-interactive spawn arm) still puts the
// child in its OWN new process group — child pgid == child pid — so a
// timeout/idle kill can `kill(-pid, …)` the whole subtree without hitting
// cfgd itself. This is the behavior every arm had before the interactive
// fix, and must stay unchanged.
#[cfg(unix)]
#[test]
fn build_inline_command_default_spawns_own_process_group() {
    use nix::unistd::{Pid, getpgid};

    let _path_guard = crate::test_helpers::path_env_read_guard();
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = build_inline_command(ScriptShell::Sh, "sleep 0.3", tmp.path(), None, true);
    let mut child = cmd.spawn().expect("spawn must succeed");
    let child_pid = Pid::from_raw(child.id() as i32);
    let child_pgid = getpgid(Some(child_pid)).expect("child must still be alive");
    assert_eq!(
        child_pgid, child_pid,
        "set_process_group=true must make the child its own group leader"
    );
    // Signal the GROUP, not the leader: whether `sh -c 'sleep …'` execs the
    // sleep or forks it is the host's choice of /bin/sh (dash execs, bash
    // forks), and killing only the leader leaves a bash host's grandchild
    // holding the test's stdio — which is what nextest reports as a leak.
    // Safe here precisely because the assertion above proved the group is the
    // child's own.
    let _ = nix::sys::signal::killpg(child_pgid, nix::sys::signal::Signal::SIGKILL);
    let _ = child.wait();
}

// set_process_group=false (the interactive `Run` arm only) leaves the
// child in cfgd's OWN process group instead of a new one — the fix that
// restores terminal Ctrl-C delivery and raw-mode TUI reads to an
// interactive script (see execute_script_inner's `Run` arm doc comment).
#[cfg(unix)]
#[test]
fn build_inline_command_interactive_shares_callers_process_group() {
    use nix::unistd::{Pid, getpgid, getpgrp};

    let _path_guard = crate::test_helpers::path_env_read_guard();
    let tmp = tempfile::tempdir().unwrap();
    let own_pgid = getpgrp();
    // `exec` so the shell REPLACES itself instead of possibly forking the
    // sleep (bash forks, dash execs): this child shares the caller's process
    // group by design, so the sibling test's killpg escape is not available
    // here — a forked grandchild would outlive `child.kill()` holding the
    // test's stdio, which nextest reports as a leak.
    let mut cmd = build_inline_command(ScriptShell::Sh, "exec sleep 5", tmp.path(), None, false);
    let mut child = cmd.spawn().expect("spawn must succeed");
    let child_pid = Pid::from_raw(child.id() as i32);
    let child_pgid = getpgid(Some(child_pid)).expect("child must still be alive");
    assert_eq!(
        child_pgid, own_pgid,
        "set_process_group=false must leave the child in the caller's own group"
    );
    let _ = child.kill();
    let _ = child.wait();
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

    let entry = ScriptEntry::Full(ScriptCommand {
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
    });

    let result = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
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
        ScriptReport::default(),
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
        ScriptReport::default(),
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
    let entry = ScriptEntry::Full(ScriptCommand {
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
    });

    let err = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        Some(ScriptShell::Bash),
        None,
        ScriptReport::default(),
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
    ScriptEntry::Full(ScriptCommand {
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
    })
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
        ScriptReport::default(),
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
    let entry = ScriptEntry::Full(ScriptCommand {
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
    });

    let result = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_millis(100),
        &printer,
        None,
        None,
        ScriptReport::default(),
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

// With no TTY an interactive script must be SKIPPED: changed=false, the body
// does not run (sentinel absent), and a Warn line names the script and the
// missing-TTY reason. The premise is SUPPLIED through `execute_script_with_tty`
// rather than inherited from whatever the suite was invoked from — read from
// the ambient terminal, the test asserts the skip path while running the run
// path the moment the suite is started under a pty.
#[cfg(all(unix, feature = "test-helpers"))]
#[test]
fn interactive_script_without_tty_skips_with_warn() {
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    let tmp = tempfile::tempdir().unwrap();
    let sentinel = tmp.path().join("body-ran");
    let entry = ScriptEntry::Full(ScriptCommand {
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
    });

    let (_label, changed, captured) = execute_script_with_tty(
        false,
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
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
    let out = crate::test_helpers::captured_text(&buf);
    assert!(
        out.contains("interactive script skipped") && out.contains("no TTY"),
        "skip line should name the missing-TTY reason: {out:?}"
    );
}

/// A user script is the one thing cfgd runs whose effects it cannot predict: a
/// `preApply` hook that installs a toolchain must be visible to everything
/// planned after it. The tool lands in a directory a bootstrap registered, so
/// nothing about `PATH` or the registry changes while the script runs — only the
/// script's own completion can retire the miss memoized before it.
#[cfg(all(unix, feature = "test-helpers"))]
#[test]
#[serial_test::serial]
fn a_script_that_installs_a_tool_retires_the_memoized_miss() {
    let (printer, _buf) = crate::output::Printer::for_test();
    // Brackets both probes: another test emptying `PATH` between them would
    // turn the resolution this one is about into a false negative.
    let _path = crate::test_helpers::path_env_read_guard();
    let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
    let tmp = tempfile::tempdir().unwrap();
    let stem = "cfgd-probe-installed-by-script";
    crate::register_bootstrapped_path_dirs(&[tmp.path().to_string_lossy().into_owned()]);

    assert!(
        !crate::command_available(stem),
        "the tool is not there yet — and this miss is what gets memoized"
    );

    let installer = tmp.path().join(stem);
    let entry = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: format!(
            "printf '#!/bin/sh\\nexit 0\\n' > {p} && chmod 755 {p}",
            // Quoted through the crate's own helper rather than spliced bare:
            // a path interpolated into a shell command is the shape those
            // helpers exist to make unwriteable, example code included.
            p = crate::posix_single_quoted(&installer.to_string_lossy())
        ),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    });
    execute_script_with_tty(
        false,
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(30),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .expect("script must run");

    assert!(
        crate::command_available(stem),
        "a tool a lifecycle script installed must be resolvable to what follows it"
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
        ScriptReport::default(),
    )
    .expect("skip must not error");
    printer.flush();
    assert!(!changed);
    let out = crate::test_helpers::captured_text(&buf);
    assert!(
        out.contains("unless condition already holds"),
        "skip line should name the unless guard and reason: {out:?}"
    );
}

// -----------------------------------------------------------------------
// run_str resolution, spawn-failure mapping, exit-code error path
// -----------------------------------------------------------------------

// A relative `run:` naming exactly an existing file, with no trailing
// text, is direct exec — byte-identical to the behavior in place before
// `run:` args were ever resolved at all. A pure function over its own
// `script_dir` parameter, so no test-home fixture is needed.
#[test]
fn resolve_run_target_relative_no_args_is_direct_exec() {
    let script_dir = tempfile::tempdir().unwrap();
    std::fs::write(script_dir.path().join("foo.sh"), "#!/bin/sh\n").unwrap();
    let target = resolve_run_target("foo.sh", script_dir.path(), ScriptShell::Auto);
    match target {
        RunTarget::File(resolved) => assert_eq!(resolved, script_dir.path().join("foo.sh")),
        RunTarget::Inline(cmd) => panic!("expected direct exec, got inline: {cmd}"),
    }
}

// A whole-string `run:` naming a DIRECTORY, not a file, must not take the
// direct-exec arm — `exists()` accepts a directory just as readily as a
// file, and the same defect the leading-token case guards below reaches
// here too.
#[test]
fn resolve_run_target_whole_string_naming_a_directory_is_left_untouched() {
    let script_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(script_dir.path().join("subdir")).unwrap();
    let target = resolve_run_target("subdir", script_dir.path(), ScriptShell::Auto);
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, "subdir"),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// The expectation for a substituted leading token under `ScriptShell::Auto`,
// which quotes for the shell it will actually dispatch to: cmd.exe double
// quoting on Windows, POSIX single quoting everywhere else (see
// `quote_resolved_script_path`). The substitution/tail behavior under test is
// identical on both arms; only the quote dialect follows the host.
fn auto_quoted(path: &std::path::Path) -> String {
    if cfg!(windows) {
        crate::cmd_double_quoted(&path.to_string_lossy())
    } else {
        crate::posix_single_quoted(&path.to_string_lossy())
    }
}

// A relative `run:` carrying trailing text is the shell arm (never
// direct-exec, even though the leading token names a real file) — the
// remainder is untouched so the shell, not this function, parses it. The
// leading token IS resolved against `script_dir` and substituted back in,
// shell-quoted, fixing the original triage bug (unresolved relative paths)
// without taking shell parsing away from the shell (the C1 regression this
// pins against).
#[test]
fn resolve_run_target_relative_with_args_substitutes_leading_token_only() {
    let script_dir = tempfile::tempdir().unwrap();
    std::fs::write(script_dir.path().join("foo.sh"), "#!/bin/sh\n").unwrap();
    let target = resolve_run_target("foo.sh --flag value", script_dir.path(), ScriptShell::Auto);
    let expected_path = auto_quoted(&script_dir.path().join("foo.sh"));
    match target {
        RunTarget::Inline(cmd) => {
            assert_eq!(cmd, format!("{expected_path} --flag value"));
        }
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// An absolute `run:` naming exactly an existing file, with no trailing
// text, is direct exec and never joined onto `script_dir`.
#[test]
fn resolve_run_target_absolute_no_args_is_direct_exec() {
    let target_dir = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let absolute = target_dir.path().join("run-me");
    std::fs::write(&absolute, "#!/bin/sh\n").unwrap();
    let run_str = absolute.to_str().expect("tempdir path must be utf8");
    let target = resolve_run_target(run_str, script_dir.path(), ScriptShell::Auto);
    match target {
        RunTarget::File(resolved) => assert_eq!(resolved, absolute),
        RunTarget::Inline(cmd) => panic!("expected direct exec, got inline: {cmd}"),
    }
}

// An absolute `run:` carrying trailing text is the shell arm, with the
// leading absolute path substituted back in unchanged (never re-joined) and
// shell-quoted.
#[test]
fn resolve_run_target_absolute_with_args_substitutes_leading_token_only() {
    let target_dir = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let absolute = target_dir.path().join("run-me");
    std::fs::write(&absolute, "#!/bin/sh\n").unwrap();
    let run_str = format!("{} --flag value", absolute.to_str().unwrap());
    let target = resolve_run_target(&run_str, script_dir.path(), ScriptShell::Auto);
    let expected_path = auto_quoted(&absolute);
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, format!("{expected_path} --flag value")),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// A leading token that does NOT resolve to a real file (an ordinary shell
// command, e.g. `echo hello`) is left completely untouched — no
// substitution, no existence assumption, exactly the shell-arm behavior
// from before any `run:` resolution existed.
#[test]
fn resolve_run_target_unresolvable_leading_token_is_left_untouched() {
    let script_dir = tempfile::tempdir().unwrap();
    let target = resolve_run_target("echo hello", script_dir.path(), ScriptShell::Auto);
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, "echo hello"),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// A single relative token with no whitespace that doesn't exist as a file
// (a PATH-resolved binary name) is also left untouched — the no-whitespace
// early return in `resolve_run_target`.
#[test]
fn resolve_run_target_unresolvable_single_token_is_left_untouched() {
    let script_dir = tempfile::tempdir().unwrap();
    let target = resolve_run_target("does-not-exist.sh", script_dir.path(), ScriptShell::Auto);
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, "does-not-exist.sh"),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// A leading `.` (the POSIX dot-source builtin, e.g.
// `run: . ~/.venv/bin/activate && python app.py`) must NOT resolve —
// `script_dir.join(".")` names `script_dir` itself, which `exists()` (but
// not `is_file()`) accepts, and substituting a directory in place of the
// dot-source idiom silently rewrites `run:` into nonsense. The whole string
// is left byte-identical, so the shell's own dot-source handling still runs.
#[test]
fn resolve_run_target_leading_dot_source_builtin_is_left_untouched() {
    let script_dir = tempfile::tempdir().unwrap();
    let run_str = ". ~/.venv/bin/activate && python app.py";
    let target = resolve_run_target(run_str, script_dir.path(), ScriptShell::Auto);
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, run_str),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// A block scalar opening with a blank line
// (`run_str == "\necho hi\n"`) has an empty leading token —
// `script_dir.join("")` names `script_dir` itself, the same directory trap
// as the dot-source case. Must not substitute `script_dir` in as argv[0].
#[test]
fn resolve_run_target_empty_leading_token_is_left_untouched() {
    let script_dir = tempfile::tempdir().unwrap();
    let run_str = "\necho hi\n";
    let target = resolve_run_target(run_str, script_dir.path(), ScriptShell::Auto);
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, run_str),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// A single-line `run: |` block scalar clips to exactly one trailing
// newline (YAML block-scalar "clip" chomping, the default). That newline
// defeats the whole-string existence test (no real file is named
// "foo.sh\n"), so this is the shell arm — same as any other trailing text
// — with the leading token resolved and the newline preserved verbatim in
// the tail, so the resolved command still ends the line the way the shell
// expects.
#[test]
fn resolve_run_target_single_line_block_scalar_trailing_newline_is_shell_arm() {
    let script_dir = tempfile::tempdir().unwrap();
    std::fs::write(script_dir.path().join("foo.sh"), "#!/bin/sh\n").unwrap();
    let target = resolve_run_target("foo.sh\n", script_dir.path(), ScriptShell::Auto);
    let expected_path = auto_quoted(&script_dir.path().join("foo.sh"));
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, format!("{expected_path}\n")),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// `$CFGD_CONFIG_DIR/...` is a shell-expanded env-var reference the script's
// own environment supplies at spawn time (`build_script_env`) — this
// function only ever tests the LITERAL leading token against the
// filesystem, so a literal directory named "$CFGD_CONFIG_DIR" never exists
// and the whole string passes through byte-identical, unresolved, for the
// shell to expand.
#[test]
fn resolve_run_target_config_dir_var_form_is_left_byte_identical() {
    let script_dir = tempfile::tempdir().unwrap();
    let target = resolve_run_target(
        "$CFGD_CONFIG_DIR/foo.sh --flag",
        script_dir.path(),
        ScriptShell::Auto,
    );
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, "$CFGD_CONFIG_DIR/foo.sh --flag"),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// C1 regression pin (unit level): `&&` in the trailing text must survive
// into the substituted command unchanged — the discriminator that broke
// this (splitting the WHOLE string on first whitespace and treating
// everything after as argv) is gone.
#[test]
fn resolve_run_target_preserves_shell_metacharacters_in_tail() {
    let script_dir = tempfile::tempdir().unwrap();
    std::fs::write(script_dir.path().join("deploy.sh"), "#!/bin/sh\n").unwrap();
    let target = resolve_run_target(
        "deploy.sh && echo done",
        script_dir.path(),
        ScriptShell::Auto,
    );
    let expected_path = auto_quoted(&script_dir.path().join("deploy.sh"));
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, format!("{expected_path} && echo done")),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

// C1 regression pin (unit level): quoted trailing arguments survive
// unsplit — `resolve_run_target` never touches anything past the leading
// token's own byte span.
#[test]
fn resolve_run_target_preserves_quoted_tail() {
    let script_dir = tempfile::tempdir().unwrap();
    std::fs::write(script_dir.path().join("greet.sh"), "#!/bin/sh\n").unwrap();
    let target = resolve_run_target(
        "greet.sh \"hello world\"",
        script_dir.path(),
        ScriptShell::Auto,
    );
    let expected_path = auto_quoted(&script_dir.path().join("greet.sh"));
    match target {
        RunTarget::Inline(cmd) => assert_eq!(cmd, format!("{expected_path} \"hello world\"")),
        RunTarget::File(resolved) => panic!("expected shell arm, got direct exec: {resolved:?}"),
    }
}

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
        ScriptReport::default(),
    )
    .expect("absolute executable run path must run directly");
    assert!(changed, "a zero-exit file script reports changed=true");
    assert!(
        label.contains(true_bin),
        "label should reference the absolute run path: {label}"
    );
}

// End-to-end: a relative `run:` carrying trailing arguments resolves its
// FIRST token against script_dir, substitutes the quoted absolute path back
// into the command string, and hands the whole thing to the shell — the
// shell, not this function, splits "world" into $1. Before `resolve_run_target`
// existed, the whole string (space and all) was tested for existence as one
// path and never matched, so the leading token was never resolved against
// script_dir at all.
#[cfg(unix)]
#[test]
fn execute_script_relative_run_path_with_args_resolves_against_script_dir() {
    let printer = crate::test_helpers::test_printer();
    let work = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let script_path = script_dir.path().join("greet.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho \"hello $1\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entry = ScriptEntry::Simple("greet.sh world".into());

    let (_label, changed, captured) = execute_script(
        &entry,
        script_dir.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .expect("relative run path with trailing args must resolve against script_dir and run");
    assert!(changed, "a zero-exit file script reports changed=true");
    let out = captured.unwrap_or_default();
    assert!(
        out.contains("hello world"),
        "trailing arg must reach the script's argv: {out:?}"
    );
}

// C1 regression pin (end-to-end): `&&` after a resolved leading token must
// still chain to a second command — the bug this fix exists for. Before the
// `RunTarget` split, the whole `run:` string (space and all) was tested for
// existence as one path, so a trailing `&& …` became literal argv of a
// script that never received it as shell syntax.
#[cfg(unix)]
#[test]
fn execute_script_metacharacters_after_resolved_script_still_run() {
    let printer = crate::test_helpers::test_printer();
    let work = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let script_path = script_dir.path().join("first.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho first\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entry = ScriptEntry::Simple("first.sh && echo second".into());

    let (_label, changed, captured) = execute_script(
        &entry,
        script_dir.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .expect("a resolved leading token followed by `&&` must still run as shell syntax");
    assert!(changed, "a zero-exit script reports changed=true");
    let out = captured.unwrap_or_default();
    assert!(out.contains("first"), "first command must run: {out:?}");
    assert!(
        out.contains("second"),
        "second command chained with && must also run: {out:?}"
    );
}

// C1 regression pin (end-to-end): a quoted trailing argument must reach the
// script as ONE argv entry, not be split on the embedded space. Before this
// fix the whole string never resolved as a file, so it ran inline via the
// shell already — this pins that the resolved-leading-token substitution
// does not disturb quoting later in the string.
#[cfg(unix)]
#[test]
fn execute_script_quoted_argument_reaches_script_unsplit() {
    let printer = crate::test_helpers::test_printer();
    let work = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let script_path = script_dir.path().join("greet.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho \"[$1][$2]\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entry = ScriptEntry::Simple("greet.sh \"hello world\"".into());

    let (_label, changed, captured) = execute_script(
        &entry,
        script_dir.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .expect("a quoted trailing argument must run");
    assert!(changed, "a zero-exit script reports changed=true");
    let out = captured.unwrap_or_default();
    assert!(
        out.contains("[hello world][]"),
        "the quoted argument must arrive as a single $1, leaving $2 empty: {out:?}"
    );
}

// C1 regression pin (end-to-end): a multi-line `run: |` body — a resolved
// leading token on the first line, an ordinary shell statement on the
// second — must run BOTH lines. Before this fix the whole string was
// tested for existence as one path (never matching, because of the
// embedded newline), so it fell into the same args-ification bug as the
// single-line case: only the first line ever ran.
#[cfg(unix)]
#[test]
fn execute_script_multiline_body_runs_every_line() {
    let printer = crate::test_helpers::test_printer();
    let work = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let script_path = script_dir.path().join("first.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho first\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entry = ScriptEntry::Simple("first.sh\necho second".into());

    let (_label, changed, captured) = execute_script(
        &entry,
        script_dir.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .expect("every line of a multi-line body must run");
    assert!(changed, "a zero-exit script reports changed=true");
    let out = captured.unwrap_or_default();
    assert!(out.contains("first"), "first line must run: {out:?}");
    assert!(
        out.contains("second"),
        "second line of the multi-line body must also run: {out:?}"
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
        ScriptReport::default(),
    )
    .expect_err("a non-zero exit must surface as Err");

    match err {
        CfgdError::Config(ConfigError::Invalid { message }) => {
            assert!(
                message.contains("failed (exit code 7)"),
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
    let entry = ScriptEntry::Full(ScriptCommand {
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
    });

    let err = execute_script(
        &entry,
        work.path(),
        work.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
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
    let cmd = build_inline_command(ScriptShell::Pwsh, "Get-Date", tmp.path(), None, true);
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
    let env = build_script_env(&ScriptEnvContext {
        config_dir: std::path::Path::new("/cfg"),
        profile_name: "node",
        context: ReconcileContext::Reconcile,
        phase: &ScriptPhase::OnDrift,
        module_name: None,
        module_dir: Some(std::path::Path::new("/mods/x")),
        path_dirs: &[],
    });
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

// Regression: a `run:` script whose body spans multiple lines must never
// hand a *rendered* status subject the raw body, since `Renderer::write_line`
// debug_asserts `!body.contains('\n')` — a release build would otherwise
// print the whole script down the terminal as if it were one status line.
// The `creates` guard triggers a `status_simple(Role::Skipped, ...)` call
// built from the condensed `run_label`, without ever spawning a shell, so
// this exercises the real reconciler render path on every OS.
//
// The function's RETURN value is a different string on purpose: it is
// `resource_desc`, the raw body, because callers push it straight into
// `ActionResult.description` for state-matching (see the comment above the
// `creates` guard in `scripts.rs`, and the analogous split in
// `format_action_description` / `apply_script_action`). It must keep the
// newline, not lose it.
#[test]
fn multi_line_inline_script_never_reaches_status_subject_with_newline() {
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    let tmp = tempfile::tempdir().unwrap();
    let entry = ScriptEntry::Full(ScriptCommand {
        workdir: None,
        run: "echo one\necho two\necho three".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        // "." always exists relative to `working_dir`, so the guard fires
        // unconditionally without ever spawning the (never-executed) body.
        creates: Some(".".to_string()),
        interactive: false,
    });

    let (desc, changed, _captured) = execute_script(
        &entry,
        tmp.path(),
        tmp.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
    )
    .expect("creates guard must skip cleanly, not error");

    assert!(!changed, "the creates guard must report a clean skip");
    assert!(
        desc.contains('\n') && desc.contains("echo two") && desc.contains("echo three"),
        "the persisted description must stay the raw multi-line body for state-matching: {desc:?}"
    );

    let rendered = crate::test_helpers::captured_text(&buf);
    assert!(
        rendered.contains("echo one"),
        "the first line must still reach the rendered skip subject: {rendered:?}"
    );
    assert!(
        !rendered.contains("echo two") && !rendered.contains("echo three"),
        "only the first line of a multi-line inline script may reach a status subject: {rendered:?}"
    );
}

// --- one status line per `execute_script`, whatever the exit ---

use crate::test_helpers::settled_status_lines as settled_lines;

fn script(run: &str) -> ScriptEntry {
    ScriptEntry::Full(ScriptCommand {
        run: run.into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
        workdir: None,
    })
}

fn with_guard(mut entry: ScriptEntry, f: impl FnOnce(&mut ScriptEntry)) -> ScriptEntry {
    f(&mut entry);
    entry
}

/// Write a file the OS will accept as executable but refuse to load, and return
/// its path. The pre-window spawn seam: `Command::spawn` fails AFTER the file
/// branch is chosen and BEFORE `output_window_at` opens anything, with an error
/// that is not `NotFound`. Unix spells it as a shebang naming an absent
/// interpreter, Windows as `.exe` bytes that are not a PE image.
#[cfg(unix)]
fn write_unspawnable(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("bad-shebang.sh");
    std::fs::write(&path, "#!/nonexistent/interp\ntrue\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(windows)]
fn write_unspawnable(dir: &std::path::Path) -> std::path::PathBuf {
    // `is_executable` answers true on the extension alone, so no mode bit is
    // needed and the file branch is still the one taken.
    let path = dir.join("not-an-image.exe");
    std::fs::write(&path, "this is not a PE image\n").unwrap();
    path
}

/// The exit table's shell bodies, spelled for the shell `ScriptShell::Auto`
/// resolves to: `sh -c` on Unix, `cmd.exe /C` on Windows.
#[cfg(unix)]
mod body {
    pub const OK: &str = "true";
    pub const FAIL: &str = "exit 1";
    pub const FAIL_3: &str = "exit 3";
    /// Outlives the 50ms timeout the guard-timeout row drives.
    pub const SLOW: &str = "sleep 5";
}

#[cfg(windows)]
mod body {
    pub const OK: &str = "exit 0";
    pub const FAIL: &str = "exit 1";
    pub const FAIL_3: &str = "exit 3";
    /// `ping -n` rather than `timeout /t`: `timeout` reads the console input
    /// handle and fails outright when stdin is redirected, which it is here.
    pub const SLOW: &str = "ping -n 6 127.0.0.1";
}

/// Run one entry through the shipped wrapper and return the settled lines it
/// emitted, with `stdin_is_tty` supplied so the interactive arm is reachable.
fn drive(entry: &ScriptEntry, stdin_is_tty: bool, timeout_ms: u64) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let (printer, cap) = crate::output::Printer::for_test_doc();
    let _ = super::execute_script_with_tty(
        stdin_is_tty,
        entry,
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_millis(timeout_ms),
        &printer,
        None,
        None,
        ScriptReport::default(),
    );
    drop(printer);
    settled_lines(&crate::output::strip_ansi(&cap.human()))
}

#[test]
fn every_script_exit_emits_one_status() {
    let spawn_dir = tempfile::tempdir().unwrap();
    let existing = tempfile::tempdir().unwrap();
    let creates_path = existing.path().join("already-there");
    std::fs::write(&creates_path, "x").unwrap();
    // Absolute, because `drive` runs every case from a tempdir of its own and a
    // relative name that resolves to nothing lands in the inline-command
    // branch instead — a case that passes for the wrong reason.
    let unspawnable = script(&write_unspawnable(spawn_dir.path()).display().to_string());

    let cases: Vec<(&str, ScriptEntry, bool, u64, char)> = vec![
        (
            "creates path exists",
            with_guard(script(body::OK), |e| {
                if let ScriptEntry::Full(ScriptCommand { creates, .. }) = e {
                    *creates = Some(creates_path.display().to_string());
                }
            }),
            false,
            5_000,
            '\u{2205}',
        ),
        (
            "onlyIf fails",
            with_guard(script(body::OK), |e| {
                if let ScriptEntry::Full(ScriptCommand { only_if, .. }) = e {
                    *only_if = Some(body::FAIL.to_string());
                }
            }),
            false,
            5_000,
            '\u{2205}',
        ),
        (
            "unless holds",
            with_guard(script(body::OK), |e| {
                if let ScriptEntry::Full(ScriptCommand { unless, .. }) = e {
                    *unless = Some(body::OK.to_string());
                }
            }),
            false,
            5_000,
            '\u{2205}',
        ),
        (
            "interactive without a tty",
            with_guard(script(body::OK), |e| {
                if let ScriptEntry::Full(ScriptCommand { interactive, .. }) = e {
                    *interactive = true;
                }
            }),
            false,
            5_000,
            '\u{26A0}',
        ),
        (
            "interactive success",
            with_guard(script(body::OK), |e| {
                if let ScriptEntry::Full(ScriptCommand { interactive, .. }) = e {
                    *interactive = true;
                }
            }),
            true,
            5_000,
            '\u{2713}',
        ),
        (
            "interactive failure",
            with_guard(script(body::FAIL_3), |e| {
                if let ScriptEntry::Full(ScriptCommand { interactive, .. }) = e {
                    *interactive = true;
                }
            }),
            true,
            5_000,
            '\u{2717}',
        ),
        (
            "windowed success",
            script(body::OK),
            false,
            5_000,
            '\u{2713}',
        ),
        (
            "windowed failure",
            script(body::FAIL),
            false,
            5_000,
            '\u{2717}',
        ),
        ("unspawnable image", unspawnable, false, 5_000, '\u{2717}'),
        (
            // The guard body outlives the timeout, so `run_guard_command`
            // returns a real error before any window is opened. No absent
            // binary is needed and nothing spawned can hang the suite.
            "guard command times out",
            with_guard(script(body::OK), |e| {
                if let ScriptEntry::Full(ScriptCommand { only_if, .. }) = e {
                    *only_if = Some(body::SLOW.to_string());
                }
            }),
            false,
            50,
            '\u{2717}',
        ),
    ];

    for (label, entry, tty, timeout_ms, glyph) in cases {
        let lines = drive(&entry, tty, timeout_ms);
        assert_eq!(
            lines.len(),
            1,
            "{label}: expected one status, got {lines:?}"
        );
        assert!(
            lines[0].starts_with(glyph),
            "{label}: expected role glyph {glyph}, got {}",
            lines[0]
        );
    }

    // The interactive-success row asserts more than its glyph: a silent `Ok`
    // rendered as a `Fail` is exactly what the outcome-branching tail exists
    // to prevent, so it must carry a duration and no ` — ` detail.
    let interactive_ok = drive(
        &with_guard(script(body::OK), |e| {
            if let ScriptEntry::Full(ScriptCommand { interactive, .. }) = e {
                *interactive = true;
            }
        }),
        true,
        5_000,
    );
    assert!(
        !interactive_ok[0].contains(" \u{2014} "),
        "an attended success carries no error detail: {}",
        interactive_ok[0]
    );
    assert!(
        interactive_ok[0].ends_with("s)"),
        "an attended success carries its elapsed duration: {}",
        interactive_ok[0]
    );
}

#[test]
fn unspawnable_script_emits_one_status_without_opening_a_window() {
    // `resolved.exists()` and the exec check both pass, so the file branch is
    // taken and the spawn fails on the IMAGE — above `output_window_at`, which
    // is the ordering this test pins.
    let dir = tempfile::tempdir().unwrap();
    let path = write_unspawnable(dir.path());
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("the helper names its own file");

    let (printer, cap) = crate::output::Printer::for_test_doc();
    let result = execute_script(
        &script(name),
        dir.path(),
        dir.path(),
        &[],
        std::time::Duration::from_secs(5),
        &printer,
        None,
        None,
        ScriptReport::default(),
    );
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());

    assert!(result.is_err(), "an unloadable image must not succeed");
    let lines = settled_lines(&out);
    assert_eq!(lines.len(), 1, "exactly one status line: {out}");
    assert!(lines[0].starts_with('\u{2717}'), "got: {}", lines[0]);
    assert!(
        lines[0].contains(" \u{2014} "),
        "the collapsed spawn error is the detail: {}",
        lines[0]
    );
    assert!(
        !out.contains('\u{25D0}'),
        "no window may open before the spawn fails: {out}"
    );
    assert!(
        !out.contains('\u{25C9}'),
        "a dropped window's Info line is the two-line regression: {out}"
    );
}

#[test]
fn script_failure_role_follows_non_fatal() {
    let mut rendered = Vec::new();
    for non_fatal in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (printer, cap) = crate::output::Printer::for_test_doc();
        let _ = execute_script(
            &script(body::FAIL),
            dir.path(),
            dir.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
            ScriptReport {
                subject: ScriptSubject::Bare,
                non_fatal,
                ..ScriptReport::default()
            },
        );
        drop(printer);
        let mut lines = settled_lines(&crate::output::strip_ansi(&cap.human()));
        assert_eq!(lines.len(), 1, "one line per invocation: {lines:?}");
        rendered.push(lines.remove(0));
    }

    assert!(rendered[0].starts_with('\u{2717}'), "got: {}", rendered[0]);
    assert!(rendered[1].starts_with('\u{26A0}'), "got: {}", rendered[1]);
    // Asserted before the placeholdering below, which would otherwise let a
    // renderer that stopped emitting a duration pass unnoticed.
    for line in &rendered {
        assert!(
            line.ends_with("s)"),
            "each failure carries a duration: {line}"
        );
    }
    // Two separately-spawned processes settle on either side of the
    // tenth-of-a-second floor often enough that comparing the lines verbatim
    // asserts on the host's scheduler; the ONE normalizer knows the `<0.1s`
    // floor spelling, which a local scan of digits did not.
    assert_eq!(
        crate::normalize_snapshot_durations(rendered[0].trim_start_matches('\u{2717}')),
        crate::normalize_snapshot_durations(rendered[1].trim_start_matches('\u{26A0}')),
        "only the role differs between a fatal and a non-fatal failure"
    );
}

#[test]
fn script_status_fail_after_window_emits_one_fail() {
    // The post-window `?` — `child.try_wait()`, a `waitpid` failure no test can
    // provoke portably — driven on the type that makes a second line
    // impossible.
    let (printer, cap) = crate::output::Printer::for_test_doc();
    {
        let mut st = ScriptStatus::new(
            &printer,
            "exit 1",
            ScriptReport {
                subject: ScriptSubject::Hook("postApply"),
                non_fatal: false,
                ..ScriptReport::default()
            },
        );
        st.open_window();
        st.finish_fail("waitpid failed", None);
    }
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());
    let lines = settled_lines(&out);

    assert_eq!(lines.len(), 1, "exactly one settled line: {out}");
    assert!(lines[0].starts_with('\u{2717}'), "got: {}", lines[0]);
    assert!(
        lines[0].contains("postApply: exit 1"),
        "the marked subject, never the spinner's label: {}",
        lines[0]
    );
    assert!(
        !out.contains('\u{25C9}'),
        "the window was finished, not dropped: {out}"
    );
}

#[test]
fn script_status_status_after_open_window_emits_one_line() {
    let (printer, cap) = crate::output::Printer::for_test_doc();
    {
        let mut st = ScriptStatus::new(
            &printer,
            "exit 1",
            ScriptReport {
                subject: ScriptSubject::Hook("postApply"),
                non_fatal: false,
                ..ScriptReport::default()
            },
        );
        st.open_window();
        st.status(crate::output::Role::Skipped, Some("creates path exists"));
    }
    drop(printer);
    let out = crate::output::strip_ansi(&cap.human());
    let lines = settled_lines(&out);

    assert_eq!(lines.len(), 1, "exactly one settled line: {out}");
    assert!(lines[0].starts_with('\u{2205}'), "got: {}", lines[0]);
    assert!(
        lines[0].contains("postApply: exit 1"),
        "the marked subject: {}",
        lines[0]
    );
    assert!(
        !out.contains("(interrupted)"),
        "the window was finished explicitly via status(), not dropped: {out}"
    );
}
