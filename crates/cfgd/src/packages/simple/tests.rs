use cfgd_core::command_available;
use cfgd_core::providers::PackageManager;

use super::*;

#[test]
fn simple_manager_display_cmd_shows_packages() {
    let mgr = apt_manager();
    let label = mgr.display_cmd(
        &["sudo", "apt-get", "install", "-y"],
        &["curl".to_string(), "wget".to_string()],
    );
    // display_cmd calls strip_sudo_if_root; as non-root in tests, sudo stays
    // It concatenates effective cmd + packages
    assert!(label.contains("apt-get"));
    assert!(label.contains("install"));
    assert!(label.contains("curl"));
    assert!(label.contains("wget"));
}

#[test]
fn simple_manager_display_cmd_empty_packages() {
    let mgr = apt_manager();
    let label = mgr.display_cmd(&["sudo", "apt-get", "update"], &[]);
    assert!(label.contains("apt-get"));
    assert!(label.contains("update"));
}

#[test]
fn apt_manager_has_correct_fields() {
    let mgr = apt_manager();
    assert_eq!(mgr.name(), "apt");
    assert!(!mgr.can_bootstrap());
    // list_cmd should use dpkg-query
    assert_eq!(mgr.list_cmd[0], "dpkg-query");
    // install_cmd should include sudo and -y
    assert!(mgr.install_cmd.contains(&"sudo"));
    assert!(mgr.install_cmd.contains(&"-y"));
    // uninstall_cmd should include sudo and -y
    assert!(mgr.uninstall_cmd.contains(&"sudo"));
    assert!(mgr.uninstall_cmd.contains(&"-y"));
    // should have update_cmd
    assert!(mgr.update_cmd.is_some());
    // should not ignore update exit
    assert!(!mgr.ignore_update_exit);
    // should have list_with_versions
    assert!(mgr.list_with_versions.is_some());
    // should have aliases
    assert!(mgr.aliases_fn.is_some());
}

#[test]
fn dnf_manager_has_correct_fields() {
    let mgr = dnf_manager();
    assert_eq!(mgr.name(), "dnf");
    assert!(!mgr.can_bootstrap());
    assert!(mgr.install_cmd.contains(&"sudo"));
    assert!(mgr.install_cmd.contains(&"-y"));
    // dnf ignores update exit (check-update returns 100 for available updates)
    assert!(mgr.ignore_update_exit);
    assert!(mgr.list_with_versions.is_some());
    assert!(mgr.aliases_fn.is_some());
}

#[test]
fn yum_manager_has_correct_fields() {
    let mgr = yum_manager();
    assert_eq!(mgr.name(), "yum");
    assert!(!mgr.can_bootstrap());
    assert!(mgr.install_cmd.contains(&"sudo"));
    // yum also ignores update exit
    assert!(mgr.ignore_update_exit);
    // yum has a custom is_available_fn
    assert!(mgr.is_available_fn.is_some());
}

#[test]
fn apk_manager_has_correct_fields() {
    let mgr = apk_manager();
    assert_eq!(mgr.name(), "apk");
    // apk doesn't use sudo in install_cmd (Alpine runs as root)
    assert!(!mgr.install_cmd.contains(&"sudo"));
    assert!(!mgr.ignore_update_exit);
    // apk has no list_with_versions override
    assert!(mgr.list_with_versions.is_none());
    // apk has no aliases
    assert!(mgr.aliases_fn.is_none());
}

#[test]
fn pacman_manager_has_correct_fields() {
    let mgr = pacman_manager();
    assert_eq!(mgr.name(), "pacman");
    assert!(mgr.install_cmd.contains(&"sudo"));
    assert!(mgr.install_cmd.contains(&"--noconfirm"));
    assert!(!mgr.ignore_update_exit);
    assert!(mgr.aliases_fn.is_none());
}

#[test]
fn zypper_manager_has_correct_fields() {
    let mgr = zypper_manager();
    assert_eq!(mgr.name(), "zypper");
    assert!(mgr.install_cmd.contains(&"sudo"));
    assert!(mgr.install_cmd.contains(&"-y"));
    assert!(!mgr.ignore_update_exit);
}

#[test]
fn pkg_manager_has_correct_fields() {
    let mgr = pkg_manager();
    assert_eq!(mgr.name(), "pkg");
    // FreeBSD pkg doesn't use sudo
    assert!(!mgr.install_cmd.contains(&"sudo"));
    assert!(mgr.install_cmd.contains(&"-y"));
    assert!(!mgr.ignore_update_exit);
}

