use super::*;

#[test]
fn detect_returns_valid_platform() {
    let platform = Platform::detect();
    // We can't assert specific values since tests run on different platforms,
    // but we can verify the struct is populated
    assert!(!format!("{}", platform.os).is_empty());
    assert!(!format!("{}", platform.arch).is_empty());
}

#[test]
fn native_manager_mapping() {
    let cases: &[(Os, Distro, &str, &str)] = &[
        (Os::MacOS, Distro::MacOS, "14.0", "brew"),
        (Os::Linux, Distro::Ubuntu, "22.04", "apt"),
        (Os::Linux, Distro::Debian, "12", "apt"),
        (Os::Linux, Distro::Fedora, "39", "dnf"),
        (Os::Linux, Distro::RHEL, "7.9", "yum"),
        (Os::Linux, Distro::RHEL, "8.9", "dnf"),
        (Os::Linux, Distro::Arch, "", "pacman"),
        (Os::Linux, Distro::Alpine, "3.19", "apk"),
        (Os::Linux, Distro::OpenSUSE, "15.5", "zypper"),
        (Os::FreeBSD, Distro::FreeBSD, "14.0", "pkg"),
    ];
    for (os, distro, version, expected) in cases {
        let p = Platform {
            os: os.clone(),
            distro: distro.clone(),
            version: version.to_string(),
            arch: Arch::X86_64,
        };
        assert_eq!(
            p.native_manager(),
            *expected,
            "failed for {:?}/{:?}",
            os,
            distro
        );
    }
}

#[test]
fn parse_os_release_ubuntu() {
    let content = r#"
NAME="Ubuntu"
VERSION="22.04.3 LTS (Jammy Jellyfish)"
ID=ubuntu
ID_LIKE=debian
VERSION_ID="22.04"
"#;
    let (distro, version) = parse_os_release_content_to_distro(content);
    assert_eq!(distro, Distro::Ubuntu);
    assert_eq!(version, "22.04");
}

#[test]
fn parse_os_release_fedora() {
    let content = r#"
NAME="Fedora Linux"
ID=fedora
VERSION_ID=39
"#;
    let (distro, version) = parse_os_release_content_to_distro(content);
    assert_eq!(distro, Distro::Fedora);
    assert_eq!(version, "39");
}

#[test]
fn parse_os_release_arch() {
    let content = r#"
NAME="Arch Linux"
ID=arch
"#;
    let (distro, version) = parse_os_release_content_to_distro(content);
    assert_eq!(distro, Distro::Arch);
    assert_eq!(version, "");
}

#[test]
fn parse_os_release_alpine() {
    let content = r#"
NAME="Alpine Linux"
ID=alpine
VERSION_ID=3.19.0
"#;
    let (distro, version) = parse_os_release_content_to_distro(content);
    assert_eq!(distro, Distro::Alpine);
    assert_eq!(version, "3.19.0");
}

#[test]
fn parse_os_release_derivative() {
    let content = r#"
NAME="Linux Mint"
ID=linuxmint
ID_LIKE="ubuntu debian"
VERSION_ID="21.2"
"#;
    let (distro, version) = parse_os_release_content_to_distro(content);
    assert_eq!(distro, Distro::Ubuntu);
    assert_eq!(version, "21.2");
}

#[test]
fn parse_os_release_opensuse_leap() {
    let content = r#"
NAME="openSUSE Leap"
ID="opensuse-leap"
VERSION_ID="15.5"
"#;
    let (distro, version) = parse_os_release_content_to_distro(content);
    assert_eq!(distro, Distro::OpenSUSE);
    assert_eq!(version, "15.5");
}

#[test]
fn parse_os_release_centos() {
    let content = r#"
NAME="CentOS Linux"
ID="centos"
ID_LIKE="rhel fedora"
VERSION_ID="7"
"#;
    let (distro, version) = parse_os_release_content_to_distro(content);
    assert_eq!(distro, Distro::CentOS);
    assert_eq!(version, "7");
}

#[test]
fn enum_display_formatting() {
    // Arch
    assert_eq!(format!("{}", Arch::X86_64), "x86_64");
    assert_eq!(format!("{}", Arch::Aarch64), "aarch64");
    assert_eq!(format!("{}", Arch::Other("riscv64".into())), "riscv64");
    // Os
    assert_eq!(format!("{}", Os::Linux), "linux");
    assert_eq!(format!("{}", Os::MacOS), "macos");
    assert_eq!(format!("{}", Os::FreeBSD), "freebsd");
    // Distro
    assert_eq!(format!("{}", Distro::Ubuntu), "ubuntu");
    assert_eq!(format!("{}", Distro::RHEL), "rhel");
    assert_eq!(format!("{}", Distro::OpenSUSE), "opensuse");
}

/// Helper: parse os-release content string into (Distro, version).
/// Delegates to the shared `distro_from_os_release_content`.
fn parse_os_release_content_to_distro(content: &str) -> (Distro, String) {
    distro_from_os_release_content(content)
}

