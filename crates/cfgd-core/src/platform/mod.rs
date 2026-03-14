// Platform detection — OS, distro, architecture, native package manager mapping

use std::collections::HashMap;
use std::fs;

/// Detected operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOS,
    FreeBSD,
}

/// Detected Linux distribution (or MacOS/FreeBSD pseudo-distro).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    Ubuntu,
    Debian,
    Fedora,
    RHEL,
    CentOS,
    Arch,
    Manjaro,
    Alpine,
    OpenSUSE,
    FreeBSD,
    MacOS,
    Unknown,
}

/// CPU architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
    Other(String),
}

/// Detected platform: OS, distro, version, and architecture.
#[derive(Debug, Clone)]
pub struct Platform {
    pub os: Os,
    pub distro: Distro,
    pub version: String,
    pub arch: Arch,
}

impl Platform {
    /// Detect the current platform.
    ///
    /// - macOS: uses `cfg!(target_os)` and `sw_vers` for version.
    /// - Linux: parses `/etc/os-release`.
    /// - FreeBSD: uses `cfg!(target_os)` and `freebsd-version`.
    pub fn detect() -> Self {
        let arch = detect_arch();

        if cfg!(target_os = "macos") {
            let version = read_macos_version().unwrap_or_default();
            return Platform {
                os: Os::MacOS,
                distro: Distro::MacOS,
                version,
                arch,
            };
        }

        if cfg!(target_os = "freebsd") {
            let version = read_command_output("freebsd-version", &[]).unwrap_or_default();
            return Platform {
                os: Os::FreeBSD,
                distro: Distro::FreeBSD,
                version: version.trim().to_string(),
                arch,
            };
        }

        // Linux — parse /etc/os-release
        let (distro, version) = parse_os_release().unwrap_or((Distro::Unknown, String::new()));
        Platform {
            os: Os::Linux,
            distro,
            version,
            arch,
        }
    }

    /// Check whether this platform matches any tag in the given filter list.
    /// Tags are matched against OS, distro, and arch names.
    /// Returns `true` if any tag matches, or if the filter list is empty.
    pub fn matches_any(&self, tags: &[String]) -> bool {
        if tags.is_empty() {
            return true;
        }
        tags.iter().any(|tag| {
            tag == self.os.as_str() || tag == self.distro.as_str() || tag == self.arch.as_str()
        })
    }

    /// Return the canonical native package manager name for this platform.
    pub fn native_manager(&self) -> &str {
        match self.distro {
            Distro::MacOS => "brew",
            Distro::Ubuntu | Distro::Debian => "apt",
            Distro::Fedora => "dnf",
            Distro::RHEL | Distro::CentOS => {
                // RHEL 8+ and CentOS 8+ use dnf; 7 uses yum.
                // If version starts with "7", use yum.
                if self.version.starts_with('7') {
                    "yum"
                } else {
                    "dnf"
                }
            }
            Distro::Arch | Distro::Manjaro => "pacman",
            Distro::Alpine => "apk",
            Distro::OpenSUSE => "zypper",
            Distro::FreeBSD => "pkg",
            Distro::Unknown => "apt", // best-effort default for unknown Linux
        }
    }
}

impl Os {
    pub fn as_str(&self) -> &str {
        match self {
            Os::Linux => "linux",
            Os::MacOS => "macos",
            Os::FreeBSD => "freebsd",
        }
    }
}

impl Distro {
    pub fn as_str(&self) -> &str {
        match self {
            Distro::Ubuntu => "ubuntu",
            Distro::Debian => "debian",
            Distro::Fedora => "fedora",
            Distro::RHEL => "rhel",
            Distro::CentOS => "centos",
            Distro::Arch => "arch",
            Distro::Manjaro => "manjaro",
            Distro::Alpine => "alpine",
            Distro::OpenSUSE => "opensuse",
            Distro::FreeBSD => "freebsd",
            Distro::MacOS => "macos",
            Distro::Unknown => "unknown",
        }
    }
}

impl Arch {
    pub fn as_str(&self) -> &str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
            Arch::Other(s) => s,
        }
    }
}

impl std::fmt::Display for Os {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for Distro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn detect_arch() -> Arch {
    match std::env::consts::ARCH {
        "x86_64" => Arch::X86_64,
        "aarch64" => Arch::Aarch64,
        other => Arch::Other(other.to_string()),
    }
}

fn read_macos_version() -> Option<String> {
    read_command_output("sw_vers", &["-productVersion"])
        .map(|s| s.trim().to_string())
        .ok()
}

fn read_command_output(cmd: &str, args: &[&str]) -> Result<String, std::io::Error> {
    let output = std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(std::io::Error::other("command failed"))
    }
}

