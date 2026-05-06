use cfgd_core::config::PackagesSpec;
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::*;

#[test]
fn scripted_manager_from_spec() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "mypm".to_string(),
        check: "which mypm".to_string(),
        list_installed: "mypm list".to_string(),
        install: "mypm install {package}".to_string(),
        uninstall: "mypm remove {packages}".to_string(),
        update: Some("mypm update".to_string()),
        packages: vec!["foo".to_string(), "bar".to_string()],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    assert_eq!(mgr.name(), "mypm");
    assert!(!mgr.can_bootstrap());
}

#[test]
fn scripted_manager_install_uses_sh() {
    // ScriptedManager with a command that always succeeds
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "testpm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo installing {package}".to_string(),
        uninstall: "echo removing {package}".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.install(&["pkg1".to_string()], &printer).unwrap();
}

#[test]
fn scripted_manager_batch_mode() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "batch".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo {packages}".to_string(),
        uninstall: "echo {packages}".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.install(
        &["a".to_string(), "b".to_string(), "c".to_string()],
        &printer,
    )
    .unwrap();
}

#[test]
fn scripted_manager_is_available() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "avail".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    assert!(mgr.is_available());

    let spec_unavail = cfgd_core::config::CustomManagerSpec {
        name: "unavail".to_string(),
        check: "false".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr2 = ScriptedManager::from_spec(&spec_unavail);
    assert!(!mgr2.is_available());
}

#[test]
fn scripted_manager_installed_packages() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "listtest".to_string(),
        check: "true".to_string(),
        list_installed: "printf 'alpha\\nbeta\\ngamma\\n'".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let installed = mgr.installed_packages().unwrap();
    assert_eq!(installed.len(), 3);
    assert!(installed.contains("alpha"));
    assert!(installed.contains("beta"));
    assert!(installed.contains("gamma"));
}

#[test]
fn scripted_manager_install_failure() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "failpm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "exit 1".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.install(&["pkg".to_string()], &printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failpm install failed"), "got: {msg}");
}

#[test]
fn custom_managers_creates_from_specs() {
    let specs = vec![
        cfgd_core::config::CustomManagerSpec {
            name: "pm1".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        },
        cfgd_core::config::CustomManagerSpec {
            name: "pm2".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        },
    ];
    let managers = custom_managers(&specs);
    assert_eq!(managers.len(), 2);
    assert_eq!(managers[0].name(), "pm1");
    assert_eq!(managers[1].name(), "pm2");
}

#[test]
fn scripted_manager_update_noop_when_no_cmd() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "noup".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.update(&printer).unwrap();
}

#[test]
fn custom_manager_config_parsing() {
    let yaml = r#"
custom:
  - name: mise
    check: "command -v mise"
    listInstalled: "mise list --installed --json | jq -r 'keys[]'"
    install: "mise install {package}"
    uninstall: "mise uninstall {package}"
    packages:
      - node@20
      - python@3.12
"#;
    let packages: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(packages.custom.len(), 1);
    let cm = &packages.custom[0];
    assert_eq!(cm.name, "mise");
    assert_eq!(cm.install, "mise install {package}");
    assert_eq!(cm.packages, vec!["node@20", "python@3.12"]);
    assert!(cm.update.is_none());
}

#[test]
fn scripted_manager_empty_packages_is_noop() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "noop".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo {package}".to_string(),
        uninstall: "echo {packages}".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    // Empty packages should be a no-op (returns Ok immediately)
    mgr.install(&[], &printer).unwrap();
    mgr.uninstall(&[], &printer).unwrap();
}

#[test]
fn scripted_manager_batch_append_mode() {
    // Template without {package} or {packages} → packages appended
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "appendpm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo install".to_string(),
        uninstall: "echo remove".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    // Should succeed — command becomes "echo install pkg1 pkg2"
    mgr.install(&["pkg1".to_string(), "pkg2".to_string()], &printer)
        .unwrap();
}

#[test]
fn scripted_manager_uninstall_one_at_a_time() {
    // Template with {package} → runs once per package
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "onepm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo removing {package}".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.uninstall(&["a".to_string(), "b".to_string()], &printer)
        .unwrap();
}

#[test]
fn scripted_manager_available_version_always_none() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "noversion".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    assert!(mgr.available_version("anything").unwrap().is_none());
}

#[test]
fn scripted_manager_update_runs_command() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "uppm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: Some("echo updating".to_string()),
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.update(&printer).unwrap();
}

