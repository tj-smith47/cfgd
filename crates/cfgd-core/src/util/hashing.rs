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

/// The display form of a commit id: enough to identify it, short enough for
/// two of them to sit on one line. Every human surface that names a commit
/// (`source show`, `sync`, the daemon's sync log) renders through it; a
/// persisted or `-o json` id stays full-length.
pub fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The type's whole contract: the seam between two `update` calls adds
    /// nothing to the digest, so a caller that replaced a buffered
    /// concatenation with the stream produces the same bytes it always did.
    /// Every existing `modules.lock` entry verifies against a digest taken
    /// the old way.
    #[test]
    fn a_seam_between_updates_is_not_part_of_the_digest() {
        let whole = b"module.yaml\0kind: Module\0files/init.lua\0vim.opt\0";
        for split in 0..=whole.len() {
            let mut stream = Sha256Stream::new();
            stream.update(&whole[..split]);
            stream.update(&whole[split..]);
            assert_eq!(
                stream.finish_hex(),
                sha256_hex(whole),
                "the seam at byte {split} changed the digest"
            );
        }
    }

    /// A file part hashes as its bytes and nothing else — no length prefix, no
    /// path, no separator of its own. The caller supplies every delimiter, so
    /// an in-memory part and a file part are interchangeable at the same
    /// position in the sequence.
    #[test]
    fn a_file_part_hashes_as_its_bytes_in_the_callers_order() {
        let tmp = tempfile::tempdir().unwrap();
        let body = b"kind: Module\nmetadata:\n  name: nvim\n";
        let path = tmp.path().join("module.yaml");
        std::fs::write(&path, body).unwrap();

        let mut stream = Sha256Stream::new();
        stream.update(b"module.yaml");
        stream.update(&[0]);
        stream.absorb_file(&path).unwrap();
        stream.update(&[0]);

        let mut expected = Vec::new();
        expected.extend_from_slice(b"module.yaml");
        expected.push(0);
        expected.extend_from_slice(body);
        expected.push(0);
        assert_eq!(stream.finish_hex(), sha256_hex(&expected));
    }

    /// The chunked read is a read, not a framing: a file larger than one chunk
    /// hashes as one uninterrupted sequence, and the parts around it keep
    /// their places.
    #[test]
    fn a_file_larger_than_one_chunk_hashes_as_one_sequence() {
        let tmp = tempfile::tempdir().unwrap();
        let body = vec![b'x'; SHA256_STREAM_CHUNK + 7];
        let path = tmp.path().join("big.bin");
        std::fs::write(&path, &body).unwrap();

        let mut stream = Sha256Stream::new();
        stream.update(b"head");
        stream.absorb_file(&path).unwrap();
        stream.update(b"tail");

        assert_eq!(
            stream.finish_hex(),
            "98227d8ab8add3942a886765921db6d32df6bf4c158897d9e250a0390a4674a0"
        );
    }

    /// `finish_digest` is `finish_hex` under the same `sha256:` prefix
    /// [`sha256_digest`] uses — the form `hash_module_contents` stores in
    /// `modules.lock`.
    #[test]
    fn the_digest_form_matches_the_one_shot_spelling() {
        let mut stream = Sha256Stream::new();
        stream.update(b"alpha");
        stream.update(b"beta");
        let digest = stream.finish_digest();
        assert_eq!(digest, sha256_digest(b"alphabeta"));
        assert_eq!(strip_sha256_prefix(&digest), sha256_hex(b"alphabeta"));
    }

    /// One literal digest over the exact seam shape `hash_module_contents`
    /// builds (`<rel-path>\0<contents>\0`, files in sorted order). The tests
    /// above are all self-consistent — they would stay green if the seam
    /// order or a delimiter changed on both sides at once. This one pins the
    /// VALUE, so a reframing has to be a deliberate edit here rather than a
    /// silent re-hash of every user's lockfile.
    #[test]
    fn the_lockfile_seam_shape_has_a_pinned_digest() {
        let mut stream = Sha256Stream::new();
        for (name, body) in [("a.txt", "alpha"), ("b.txt", "beta")] {
            stream.update(name.as_bytes());
            stream.update(&[0]);
            stream.update(body.as_bytes());
            stream.update(&[0]);
        }
        assert_eq!(
            stream.finish_digest(),
            "sha256:44e330d3f44895307cdc6c23a01e6c001f9db1a5ed49b5ad2553206d1adbf105"
        );
    }

    /// An absent file is reported, not silently absorbed as nothing — a tree
    /// walk that lost a file between listing and hashing must not produce a
    /// digest that looks like a successful one.
    #[test]
    fn absorbing_a_missing_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut stream = Sha256Stream::new();
        let err = stream.absorb_file(&tmp.path().join("nope")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
