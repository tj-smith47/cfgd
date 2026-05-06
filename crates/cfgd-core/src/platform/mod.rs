// Platform detection — OS, distro, architecture, native package manager mapping

use std::collections::HashMap;
use std::fs;

/// Detected operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOS,
    FreeBSD,
    Windows,
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
    Windows,
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

        if cfg!(windows) {
            return Platform {
                os: Os::Windows,
                distro: Distro::Windows,
                version: String::new(),
                arch,
            };
        }

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
            Distro::Windows => "winget",
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
            Os::Windows => "windows",
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
            Distro::Windows => "windows",
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
        .stderr(std::process::Stdio::piped())
        .output()?;
    if output.status.success() {
        Ok(crate::stdout_lossy_trimmed(&output))
    } else {
        let stderr = crate::stderr_lossy_trimmed(&output);
        Err(std::io::Error::other(format!(
            "{} failed: {}",
            cmd,
            if stderr.is_empty() {
                format!("exit code {}", output.status.code().unwrap_or(-1))
            } else {
                stderr
            }
        )))
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
pub(crate) fn parse_os_release_content(content: &str) -> HashMap<String, String> {
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
mod tests;