#[test]
fn platform_matches_any_os() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::Ubuntu,
        version: "22.04".into(),
        arch: Arch::X86_64,
    };
    assert!(p.matches_any(&["linux".into()]));
    assert!(!p.matches_any(&["macos".into()]));
}

#[test]
fn platform_matches_any_distro() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::Ubuntu,
        version: "22.04".into(),
        arch: Arch::X86_64,
    };
    assert!(p.matches_any(&["ubuntu".into()]));
    assert!(!p.matches_any(&["fedora".into()]));
}

#[test]
fn platform_matches_any_arch() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::Ubuntu,
        version: "22.04".into(),
        arch: Arch::Aarch64,
    };
    assert!(p.matches_any(&["aarch64".into()]));
    assert!(!p.matches_any(&["x86_64".into()]));
}

#[test]
fn macos_is_the_canonical_token_for_module_platform_gating() {
    // The module-level `platforms` filter resolves through `matches_any`; the
    // canonical macOS token is `macos` (NOT `darwin`). A macOS platform matches
    // it, a Linux platform does not, and `darwin` matches nothing.
    let mac = Platform {
        os: Os::MacOS,
        distro: Distro::MacOS,
        version: "14.0".into(),
        arch: Arch::Aarch64,
    };
    let linux = Platform {
        os: Os::Linux,
        distro: Distro::Ubuntu,
        version: "22.04".into(),
        arch: Arch::X86_64,
    };
    assert!(mac.matches_any(&["macos".into()]));
    assert!(!linux.matches_any(&["macos".into()]));
    assert!(!mac.matches_any(&["darwin".into()]));
}

#[test]
fn platform_matches_any_empty_matches_all() {
    let p = Platform {
        os: Os::MacOS,
        distro: Distro::MacOS,
        version: "14.0".into(),
        arch: Arch::Aarch64,
    };
    assert!(p.matches_any(&[]));
}

#[test]
fn platform_matches_any_multiple_tags() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::Fedora,
        version: "39".into(),
        arch: Arch::X86_64,
    };
    // Any match is sufficient
    assert!(p.matches_any(&["macos".into(), "linux".into()]));
    assert!(p.matches_any(&["ubuntu".into(), "fedora".into()]));
    assert!(!p.matches_any(&["macos".into(), "freebsd".into()]));
}

// --- native_manager: additional distro mappings ---

#[test]
fn native_manager_centos_7_uses_yum() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::CentOS,
        version: "7.9".into(),
        arch: Arch::X86_64,
    };
    assert_eq!(p.native_manager(), "yum");
}

#[test]
fn native_manager_centos_8_uses_dnf() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::CentOS,
        version: "8.5".into(),
        arch: Arch::X86_64,
    };
    assert_eq!(p.native_manager(), "dnf");
}

#[test]
fn native_manager_manjaro() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::Manjaro,
        version: "23.0".into(),
        arch: Arch::X86_64,
    };
    assert_eq!(p.native_manager(), "pacman");
}

#[test]
fn native_manager_windows() {
    let p = Platform {
        os: Os::Windows,
        distro: Distro::Windows,
        version: String::new(),
        arch: Arch::X86_64,
    };
    assert_eq!(p.native_manager(), "winget");
}

#[test]
fn native_manager_unknown_defaults_to_apt() {
    let p = Platform {
        os: Os::Linux,
        distro: Distro::Unknown,
        version: String::new(),
        arch: Arch::X86_64,
    };
    assert_eq!(p.native_manager(), "apt");
}

// --- distro_from_os_release_content: ID_LIKE derivative detection ---

#[test]
fn parse_os_release_debian_only_derivative() {
    // A distro with ID_LIKE=debian (not ubuntu)
    let content = r#"
NAME="Raspberry Pi OS"
ID=raspbian
ID_LIKE=debian
VERSION_ID="11"
"#;
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::Debian);
    assert_eq!(version, "11");
}

#[test]
fn parse_os_release_fedora_derivative() {
    // A distro with ID_LIKE containing fedora
    let content = r#"
NAME="Nobara"
ID=nobara
ID_LIKE="fedora"
VERSION_ID="38"
"#;
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::Fedora);
    assert_eq!(version, "38");
}

#[test]
fn parse_os_release_rhel_derivative() {
    // A distro with ID_LIKE containing rhel
    let content = r#"
NAME="Rocky Linux"
ID=rocky
ID_LIKE="rhel centos fedora"
VERSION_ID="9.2"
"#;
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::Fedora);
    assert_eq!(version, "9.2");
}

#[test]
fn parse_os_release_arch_derivative() {
    // A distro with ID_LIKE containing arch
    let content = r#"
NAME="EndeavourOS"
ID=endeavouros
ID_LIKE=arch
VERSION_ID="2023.11.17"
"#;
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::Arch);
    assert_eq!(version, "2023.11.17");
}

