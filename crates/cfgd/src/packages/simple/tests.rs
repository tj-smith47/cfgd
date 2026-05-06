use cfgd_core::command_available;
use cfgd_core::output::Printer;
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
    let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
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