#[test]
fn scripted_manager_update_failure() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "failup".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: Some("exit 1".to_string()),
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.update(&printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failup") && msg.contains("install failed"),
        "got: {msg}"
    );
}

#[test]
fn custom_managers_empty_specs() {
    let managers = custom_managers(&[]);
    assert!(managers.is_empty());
}

#[test]
fn custom_managers_preserves_names() {
    let specs = vec![
        cfgd_core::config::CustomManagerSpec {
            name: "alpha".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: Some("echo update".to_string()),
            packages: vec!["pkg1".to_string()],
        },
        cfgd_core::config::CustomManagerSpec {
            name: "beta".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        },
        cfgd_core::config::CustomManagerSpec {
            name: "gamma".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec!["a".to_string(), "b".to_string()],
        },
    ];
    let managers = custom_managers(&specs);
    assert_eq!(managers.len(), 3);
    assert_eq!(managers[0].name(), "alpha");
    assert_eq!(managers[1].name(), "beta");
    assert_eq!(managers[2].name(), "gamma");
    // All should not be bootstrappable
    for m in &managers {
        assert!(!m.can_bootstrap());
    }
}

#[test]
fn scripted_manager_from_spec_preserves_all_fields() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "test-pm".to_string(),
        check: "which test-pm".to_string(),
        list_installed: "test-pm list".to_string(),
        install: "test-pm install {package}".to_string(),
        uninstall: "test-pm remove {packages}".to_string(),
        update: Some("test-pm update".to_string()),
        packages: vec!["a".to_string(), "b".to_string()],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    assert_eq!(mgr.name(), "test-pm");
    assert_eq!(mgr.check_cmd, "which test-pm");
    assert_eq!(mgr.list_cmd, "test-pm list");
    assert_eq!(mgr.install_cmd, "test-pm install {package}");
    assert_eq!(mgr.uninstall_cmd, "test-pm remove {packages}");
    assert_eq!(mgr.update_cmd, Some("test-pm update".to_string()));
}

#[test]
fn scripted_manager_shell_escapes_packages() {
    // Verify that a package name with special characters is handled
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "escapepm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo {package}".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    // Package name with spaces and special chars
    mgr.install(&["pkg with spaces".to_string()], &printer)
        .unwrap();
}

#[test]
fn scripted_manager_uninstall_failure_reports_correct_error() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "failrm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "exit 1".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.uninstall(&["pkg".to_string()], &printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    // run_pkg_cmd_msg with error_kind "uninstall" maps to UninstallFailed
    assert!(
        msg.contains("failrm") && msg.contains("uninstall failed"),
        "got: {msg}"
    );
}

#[test]
fn scripted_manager_list_failure_reports_correct_error() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "faillist".to_string(),
        check: "true".to_string(),
        list_installed: "exit 1".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let result = mgr.installed_packages();
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("faillist") && msg.contains("list"),
        "expected list error, got: {msg}"
    );
}

#[test]
fn scripted_manager_per_package_error_includes_package_name_as_prefix() {
    // {package} template → run_pkg_cmd_msg with msg_prefix = the package name
    // Verifies that the per-package error message includes both the manager
    // name AND the specific package that failed (via msg_prefix in run_pkg_cmd_msg)
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "prefixpm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "sh -c 'echo dependency-conflict >&2; exit 1' # {package}".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.install(&["my-pkg".to_string()], &printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    // The error should contain the package name as prefix AND the stderr content
    assert!(
        msg.contains("my-pkg") && msg.contains("dependency-conflict"),
        "expected package name prefix and stderr in error, got: {msg}"
    );
}

#[test]
fn scripted_manager_batch_install_failure_is_install_error() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "batchfail".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "exit 1".to_string(), // no {package}/{packages} → batch mode → run_pkg_cmd
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.install(&["pkg".to_string()], &printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("install failed"),
        "expected install error, got: {msg}"
    );
}

#[test]
fn scripted_manager_batch_uninstall_failure() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "batchrmfail".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "exit 1".to_string(), // no {package}/{packages} → batch mode
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.uninstall(&["pkg".to_string()], &printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("uninstall failed"),
        "expected uninstall error, got: {msg}"
    );
}

#[test]
fn scripted_manager_install_stderr_in_error_message() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "stderrpm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "sh -c 'echo custom-error-text >&2; exit 1'".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.install(&["pkg".to_string()], &printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    // run_pkg_cmd captures stderr and includes it in the error message
    assert!(
        msg.contains("custom-error-text"),
        "expected stderr in error, got: {msg}"
    );
}

