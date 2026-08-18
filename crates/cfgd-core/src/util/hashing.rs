use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;

use sha2::Digest as _;

/// Render a finished SHA-256 digest as lowercase hex.
///
/// sha2 0.11's digest output is a `hybrid_array::Array` (derefs to `[u8]`) that
/// no longer implements `LowerHex`; encoding the bytes here keeps the
/// lowercase-hex output byte-identical to the `{:x}` formatting used before the
/// bump, for both the one-shot and the streaming spelling.
fn hex_of(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Compute SHA256 hash of data and return as lowercase hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    hex_of(&sha2::Sha256::digest(data))
}

/// Compute an OCI-style `sha256:<hex>` digest string from data.
pub fn sha256_digest(data: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(data))
}

/// How much of a file is held in memory at once while it is being hashed.
const SHA256_STREAM_CHUNK: usize = 64 * 1024;

/// SHA-256 over a sequence of pieces, none of which has to be held in memory.
///
/// The one streaming counterpart to [`sha256_hex`] / [`sha256_digest`], for a
/// digest taken over a whole directory tree: buffering every file's bytes and
/// then concatenating them into a second buffer costs twice the tree's size in
/// resident memory to produce a 32-byte answer, and the tree is user content
/// with no size bound (a module may ship a font, a binary, a tarball).
///
/// Feeding `update(a); update(b)` is defined to equal hashing `a` followed by
/// `b` as one slice, so a caller replacing a buffered concatenation with this
/// type produces the SAME digest — which is what lets an existing
/// `modules.lock` integrity hash keep verifying.
pub struct Sha256Stream {
    hasher: sha2::Sha256,
}

impl Default for Sha256Stream {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256Stream {
    pub fn new() -> Self {
        Self {
            hasher: sha2::Sha256::new(),
        }
    }

    /// Append `bytes` to the hashed sequence.
    pub fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    /// Append the whole contents of `path`, read in fixed-size chunks.
    pub fn absorb_file(&mut self, path: &Path) -> std::io::Result<()> {
        let mut file = std::fs::File::open(path)?;
        let mut buf = vec![0u8; SHA256_STREAM_CHUNK];
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                return Ok(());
            }
            self.hasher.update(&buf[..read]);
        }
    }

    /// Finish and render as lowercase hex.
    pub fn finish_hex(self) -> String {
        hex_of(&self.hasher.finalize())
    }

    /// Finish and render as an OCI-style `sha256:<hex>` digest string.
    pub fn finish_digest(self) -> String {
        format!("sha256:{}", self.finish_hex())
    }
}

/// Strip the `sha256:` prefix from a digest string, returning the hex body.
/// Returns the original string unchanged if no prefix is present.
pub fn strip_sha256_prefix(s: &str) -> &str {
    s.strip_prefix("sha256:").unwrap_or(s)
}

/// Parse a potentially loose version string into a semver Version.
/// Handles "1.28" → "1.28.0", "1" → "1.0.0", and a leading `v`/`V` prefix
/// (`v1.10.0` → `1.10.0`) so callers can feed git/OCI tag names directly.
pub fn parse_loose_version(s: &str) -> Option<semver::Version> {
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
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
