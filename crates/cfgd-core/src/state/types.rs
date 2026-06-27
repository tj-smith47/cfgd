//! State-store record types.
//!
//! Types that derive `Serialize + #[serde(rename_all = "camelCase")]` are part
//! of the cfgd CLI JSON output surface (`cfgd <cmd> -o json` paths). Types
//! that don't derive `Serialize` are internal-only DAOs — they're returned
//! from `StateStore` methods to crate-internal callers but never marshaled
//! across the CLI boundary. To surface a previously-internal type, add the
//! pair (`#[derive(Serialize)] #[serde(rename_all = "camelCase")]`) and wire
//! it into the relevant `*_output_types.rs` wrapper.

use serde::Serialize;

/// Apply status for a reconciliation run.
///
/// `rename_all = "camelCase"` makes the derived `Serialize` token match
/// [`Self::display_str`] for every variant (e.g. `inProgress`), so the CLI JSON
/// surface, the human display, and the `cfgd log` column never drift. The
/// snake_case state-store persistence form is the separate [`Self::as_str`] /
/// [`Self::from_str`] pair and is unaffected by this attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplyStatus {
    /// Apply completed with all actions successful.
    Success,
    /// Apply completed but some actions failed.
    Partial,
    /// Apply failed entirely.
    Failed,
    /// Apply is currently in progress (not yet finished).
    InProgress,
    /// Apply was cooperatively stopped by a signal (SIGINT/SIGTERM) after
    /// finishing the in-flight atomic action — no partial writes.
    Aborted,
}

impl ApplyStatus {
    pub(in crate::state) fn as_str(&self) -> &str {
        match self {
            ApplyStatus::Success => "success",
            ApplyStatus::Partial => "partial",
            ApplyStatus::Failed => "failed",
            ApplyStatus::InProgress => "in_progress",
            ApplyStatus::Aborted => "aborted",
        }
    }

    /// camelCase token for the CLI JSON surface (`cfgd apply`/`status -o json`).
    /// Distinct from [`Self::as_str`], which is the snake_case state-store
    /// persistence form that round-trips through [`Self::from_str`].
    pub fn display_str(&self) -> &'static str {
        match self {
            ApplyStatus::Success => "success",
            ApplyStatus::Partial => "partial",
            ApplyStatus::Failed => "failed",
            ApplyStatus::InProgress => "inProgress",
            ApplyStatus::Aborted => "aborted",
        }
    }

    pub(in crate::state) fn from_str(s: &str) -> Self {
        match s {
            "success" => ApplyStatus::Success,
            "partial" => ApplyStatus::Partial,
            "in_progress" => ApplyStatus::InProgress,
            "aborted" => ApplyStatus::Aborted,
            "failed" => ApplyStatus::Failed,
            _ => ApplyStatus::Failed,
        }
    }
}

#[cfg(test)]
mod apply_status_tests {
    use super::ApplyStatus;

    #[test]
    fn from_str_round_trips_known_and_defaults_unknown_to_failed() {
        assert_eq!(ApplyStatus::from_str("success"), ApplyStatus::Success);
        assert_eq!(ApplyStatus::from_str("partial"), ApplyStatus::Partial);
        assert_eq!(
            ApplyStatus::from_str("in_progress"),
            ApplyStatus::InProgress
        );
        assert_eq!(ApplyStatus::from_str("aborted"), ApplyStatus::Aborted);
        assert_eq!(ApplyStatus::from_str("failed"), ApplyStatus::Failed);
        // An unrecognized status conservatively maps to Failed.
        assert_eq!(ApplyStatus::from_str("bogus-status"), ApplyStatus::Failed);
    }

    /// Drift gate: the derived `Serialize` token (every `ApplyStatus` field
    /// embedded in a `-o json` struct) must equal `display_str` (the text/table
    /// surfaces) for EVERY variant. A fourth surface that re-derives a token, or
    /// a serde/display divergence, fails here. The exhaustive `match` is a
    /// tripwire: a newly added variant breaks compilation until it is listed.
    #[test]
    fn serialize_token_equals_display_str_for_every_variant() {
        let all = [
            ApplyStatus::Success,
            ApplyStatus::Partial,
            ApplyStatus::Failed,
            ApplyStatus::InProgress,
            ApplyStatus::Aborted,
        ];
        for v in &all {
            match v {
                ApplyStatus::Success
                | ApplyStatus::Partial
                | ApplyStatus::Failed
                | ApplyStatus::InProgress
                | ApplyStatus::Aborted => {}
            }
            let serde_token = serde_json::to_value(v).expect("serialize ApplyStatus");
            assert_eq!(
                serde_token,
                serde_json::Value::String(v.display_str().to_string()),
                "serde token and display_str drifted for {v:?}"
            );
        }
        // Concrete anchor for the only multi-word variant (where drift hid).
        assert_eq!(ApplyStatus::InProgress.display_str(), "inProgress");
        assert_eq!(
            serde_json::to_value(ApplyStatus::InProgress).expect("serialize"),
            serde_json::Value::String("inProgress".to_string())
        );
    }
}