#[test]
fn scripted_manager_list_stderr_in_error_message() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "stderrlist".to_string(),
        check: "true".to_string(),
        list_installed: "sh -c 'echo list-error-text >&2; exit 1'".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let result = mgr.installed_packages();
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("list-error-text"),
        "expected stderr in list error, got: {msg}"
    );
}

#[test]
fn scripted_manager_update_failure_includes_stderr() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "upfail".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: Some("sh -c 'echo update-err >&2; exit 1'".to_string()),
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.update(&printer);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("update-err"),
        "expected stderr in update error, got: {msg}"
    );
}

#[test]
fn scripted_manager_from_spec_no_update() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "noupd".to_string(),
        check: "which noupd".to_string(),
        list_installed: "noupd list".to_string(),
        install: "noupd add {packages}".to_string(),
        uninstall: "noupd rm {packages}".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    assert_eq!(mgr.mgr_name, "noupd");
    assert_eq!(mgr.check_cmd, "which noupd");
    assert_eq!(mgr.list_cmd, "noupd list");
    assert_eq!(mgr.install_cmd, "noupd add {packages}");
    assert_eq!(mgr.uninstall_cmd, "noupd rm {packages}");
    assert!(mgr.update_cmd.is_none());
}

#[test]
fn scripted_manager_list_failure_is_list_error_variant() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "listerr".to_string(),
        check: "true".to_string(),
        list_installed: "sh -c 'echo permission-denied >&2; exit 1'".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let result = mgr.installed_packages();
    let err = result.unwrap_err();
    let msg = err.to_string();
    // The error goes through run_pkg_cmd with error_kind="list"
    // which maps to PackageError::ListFailed
    assert!(
        msg.contains("list") && msg.contains("permission-denied"),
        "expected list error with stderr, got: {msg}"
    );
}

#[test]
fn scripted_manager_packages_plural_template() {
    // {packages} template uses batch mode with explicit replacement
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "pluralpm".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo installing {packages} done".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    // Should succeed — {packages} replaced with "a b c"
    mgr.install(
        &["a".to_string(), "b".to_string(), "c".to_string()],
        &printer,
    )
    .unwrap();
}

#[test]
fn scripted_manager_per_package_stops_on_first_failure() {
    let spec = cfgd_core::config::CustomManagerSpec {
            name: "stoppm".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install:
                "sh -c 'if [ \"$(echo {package} | tr -d \"'\\''\" )\" = \"fail-pkg\" ]; then exit 1; fi'"
                    .to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec![],
        };
    let mgr = ScriptedManager::from_spec(&spec);
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    let result = mgr.install(
        &[
            "ok-pkg".to_string(),
            "fail-pkg".to_string(),
            "never-reached".to_string(),
        ],
        &printer,
    );
    assert!(result.is_err());
}

#[test]
fn scripted_manager_available_version_returns_none_for_any_input() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "versiontest".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr = ScriptedManager::from_spec(&spec);
    assert!(mgr.available_version("anything").unwrap().is_none());
    assert!(mgr.available_version("").unwrap().is_none());
    assert!(mgr.available_version("complex/pkg@1.0").unwrap().is_none());
}

#[test]
fn custom_managers_all_return_none_for_version() {
    let specs = vec![cfgd_core::config::CustomManagerSpec {
        name: "pm1".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    }];
    let managers = custom_managers(&specs);
    for m in &managers {
        assert!(m.available_version("any").unwrap().is_none());
        assert!(!m.can_bootstrap());
    }
}

#[test]
fn scripted_manager_is_available_through_trait() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "traitcheck".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr: Box<dyn PackageManager> = Box::new(ScriptedManager::from_spec(&spec));
    // Exercise is_available through the trait object
    assert!(mgr.is_available());
    assert!(!mgr.can_bootstrap());
    assert!(mgr.available_version("anything").unwrap().is_none());
}

#[test]
fn scripted_manager_bootstrap_through_trait() {
    let spec = cfgd_core::config::CustomManagerSpec {
        name: "boottest".to_string(),
        check: "true".to_string(),
        list_installed: "echo".to_string(),
        install: "echo".to_string(),
        uninstall: "echo".to_string(),
        update: None,
        packages: vec![],
    };
    let mgr: Box<dyn PackageManager> = Box::new(ScriptedManager::from_spec(&spec));
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
    mgr.bootstrap(&printer).unwrap();
}
