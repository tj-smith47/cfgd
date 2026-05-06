use sha2::Digest as _;

/// Compute SHA256 hash of data and return as lowercase hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(data))
}

/// Compute an OCI-style `sha256:<hex>` digest string from data.
pub fn sha256_digest(data: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(data))
}

/// Strip the `sha256:` prefix from a digest string, returning the hex body.
/// Returns the original string unchanged if no prefix is present.
pub fn strip_sha256_prefix(s: &str) -> &str {
    s.strip_prefix("sha256:").unwrap_or(s)
}

/// Parse a potentially loose version string into a semver Version.
/// Handles "1.28" → "1.28.0" and "1" → "1.0.0".
pub fn parse_loose_version(s: &str) -> Option<semver::Version> {
    if let Ok(ver) = semver::Version::parse(s) {
        return Some(ver);
    }
    if s.matches('.').count() == 1
        && let Ok(ver) = semver::Version::parse(&format!("{s}.0"))
    {
        return Some(ver);
    }
    if !s.contains('.')
        && let Ok(ver) = semver::Version::parse(&format!("{s}.0.0"))
    {
        return Some(ver);
    }
    None
}

/// Check whether `version_str` satisfies `requirement_str` (semver range).
pub fn version_satisfies(version_str: &str, requirement_str: &str) -> bool {
    let req = match semver::VersionReq::parse(requirement_str) {
        Ok(r) => r,
        Err(_) => return false,
    };
    parse_loose_version(version_str)
        .map(|ver| req.matches(&ver))
        .unwrap_or(false)
}
