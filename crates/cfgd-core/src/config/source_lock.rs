use serde::{Deserialize, Serialize};

/// The on-disk `sources.lock` file — records the resolved commit SHA for each
/// source so composition is bit-reproducible across machines.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcesLockfile {
    #[serde(default)]
    pub sources: Vec<SourceLockEntry>,
}

/// A single locked source entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLockEntry {
    /// Source name matching `metadata.name` in the source spec.
    pub name: String,
    /// Origin URL of the source repository.
    pub url: String,
    /// The `pinVersion` constraint from config (None for HEAD-tracking sources).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_version: Option<String>,
    /// Resolved tag name (None for HEAD-tracking sources and commit-SHA pins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ref: Option<String>,
    /// 40-char commit SHA at time of lock.
    pub resolved_commit: String,
    /// ISO 8601 timestamp of when this entry was written.
    pub locked_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_lock_entry_serde_camel_case() {
        let entry = SourceLockEntry {
            name: "test-source".to_string(),
            url: "https://github.com/org/repo.git".to_string(),
            pin_version: Some("~2".to_string()),
            resolved_ref: Some("v2.1.0".to_string()),
            resolved_commit: "a".repeat(40),
            locked_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let lf = SourcesLockfile {
            sources: vec![entry],
        };
        let yaml = serde_yaml::to_string(&lf).expect("serialize");
        assert!(
            yaml.contains("pinVersion"),
            "expected camelCase pinVersion, got: {yaml}"
        );
        assert!(
            yaml.contains("resolvedRef"),
            "expected camelCase resolvedRef, got: {yaml}"
        );
        assert!(
            yaml.contains("resolvedCommit"),
            "expected camelCase resolvedCommit, got: {yaml}"
        );
        assert!(
            yaml.contains("lockedAt"),
            "expected camelCase lockedAt, got: {yaml}"
        );
    }
}