#[test]
fn simple_manager_name_matches() {
    let managers: Vec<SimpleManager> = vec![
        apt_manager(),
        dnf_manager(),
        yum_manager(),
        apk_manager(),
        pacman_manager(),
        zypper_manager(),
        pkg_manager(),
    ];
    let expected_names = ["apt", "dnf", "yum", "apk", "pacman", "zypper", "pkg"];
    for (mgr, expected) in managers.iter().zip(expected_names.iter()) {
        assert_eq!(mgr.name(), *expected);
    }
}

#[test]
fn simple_manager_none_can_bootstrap() {
    let managers: Vec<SimpleManager> = vec![
        apt_manager(),
        dnf_manager(),
        apk_manager(),
        pacman_manager(),
        zypper_manager(),
        pkg_manager(),
    ];
    for mgr in &managers {
        assert!(
            !mgr.can_bootstrap(),
            "{} should not be bootstrappable",
            mgr.name()
        );
    }
}

#[test]
fn all_simple_managers_have_list_cmd() {
    let managers = [
        apt_manager(),
        dnf_manager(),
        yum_manager(),
        apk_manager(),
        pacman_manager(),
        zypper_manager(),
        pkg_manager(),
    ];
    for mgr in &managers {
        assert!(
            !mgr.list_cmd.is_empty(),
            "{} should have list_cmd",
            mgr.name()
        );
        assert!(
            !mgr.install_cmd.is_empty(),
            "{} should have install_cmd",
            mgr.name()
        );
        assert!(
            !mgr.uninstall_cmd.is_empty(),
            "{} should have uninstall_cmd",
            mgr.name()
        );
    }
}

#[test]
fn all_simple_managers_have_update_cmd() {
    let managers = [
        apt_manager(),
        dnf_manager(),
        yum_manager(),
        apk_manager(),
        pacman_manager(),
        zypper_manager(),
        pkg_manager(),
    ];
    for mgr in &managers {
        assert!(
            mgr.update_cmd.is_some(),
            "{} should have update_cmd",
            mgr.name()
        );
    }
}

#[test]
fn simple_managers_without_aliases_return_empty() {
    let managers = [
        apk_manager(),
        pacman_manager(),
        zypper_manager(),
        pkg_manager(),
    ];
    for mgr in &managers {
        let aliases = mgr.package_aliases("fd").unwrap();
        assert!(aliases.is_empty(), "{} should have no aliases", mgr.name());
    }
}

#[test]
fn apt_manager_install_cmd_is_apt_get() {
    let mgr = apt_manager();
    assert_eq!(mgr.install_cmd[0], "sudo");
    assert_eq!(mgr.install_cmd[1], "apt-get");
    assert_eq!(mgr.install_cmd[2], "install");
    assert_eq!(mgr.install_cmd[3], "-y");
}

#[test]
fn dnf_manager_list_cmd_uses_quiet() {
    let mgr = dnf_manager();
    assert!(mgr.list_cmd.contains(&"--quiet"));
}

#[test]
fn apk_manager_install_cmd_is_add() {
    let mgr = apk_manager();
    assert_eq!(mgr.install_cmd, &["apk", "add"]);
}

#[test]
fn pacman_manager_install_uses_noconfirm() {
    let mgr = pacman_manager();
    assert!(mgr.install_cmd.contains(&"--noconfirm"));
    assert!(mgr.uninstall_cmd.contains(&"--noconfirm"));
}

#[test]
fn zypper_manager_list_cmd_searches_installed() {
    let mgr = zypper_manager();
    assert!(mgr.list_cmd.contains(&"--installed-only"));
    assert!(mgr.list_cmd.contains(&"--type"));
    assert!(mgr.list_cmd.contains(&"package"));
}

#[test]
fn pkg_manager_install_uses_dash_y() {
    let mgr = pkg_manager();
    assert_eq!(mgr.install_cmd, &["pkg", "install", "-y"]);
    assert_eq!(mgr.uninstall_cmd, &["pkg", "remove", "-y"]);
}

#[test]
fn yum_manager_is_available_uses_custom_fn() {
    // yum_manager has is_available_fn that checks !dnf && yum
    // This exercises the is_available_fn dispatch path (line 861-863)
    let yum = yum_manager();
    // On most CI systems, neither yum nor dnf is available, so this returns false.
    // The key is that it exercises the is_available_fn dispatch path.
    let available = yum.is_available();
    // If dnf is available, yum should NOT be available (they're mutually exclusive)
    if command_available("dnf") {
        assert!(
            !available,
            "yum should not be available when dnf is present"
        );
    }
}