/// A recorded apply operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRecord {
    pub id: i64,
    pub timestamp: String,
    pub profile: String,
    pub plan_hash: String,
    pub status: ApplyStatus,
    pub summary: Option<String>,
}

/// A recorded drift event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftEvent {
    pub id: i64,
    pub timestamp: String,
    pub resource_type: String,
    pub resource_id: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub resolved_by: Option<i64>,
    pub source: String,
}

/// A managed resource tracked in the state store.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResource {
    pub resource_type: String,
    pub resource_id: String,
    pub source: String,
    pub last_hash: Option<String>,
    pub last_applied: Option<i64>,
}

/// A tracked config source.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSourceRecord {
    pub id: i64,
    pub name: String,
    pub origin_url: String,
    pub origin_branch: String,
    pub last_fetched: Option<String>,
    pub last_commit: Option<String>,
    pub source_version: Option<String>,
    pub pinned_version: Option<String>,
    pub status: String,
}

/// A conflict record from composition. Internal-only DAO.
#[derive(Debug, Clone)]
pub struct SourceConflictRecord {
    pub id: i64,
    pub timestamp: String,
    pub source_name: String,
    pub resource_type: String,
    pub resource_id: String,
    pub resolution: String,
    pub detail: Option<String>,
}

/// A pending decision for a source item needing user review.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDecision {
    pub id: i64,
    pub source: String,
    pub resource: String,
    pub tier: String,
    pub action: String,
    pub summary: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub resolution: Option<String>,
}

/// A stored config hash for detecting source changes. Internal-only DAO.
#[derive(Debug, Clone)]
pub struct SourceConfigHash {
    pub source: String,
    pub config_hash: String,
    pub merged_at: String,
}

/// A module's state in the state store.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStateRecord {
    pub module_name: String,
    pub installed_at: String,
    pub last_applied: Option<i64>,
    pub packages_hash: String,
    pub files_hash: String,
    pub git_sources: Option<String>,
    pub status: String,
}

/// A file backup record from the safety store. Internal-only DAO; the
/// `content` blob would balloon JSON output, so deliberately non-`Serialize` —
/// surface via a derived view-struct if you need to expose this through
/// the CLI.
#[derive(Debug, Clone)]
pub struct FileBackupRecord {
    pub id: i64,
    pub apply_id: i64,
    pub file_path: String,
    pub content_hash: String,
    pub content: Vec<u8>,
    pub permissions: Option<u32>,
    pub was_symlink: bool,
    pub symlink_target: Option<String>,
    pub oversized: bool,
    pub backed_up_at: String,
    /// Whether the file existed at backup time. `false` marks a pre-action
    /// backup of a CREATE action; rollback removes such files rather than
    /// restoring their (empty) content.
    pub existed: bool,
}

/// A journal entry for a single action within an apply. Internal-only DAO;
/// used by rollback and apply-recovery paths.
#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub id: i64,
    pub apply_id: i64,
    pub action_index: i64,
    pub phase: String,
    pub action_type: String,
    pub resource_id: String,
    pub pre_state: Option<String>,
    pub post_state: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub script_output: Option<String>,
}

/// A compliance snapshot summary row from the state store.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceHistoryRow {
    pub id: i64,
    pub timestamp: String,
    pub compliant: i64,
    pub warning: i64,
    pub violation: i64,
}

/// A module file manifest entry — tracks which files a module deployed.
/// Internal-only DAO.
#[derive(Debug, Clone)]
pub struct ModuleFileRecord {
    pub module_name: String,
    pub file_path: String,
    pub content_hash: String,
    pub strategy: String,
    pub last_applied: Option<i64>,
}