#[test]
fn parse_os_release_suse_derivative() {
    // A distro with ID_LIKE containing suse
    let content = r#"
NAME="GeckoLinux"
ID=geckolinux
ID_LIKE="suse opensuse"
VERSION_ID="999"
"#;
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::OpenSUSE);
    assert_eq!(version, "999");
}

#[test]
fn parse_os_release_unknown_distro() {
    let content = r#"
NAME="Exotic Linux"
ID=exotic
VERSION_ID="1.0"
"#;
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::Unknown);
    assert_eq!(version, "1.0");
}

#[test]
fn parse_os_release_rhel_id() {
    let content = "ID=rhel\nVERSION_ID=9.2\n";
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::RHEL);
    assert_eq!(version, "9.2");
}

#[test]
fn parse_os_release_redhat_id() {
    let content = "ID=redhat\nVERSION_ID=8\n";
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::RHEL);
    assert_eq!(version, "8");
}

#[test]
fn parse_os_release_archlinux_id() {
    let content = "ID=archlinux\n";
    let (distro, _) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::Arch);
}

#[test]
fn parse_os_release_opensuse_tumbleweed() {
    let content = "ID=opensuse-tumbleweed\nVERSION_ID=20231201\n";
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::OpenSUSE);
    assert_eq!(version, "20231201");
}

#[test]
fn parse_os_release_manjaro() {
    let content = "ID=manjaro\nVERSION_ID=23.0\n";
    let (distro, version) = distro_from_os_release_content(content);
    assert_eq!(distro, Distro::Manjaro);
    assert_eq!(version, "23.0");
}

// --- parse_os_release_content: edge cases ---

#[test]
fn parse_os_release_content_handles_comments_and_blanks() {
    let content = "# This is a comment\n\nID=ubuntu\n\n# Another comment\nVERSION_ID=\"22.04\"\n";
    let fields = parse_os_release_content(content);
    assert_eq!(fields.get("ID").unwrap(), "ubuntu");
    assert_eq!(fields.get("VERSION_ID").unwrap(), "22.04");
}

#[test]
fn parse_os_release_content_handles_single_quotes() {
    let content = "ID='fedora'\nVERSION_ID='39'\n";
    let fields = parse_os_release_content(content);
    assert_eq!(fields.get("ID").unwrap(), "fedora");
    assert_eq!(fields.get("VERSION_ID").unwrap(), "39");
}

#[test]
fn parse_os_release_content_no_equals() {
    let content = "NOEQUALS\nID=test\n";
    let fields = parse_os_release_content(content);
    assert!(!fields.contains_key("NOEQUALS"));
    assert_eq!(fields.get("ID").unwrap(), "test");
}

// --- Display and as_str coverage ---

#[test]
fn os_windows_display() {
    assert_eq!(format!("{}", Os::Windows), "windows");
}

#[test]
fn distro_display_all_variants() {
    let cases: &[(Distro, &str)] = &[
        (Distro::CentOS, "centos"),
        (Distro::Manjaro, "manjaro"),
        (Distro::FreeBSD, "freebsd"),
        (Distro::MacOS, "macos"),
        (Distro::Windows, "windows"),
        (Distro::Unknown, "unknown"),
    ];
    for (distro, expected) in cases {
        assert_eq!(distro.as_str(), *expected);
        assert_eq!(format!("{}", distro), *expected);
    }
}

#[cfg(unix)]
#[test]
fn read_command_output_returns_trimmed_stdout_on_success() {
    let s = super::read_command_output("printf", &["hello world"]).expect("Ok");
    assert_eq!(s, "hello world");
}

#[cfg(unix)]
#[test]
fn read_command_output_errors_on_nonzero_with_stderr_payload() {
    // `sh -c 'echo boom >&2; exit 3'` — nonzero exit + stderr text. Error
    // must include the stderr in the message rather than the exit code.
    let err = super::read_command_output("sh", &["-c", "echo boom >&2; exit 3"])
        .expect_err("nonzero must error");
    let msg = err.to_string();
    assert!(
        msg.contains("boom") && msg.contains("sh"),
        "error must include stderr + command name: {msg}"
    );
}

#[cfg(unix)]
#[test]
fn read_command_output_errors_on_nonzero_without_stderr_falls_back_to_exit_code() {
    // `sh -c 'exit 7'` — nonzero exit, empty stderr. Error must mention
    // exit code in lieu of stderr.
    let err = super::read_command_output("sh", &["-c", "exit 7"]).expect_err("nonzero must error");
    let msg = err.to_string();
    assert!(
        msg.contains("exit code") && msg.contains("7"),
        "empty-stderr error must surface exit code: {msg}"
    );
}

#[test]
fn read_command_output_errors_on_missing_binary() {
    let err = super::read_command_output("definitely-not-a-real-command-xyzzy", &[])
        .expect_err("missing binary must error");
    let _ = err.to_string();
}
