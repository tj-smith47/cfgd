//! Image lockfile load/save/upsert helpers for the closed-loop image-pinning
//! flow. The lockfile is a user-named repo artifact at an arbitrary path
//! (default `cfgd-images.lock` in CWD), NOT in the config dir — so these take a
//! full `&Path` to the file (unlike the sources lockfile, which takes a config
//! dir). `cfgd image pack --lock` writes it; `kubectl cfgd deploy` reads it.

use std::path::Path;

use anyhow::Context;
use cfgd_core::config::{ImageLockEntry, ImagesLockfile};

/// Default image lockfile name, written next to your manifests.
pub const DEFAULT_IMAGE_LOCKFILE: &str = "cfgd-images.lock";

/// Load an image lockfile. Returns an empty lockfile if the path does not exist.
pub fn load_images_lockfile(path: &Path) -> anyhow::Result<ImagesLockfile> {
    if !path.exists() {
        return Ok(ImagesLockfile::default());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading image lockfile '{}'", path.display()))?;
    let lockfile: ImagesLockfile = serde_yaml::from_str(&contents)
        .with_context(|| format!("parsing image lockfile '{}'", path.display()))?;
    Ok(lockfile)
}

/// Save an image lockfile atomically (temp + rename) via `cfgd_core::atomic_write_str`.
pub fn save_images_lockfile(path: &Path, lockfile: &ImagesLockfile) -> anyhow::Result<()> {
    let contents = serde_yaml::to_string(lockfile).context("serializing image lockfile")?;
    cfgd_core::atomic_write_str(path, &contents)
        .with_context(|| format!("writing image lockfile '{}'", path.display()))?;
    Ok(())
}

/// Upsert one entry (matched by `reference`) then save.
pub fn update_image_lock_entry(path: &Path, entry: ImageLockEntry) -> anyhow::Result<()> {
    let mut lockfile = load_images_lockfile(path)?;
    match lockfile
        .images
        .iter_mut()
        .find(|e| e.reference == entry.reference)
    {
        Some(existing) => *existing = entry,
        None => lockfile.images.push(entry),
    }
    save_images_lockfile(path, &lockfile)
}

/// Compute the pinned digest reference (`<registry>/<repository>@<digest>`) from
/// a tag reference + a resolved digest. Reuses `cfgd_core::oci::OciReference::parse`
/// so registry-port colons are never mistaken for tag separators.
pub fn pinned_reference(reference: &str, digest: &str) -> anyhow::Result<String> {
    let parsed = cfgd_core::oci::OciReference::parse(reference)
        .map_err(|e| anyhow::anyhow!("invalid image reference '{reference}': {e}"))?;
    Ok(format!(
        "{}/{}@{}",
        parsed.registry, parsed.repository, digest
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfgd-images.lock");
        let lf = load_images_lockfile(&path).expect("missing must load empty");
        assert!(
            lf.images.is_empty(),
            "missing lockfile must yield empty images: {lf:?}"
        );
    }

    #[test]
    fn upsert_inserts_then_upserts_without_duplicating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfgd-images.lock");

        let first = ImageLockEntry {
            reference: "registry.jarvispro.io/gome/server:abc".to_string(),
            digest: "sha256:1111".to_string(),
            pinned: "registry.jarvispro.io/gome/server@sha256:1111".to_string(),
            locked_at: "2026-01-01T00:00:00Z".to_string(),
        };
        update_image_lock_entry(&path, first).expect("first upsert");

        let after_first = load_images_lockfile(&path).expect("reload after first");
        assert_eq!(after_first.images.len(), 1, "first upsert inserts one");
        assert_eq!(after_first.images[0].digest, "sha256:1111");

        let second = ImageLockEntry {
            reference: "registry.jarvispro.io/gome/server:abc".to_string(),
            digest: "sha256:2222".to_string(),
            pinned: "registry.jarvispro.io/gome/server@sha256:2222".to_string(),
            locked_at: "2026-01-02T00:00:00Z".to_string(),
        };
        update_image_lock_entry(&path, second).expect("second upsert");

        let after_second = load_images_lockfile(&path).expect("reload after second");
        assert_eq!(
            after_second.images.len(),
            1,
            "re-upsert of same reference must NOT duplicate"
        );
        assert_eq!(
            after_second.images[0].digest, "sha256:2222",
            "re-upsert must replace digest"
        );
        assert_eq!(
            after_second.images[0].pinned, "registry.jarvispro.io/gome/server@sha256:2222",
            "re-upsert must replace pinned"
        );
    }

    #[test]
    fn pinned_reference_plain_registry() {
        let pinned = pinned_reference("registry.jarvispro.io/gome/server:abc", "sha256:deadbeef")
            .expect("parse");
        assert_eq!(
            pinned, "registry.jarvispro.io/gome/server@sha256:deadbeef",
            "plain registry pin"
        );
    }

    #[test]
    fn pinned_reference_preserves_registry_port() {
        let pinned =
            pinned_reference("localhost:5000/test/img:v1", "sha256:deadbeef").expect("parse");
        assert_eq!(
            pinned, "localhost:5000/test/img@sha256:deadbeef",
            "registry-port colon must be preserved (not treated as a tag separator)"
        );
    }
}