#[test]
fn simple_manager_is_available_without_custom_fn() {
    // apk_manager has is_available_fn = None, uses default command_available
    let apk = apk_manager();
    let _available = apk.is_available(); // exercises the None branch (line 864)
}

#[test]
fn simple_manager_bootstrap_is_noop() {
    let apt = apt_manager();
    let printer = cfgd_core::test_helpers::test_printer();
    apt.bootstrap(&printer).unwrap();
}

#[test]
fn only_dnf_and_yum_ignore_update_exit() {
    let managers = [
        ("apt", apt_manager()),
        ("dnf", dnf_manager()),
        ("yum", yum_manager()),
        ("apk", apk_manager()),
        ("pacman", pacman_manager()),
        ("zypper", zypper_manager()),
        ("pkg", pkg_manager()),
    ];
    for (name, mgr) in &managers {
        let expected = *name == "dnf" || *name == "yum";
        assert_eq!(
            mgr.ignore_update_exit, expected,
            "{} ignore_update_exit mismatch",
            name
        );
    }
}

#[test]
fn simple_manager_parse_list_fns_are_set() {
    // Verify each manager uses the correct parse function
    let apt = apt_manager();
    let apt_result = (apt.parse_list)("curl\nwget\n");
    assert!(apt_result.contains("curl"));

    let dnf = dnf_manager();
    let dnf_result = (dnf.parse_list)("bash.x86_64 5.2 @base\n");
    assert!(dnf_result.contains("bash"));

    let yum = yum_manager();
    let yum_result = (yum.parse_list)("Loaded plugins\nvim.x86_64 8.2 @base\n");
    assert!(yum_result.contains("vim"));

    let apk = apk_manager();
    let apk_result = (apk.parse_list)("curl-7.88.1-r1\n");
    assert!(apk_result.contains("curl"));

    let pacman = pacman_manager();
    let pacman_result = (pacman.parse_list)("vim\ngit\n");
    assert!(pacman_result.contains("vim"));

    let zypper = zypper_manager();
    let zypper_result = (zypper.parse_list)("i | vim | 9.0\n");
    assert!(zypper_result.contains("vim"));

    let pkg = pkg_manager();
    let pkg_result = (pkg.parse_list)("curl-7.88.1\n");
    assert!(pkg_result.contains("curl"));
}

#[test]
fn simple_manager_query_version_fns_handle_missing_commands() {
    // On CI without these package managers, the commands will fail gracefully
    // This exercises the query_version function pointer dispatch in available_version()
    let managers: Vec<SimpleManager> = vec![
        apt_manager(),
        dnf_manager(),
        apk_manager(),
        pacman_manager(),
        zypper_manager(),
        pkg_manager(),
    ];
    for mgr in &managers {
        // available_version dispatches to (self.query_version)(self.mgr_name, package)
        // The underlying functions handle command-not-found gracefully
        let _result = mgr.available_version("nonexistent-package-12345");
        // We don't assert on the result because it depends on system state,
        // but this exercises the dispatch path
    }
}

#[test]
fn simple_manager_display_cmd_concatenates_correctly() {
    let mgr = dnf_manager();
    let label = mgr.display_cmd(
        &["sudo", "dnf", "install", "-y"],
        &["vim".to_string(), "git".to_string()],
    );
    // Exercises strip_sudo_if_root within display_cmd
    if cfgd_core::is_root() {
        assert!(!label.starts_with("sudo"));
    }
    assert!(label.contains("dnf"));
    assert!(label.contains("vim"));
    assert!(label.contains("git"));
}

#[cfg(unix)]
mod seam_tests {
    use serial_test::serial;

    use cfgd_core::providers::PackageManager;
    use cfgd_core::test_helpers::{ToolShim, test_printer};

    use super::super::{
        APK_BIN_ENV, APT_GET_BIN_ENV, DNF_BIN_ENV, DPKG_QUERY_BIN_ENV, PACMAN_BIN_ENV, PKG_BIN_ENV,
        ZYPPER_BIN_ENV, apk_manager, apt_manager, dnf_manager, pacman_manager, pkg_manager,
        zypper_manager,
    };

    #[test]
    #[serial]
    fn apt_manager_installed_packages_honors_dpkg_query_seam() {
        let _shim = ToolShim::install(DPKG_QUERY_BIN_ENV, 0, "curl\nwget\nbash\n", "");
        let pkgs = apt_manager()
            .installed_packages()
            .expect("installed_packages should succeed with shim");
        assert!(pkgs.contains("curl"));
        assert!(pkgs.contains("wget"));
        assert!(pkgs.contains("bash"));
    }