/// Parse `/etc/os-release` to detect Linux distro and version.
fn parse_os_release() -> Option<(Distro, String)> {
    let content = fs::read_to_string("/etc/os-release").ok()?;
    Some(distro_from_os_release_content(&content))
}

/// Given raw os-release file content, parse fields and resolve distro + version.
/// Used by both production `parse_os_release` and tests.
fn distro_from_os_release_content(content: &str) -> (Distro, String) {
    let fields = parse_os_release_content(content);

    let id = fields
        .get("ID")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let id_like = fields
        .get("ID_LIKE")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let version_id = fields.get("VERSION_ID").cloned().unwrap_or_default();

    let distro = match id.as_str() {
        "ubuntu" => Distro::Ubuntu,
        "debian" => Distro::Debian,
        "fedora" => Distro::Fedora,
        "rhel" | "redhat" => Distro::RHEL,
        "centos" => Distro::CentOS,
        "arch" | "archlinux" => Distro::Arch,
        "manjaro" => Distro::Manjaro,
        "alpine" => Distro::Alpine,
        "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" => Distro::OpenSUSE,
        _ => {
            // Check ID_LIKE for derivatives
            if id_like.contains("ubuntu") || id_like.contains("debian") {
                if id_like.contains("ubuntu") {
                    Distro::Ubuntu
                } else {
                    Distro::Debian
                }
            } else if id_like.contains("fedora") || id_like.contains("rhel") {
                Distro::Fedora
            } else if id_like.contains("arch") {
                Distro::Arch
            } else if id_like.contains("suse") {
                Distro::OpenSUSE
            } else {
                Distro::Unknown
            }
        }
    };

    (distro, version_id)
}

/// Parse os-release file content into key-value pairs.
/// Handles quoted and unquoted values.
fn parse_os_release_content(content: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim_matches('"').trim_matches('\'').to_string();
            fields.insert(key.to_string(), value);
        }
    }
    fields
}

#[cfg(test)]
mod tests {
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
    fn native_manager_macos() {
        let p = Platform {
            os: Os::MacOS,
            distro: Distro::MacOS,
            version: "14.0".into(),
            arch: Arch::Aarch64,
        };
        assert_eq!(p.native_manager(), "brew");
    }

    #[test]
    fn native_manager_ubuntu() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::Ubuntu,
            version: "22.04".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "apt");
    }

    #[test]
    fn native_manager_debian() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::Debian,
            version: "12".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "apt");
    }

    #[test]
    fn native_manager_fedora() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::Fedora,
            version: "39".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "dnf");
    }

    #[test]
    fn native_manager_rhel7_uses_yum() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::RHEL,
            version: "7.9".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "yum");
    }

    #[test]
    fn native_manager_rhel8_uses_dnf() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::RHEL,
            version: "8.9".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "dnf");
    }

    #[test]
    fn native_manager_arch() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::Arch,
            version: String::new(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "pacman");
    }

    #[test]
    fn native_manager_alpine() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::Alpine,
            version: "3.19".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "apk");
    }

    #[test]
    fn native_manager_opensuse() {
        let p = Platform {
            os: Os::Linux,
            distro: Distro::OpenSUSE,
            version: "15.5".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "zypper");
    }

    #[test]
    fn native_manager_freebsd() {
        let p = Platform {
            os: Os::FreeBSD,
            distro: Distro::FreeBSD,
            version: "14.0".into(),
            arch: Arch::X86_64,
        };
        assert_eq!(p.native_manager(), "pkg");
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
    fn arch_display() {
        assert_eq!(format!("{}", Arch::X86_64), "x86_64");
        assert_eq!(format!("{}", Arch::Aarch64), "aarch64");
        assert_eq!(format!("{}", Arch::Other("riscv64".into())), "riscv64");
    }

    #[test]
    fn os_display() {
        assert_eq!(format!("{}", Os::Linux), "linux");
        assert_eq!(format!("{}", Os::MacOS), "macos");
        assert_eq!(format!("{}", Os::FreeBSD), "freebsd");
    }

    #[test]
    fn distro_display() {
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

    #[test]
    fn os_as_str() {
        assert_eq!(Os::Linux.as_str(), "linux");
        assert_eq!(Os::MacOS.as_str(), "macos");
        assert_eq!(Os::FreeBSD.as_str(), "freebsd");
    }

    #[test]
    fn distro_as_str() {
        assert_eq!(Distro::Ubuntu.as_str(), "ubuntu");
        assert_eq!(Distro::RHEL.as_str(), "rhel");
        assert_eq!(Distro::OpenSUSE.as_str(), "opensuse");
    }

    #[test]
    fn arch_as_str() {
        assert_eq!(Arch::X86_64.as_str(), "x86_64");
        assert_eq!(Arch::Aarch64.as_str(), "aarch64");
        assert_eq!(Arch::Other("riscv64".into()).as_str(), "riscv64");
    }
}
