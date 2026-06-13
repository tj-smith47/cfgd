use serde::{Deserialize, Serialize};

/// On-disk image lockfile (`cfgd-images.lock` by default) — maps each packed
/// image tag reference to its resolved content digest, so deployments pin the
/// exact bytes that were packed rather than a mutable tag.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesLockfile {
    #[serde(default)]
    pub images: Vec<ImageLockEntry>,
}

/// A single locked image entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageLockEntry {
    /// The tag reference exactly as passed to `cfgd image pack`
    /// (e.g. `registry.jarvispro.io/gome/server:abc123`). The lookup key.
    pub reference: String,
    /// The resolved OCI content digest (`sha256:...`).
    pub digest: String,
    /// The pinned digest reference deploy substitutes
    /// (`registry.jarvispro.io/gome/server@sha256:...`).
    pub pinned: String,
    /// ISO 8601 timestamp of when this entry was written.
    pub locked_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_lock_entry_serde_camel_case_round_trip() {
        let entry = ImageLockEntry {
            reference: "registry.jarvispro.io/gome/server:abc123".to_string(),
            digest: "sha256:deadbeef".to_string(),
            pinned: "registry.jarvispro.io/gome/server@sha256:deadbeef".to_string(),
            locked_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let lf = ImagesLockfile {
            images: vec![entry.clone()],
        };
        let yaml = serde_yaml::to_string(&lf).expect("serialize");
        assert!(
            yaml.contains("lockedAt"),
            "expected camelCase lockedAt, got: {yaml}"
        );

        let parsed: ImagesLockfile = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(parsed.images.len(), 1, "round-trip must preserve one entry");
        let back = &parsed.images[0];
        assert_eq!(back.reference, entry.reference, "reference must round-trip");
        assert_eq!(back.digest, entry.digest, "digest must round-trip");
        assert_eq!(back.pinned, entry.pinned, "pinned must round-trip");
    }
}
