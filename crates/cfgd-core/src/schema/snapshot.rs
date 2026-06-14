//! Versioned schema snapshots.
//!
//! A [`SchemaSnapshot`] pairs a kind's JSON schema with the cfgd version that
//! produced it. The skill installer embeds a snapshot as an offline fallback so
//! the agent-facing skill body can describe a kind's fields even when the live
//! binary is unavailable; pinning the version lets a consumer tell whether a
//! snapshot is stale relative to the running cfgd.

use serde::{Deserialize, Serialize};

use super::KindEntry;

/// A kind's JSON schema captured against a specific cfgd version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSnapshot {
    /// The cfgd version (`CARGO_PKG_VERSION`) that produced `json_schema`.
    pub cfgd_version: String,
    /// The kind's JSON schema, serialized as a string.
    pub json_schema: String,
}

/// Capture a [`SchemaSnapshot`] for one registry entry, stamped with the
/// current cfgd version.
pub fn snapshot_for(entry: &KindEntry) -> SchemaSnapshot {
    SchemaSnapshot {
        cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
        json_schema: entry.json_schema(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::KIND_REGISTRY;

    #[test]
    fn snapshot_stamps_current_version_and_schema() {
        let entry = KIND_REGISTRY
            .iter()
            .find(|e| e.kind == "Module" && !e.crd)
            .unwrap();
        let snap = snapshot_for(entry);
        assert_eq!(snap.cfgd_version, env!("CARGO_PKG_VERSION"));
        assert!(
            snap.json_schema.contains("packages"),
            "snapshot should carry the live schema"
        );
    }
}
