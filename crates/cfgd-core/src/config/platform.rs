use std::collections::HashMap;

use super::source::ConfigSourceProvides;

// --- Platform Detection ---

/// Detected platform information for matching source `platform-profiles`.
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    pub os: String,
    pub distro: Option<String>,
    pub distro_version: Option<String>,
}

/// Detect the current platform OS, distro, and version.
pub fn detect_platform() -> PlatformInfo {
    let os = match std::env::consts::OS {
        "macos" => "macos".to_string(),
        other => other.to_string(),
    };
    let (distro, version) = if os == "linux" {
        parse_os_release_file()
    } else {
        (None, None)
    };
    PlatformInfo {
        os,
        distro,
        distro_version: version,
    }
}

/// Match platform info against a source's `platform-profiles` map.
/// Tries exact distro match first, then OS-level match.
pub fn match_platform_profile(
    platform: &PlatformInfo,
    platform_profiles: &HashMap<String, String>,
) -> Option<String> {
    // Try exact distro match first (e.g., "debian", "ubuntu", "fedora")
    if let Some(ref distro) = platform.distro
        && let Some(path) = platform_profiles.get(distro)
    {
        return Some(path.clone());
    }
    // Fall back to OS-level match (e.g., "macos", "linux")
    if let Some(path) = platform_profiles.get(&platform.os) {
        return Some(path.clone());
    }
    None
}

fn parse_os_release_file() -> (Option<String>, Option<String>) {
    let fields = crate::platform::parse_os_release_content(
        &std::fs::read_to_string("/etc/os-release").unwrap_or_default(),
    );
    (
        fields.get("ID").map(|v| v.to_lowercase()),
        fields.get("VERSION_ID").cloned(),
    )
}

/// Get the list of profile names from a ConfigSource manifest.
/// Prefers `profile_details` if populated, falls back to `profiles`.
pub fn source_profile_names(provides: &ConfigSourceProvides) -> Vec<String> {
    if !provides.profile_details.is_empty() {
        provides
            .profile_details
            .iter()
            .map(|p| p.name.clone())
            .collect()
    } else {
        provides.profiles.clone()
    }
}