    #[test]
    #[serial]
    fn install_empty_package_list_is_noop_and_does_not_invoke_shim() {
        // The empty-list early-return must NOT invoke the underlying package
        // manager — a stray invocation would mean cfgd shipped `apt-get install`
        // with no packages, which apt-get treats as success but is wasteful.
        let shim = ToolShim::install(APT_GET_BIN_ENV, 0, "", "");
        let printer = test_printer();
        apt_manager().install(&[], &printer).unwrap();
        assert_eq!(shim.invocation_count(), 0, "shim must not be invoked");
    }

    #[test]
    #[serial]
    fn uninstall_empty_package_list_is_noop_and_does_not_invoke_shim() {
        let shim = ToolShim::install(APT_GET_BIN_ENV, 0, "", "");
        let printer = test_printer();
        apt_manager().uninstall(&[], &printer).unwrap();
        assert_eq!(shim.invocation_count(), 0, "shim must not be invoked");
    }

    /// install / uninstall / update tests below depend on `strip_sudo_if_root`
    /// removing the leading `sudo` so the shimmed binary (apt-get, dnf, etc.) is
    /// the actual program invoked — the `sudo` fallback would bypass the seam.
    /// Each test guards itself with `cfgd_core::is_root()`.
    #[test]
    #[serial]
    fn apt_install_invokes_apt_get_shim_with_package_args_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        let shim = ToolShim::install(APT_GET_BIN_ENV, 0, "", "");
        let printer = test_printer();
        apt_manager()
            .install(&["curl".into(), "wget".into()], &printer)
            .unwrap();
        let log = shim.argv_log();
        assert_eq!(shim.invocation_count(), 1, "single install invocation");
        assert!(log.contains("install"), "must invoke install subcmd: {log}");
        assert!(log.contains("-y"), "must pass -y flag: {log}");
        assert!(log.contains("curl"), "must include curl in argv: {log}");
        assert!(log.contains("wget"), "must include wget in argv: {log}");
    }

    #[test]
    #[serial]
    fn apt_uninstall_invokes_apt_get_shim_with_remove_subcommand_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        let shim = ToolShim::install(APT_GET_BIN_ENV, 0, "", "");
        let printer = test_printer();
        apt_manager()
            .uninstall(&["nginx".into()], &printer)
            .unwrap();
        let log = shim.argv_log();
        assert_eq!(shim.invocation_count(), 1);
        assert!(log.contains("remove"), "must invoke remove subcmd: {log}");
        assert!(log.contains("-y"));
        assert!(log.contains("nginx"));
    }

    #[test]
    #[serial]
    fn apt_install_failure_propagates_command_failed_error_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        let _shim = ToolShim::install(APT_GET_BIN_ENV, 7, "", "E: Could not get lock\n");
        let printer = test_printer();
        let err = apt_manager()
            .install(&["curl".into()], &printer)
            .unwrap_err();
        // Failed exit must surface as PackageError::InstallFailed (via
        // run_pkg_cmd_live's tag arg "install"), not silent success.
        let msg = err.to_string();
        assert!(
            msg.contains("install") || msg.contains("apt") || msg.contains("7"),
            "install error should surface manager/op/exit-code: {msg}"
        );
    }

    #[test]
    #[serial]
    fn apt_update_invokes_apt_get_update_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        let shim = ToolShim::install(APT_GET_BIN_ENV, 0, "", "");
        let printer = test_printer();
        apt_manager().update(&printer).unwrap();
        let log = shim.argv_log();
        assert_eq!(shim.invocation_count(), 1);
        assert!(
            log.trim().ends_with("update"),
            "update should be the final argv token: {log}"
        );
    }

    #[test]
    #[serial]
    fn dnf_update_tolerates_exit_100_via_ignore_update_exit_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        // dnf check-update returns 100 when updates are available; the manager
        // is configured with `ignore_update_exit = true`, which routes the
        // call through `printer.run` (not run_pkg_cmd_live), swallowing the
        // non-zero exit instead of mapping to UpdateFailed.
        let shim = ToolShim::install(DNF_BIN_ENV, 100, "Updates available\n", "");
        let printer = test_printer();
        dnf_manager()
            .update(&printer)
            .expect("dnf update must tolerate exit 100");
        assert_eq!(shim.invocation_count(), 1);
    }

    #[test]
    #[serial]
    fn apk_install_invokes_apk_add_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        // apk_manager has no `sudo` prefix; its install_cmd is `apk add`.
        // The seam routes through APK_BIN_ENV so the shim is invoked
        // regardless of root status, but we still gate on is_root because
        // `is_root` matters for the strip_sudo path consistency.
        let shim = ToolShim::install(APK_BIN_ENV, 0, "", "");
        let printer = test_printer();
        apk_manager().install(&["curl".into()], &printer).unwrap();
        let log = shim.argv_log();
        assert!(log.contains("add"), "apk install uses 'add': {log}");
        assert!(log.contains("curl"), "package must appear: {log}");
    }

    #[test]
    #[serial]
    fn pacman_install_passes_noconfirm_through_shim_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        let shim = ToolShim::install(PACMAN_BIN_ENV, 0, "", "");
        let printer = test_printer();
        pacman_manager().install(&["vim".into()], &printer).unwrap();
        let log = shim.argv_log();
        assert!(
            log.contains("--noconfirm"),
            "pacman install must include --noconfirm: {log}"
        );
        assert!(log.contains("vim"));
    }

    #[test]
    #[serial]
    fn pkg_install_invokes_pkg_install_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        let shim = ToolShim::install(PKG_BIN_ENV, 0, "", "");
        let printer = test_printer();
        pkg_manager().install(&["bash".into()], &printer).unwrap();
        let log = shim.argv_log();
        assert!(log.contains("install"));
        assert!(log.contains("bash"));
        assert!(log.contains("-y"));
    }

    #[test]
    #[serial]
    fn zypper_uninstall_invokes_zypper_remove_when_root() {
        if !cfgd_core::is_root() {
            return;
        }
        let shim = ToolShim::install(ZYPPER_BIN_ENV, 0, "", "");
        let printer = test_printer();
        zypper_manager()
            .uninstall(&["vim".into()], &printer)
            .unwrap();
        let log = shim.argv_log();
        assert!(log.contains("remove"), "zypper uninstall: {log}");
        assert!(log.contains("vim"));
    }

    #[test]
    #[serial]
    fn apt_installed_packages_with_versions_uses_list_with_versions_override() {
        // apt_manager has `list_with_versions: Some(list_apt_with_versions)`;
        // verify the override branch fires and returns parsed pkg/version
        // pairs rather than the fallback "unknown" version.
        let _shim = ToolShim::install(
            DPKG_QUERY_BIN_ENV,
            0,
            "curl\t7.88.1-1\nwget\t1.21.3-1ubuntu1\n",
            "",
        );
        let pkgs = apt_manager().installed_packages_with_versions().unwrap();
        assert_eq!(pkgs.len(), 2);
        let curl = pkgs.iter().find(|p| p.name == "curl").unwrap();
        assert_eq!(curl.version, "7.88.1-1");
        let wget = pkgs.iter().find(|p| p.name == "wget").unwrap();
        assert_eq!(wget.version, "1.21.3-1ubuntu1");
    }

    #[test]
    #[serial]
    fn apk_installed_packages_with_versions_fallback_returns_unknown_versions() {
        // apk_manager has `list_with_versions: None`; the default trait path
        // wraps `installed_packages` with `version: "unknown"`.
        let _shim = ToolShim::install(APK_BIN_ENV, 0, "curl-7.88.1-r1\nwget-1.21.3-r2\n", "");
        let pkgs = apk_manager().installed_packages_with_versions().unwrap();
        assert!(!pkgs.is_empty(), "shim output should produce packages");
        for p in &pkgs {
            assert_eq!(
                p.version, "unknown",
                "fallback path must report 'unknown' versions, got: {p:?}"
            );
        }
    }

    #[test]
    fn apt_package_aliases_routes_through_aliases_fn_branch() {
        // apt_manager has `aliases_fn: Some(apt_aliases)`. The aliases fn
        // returns Debian package alternatives for canonical names like
        // `fd` → `["fd-find"]` (Debian renamed for namespace conflicts).
        let aliases = apt_manager().package_aliases("fd").unwrap();
        assert!(
            aliases.contains(&"fd-find".to_string()),
            "apt alias for fd should include fd-find, got: {aliases:?}"
        );
    }

    #[test]
    fn dnf_package_aliases_routes_through_aliases_fn_branch() {
        // dnf_aliases maps `fd` → `fd-find` (same upstream-vs-distro split as apt).
        let aliases = dnf_manager().package_aliases("fd").unwrap();
        assert_eq!(aliases, vec!["fd-find".to_string()]);
    }
}
