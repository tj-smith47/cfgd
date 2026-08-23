//! State-store record types.
//!
//! Types that derive `Serialize + #[serde(rename_all = "camelCase")]` are part
//! of the cfgd CLI JSON output surface (`cfgd <cmd> -o json` paths). Types
//! that don't derive `Serialize` are internal-only DAOs — they're returned
//! from `StateStore` methods to crate-internal callers but never marshaled
//! across the CLI boundary. To surface a previously-internal type, add the
//! pair (`#[derive(Serialize)] #[serde(rename_all = "camelCase")]`) and wire
//! it into the relevant `*_output_types.rs` wrapper.

use crate::output::Role;
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
    /// persistence form that round-trips through [`Self::from_str`], and from
    /// [`Self::human_str`], which is what a person reads.
    pub fn display_str(&self) -> &'static str {
        match self {
            ApplyStatus::Success => "success",
            ApplyStatus::Partial => "partial",
            ApplyStatus::Failed => "failed",
            ApplyStatus::InProgress => "inProgress",
            ApplyStatus::Aborted => "aborted",
        }
    }

    /// TitleCase spelling for every HUMAN surface — `cfgd status`'s
    /// `Last Apply → Result` row and `cfgd log`'s Status column — matching the
    /// display vocabulary the manifest enums already read in (`Symlink`,
    /// `NotApplied`).
    ///
    /// Split from [`Self::display_str`] because that token is a WIRE value: it
    /// is what `-o json` carries and what an external matcher greps for, so a
    /// reword of the words on screen must not be able to reach it.
    pub fn human_str(&self) -> &'static str {
        match self {
            ApplyStatus::Success => "Success",
            ApplyStatus::Partial => "Partial",
            ApplyStatus::Failed => "Failed",
            ApplyStatus::InProgress => "InProgress",
            ApplyStatus::Aborted => "Aborted",
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

    /// Wire contract, pinned byte-for-byte: the snake_case token stored in
    /// `applies.status` and the camelCase token every `-o json` payload
    /// carries. A reword of the words on SCREEN belongs in `human_str`; if it
    /// reaches either column here it breaks `from_str` against every state DB
    /// an older cfgd wrote, and changes what an external matcher sees. Break
    /// this test only on purpose.
    #[test]
    fn apply_status_literals_are_a_pinned_wire_contract() {
        let pins = [
            (ApplyStatus::Success, "success", "success", "Success"),
            (ApplyStatus::Partial, "partial", "partial", "Partial"),
            (ApplyStatus::Failed, "failed", "failed", "Failed"),
            (
                ApplyStatus::InProgress,
                "in_progress",
                "inProgress",
                "InProgress",
            ),
            (ApplyStatus::Aborted, "aborted", "aborted", "Aborted"),
        ];
        for (variant, stored, wire, human) in pins {
            assert_eq!(variant.as_str(), stored, "stored token drifted");
            assert_eq!(variant.display_str(), wire, "json token drifted");
            assert_eq!(variant.human_str(), human, "human spelling drifted");
            assert_eq!(
                ApplyStatus::from_str(stored),
                variant,
                "stored token no longer round-trips"
            );
        }
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

/// The `module_state.status` token for a module whose last apply completed
/// every one of its actions.
pub const MODULE_STATUS_INSTALLED: &str = "installed";
/// The `module_state.status` token for a module at least one of whose actions
/// failed on its last apply.
pub const MODULE_STATUS_ERROR: &str = "error";

/// The human vocabulary for a module's state — the ONE derivation of the word
/// a person reads from the token the state store holds, so `cfgd status`,
/// `cfgd status --module` and `cfgd module list` can never call one machine
/// state by three names.
///
/// The stored tokens are untouched WIRE values ([`MODULE_STATUS_INSTALLED`] /
/// [`MODULE_STATUS_ERROR`], pinned by
/// `module_status_literals_are_a_pinned_wire_contract`), and so are the
/// no-record spellings a `-o json` payload carries in their place
/// (`not applied`, `pending`, `available`) — a consumer matching on any of
/// them sees exactly what it saw before.
///
/// `drifted` is the caller's LIVE scan verdict, read from the same results
/// that fill the report's Drift section, so `Drifted` can never contradict the
/// rows beneath it. It is deliberately not storable: a recorded "drifted"
/// would be a claim about a machine nobody has looked at since.
///
/// Everything the store does not recognise reads `NotApplied` — the honest
/// answer about a module no apply has recorded. `cfgd module list`'s
/// `pending` / `available` split lands there too; the row's `Active` column
/// already says which of the two it is.
pub fn module_status_display(stored: &str, drifted: bool) -> (&'static str, Role) {
    match stored {
        MODULE_STATUS_ERROR => ("Failed", Role::Fail),
        MODULE_STATUS_INSTALLED if drifted => ("Drifted", Role::Warn),
        MODULE_STATUS_INSTALLED => ("Synced", Role::Ok),
        _ => ("NotApplied", Role::Pending),
    }
}

#[cfg(test)]
mod module_status_tests {
    use super::*;

    /// Wire contract, pinned byte-for-byte: the two tokens
    /// `reconciler::apply` writes into `module_state.status` and every
    /// `-o json` `status` field reports. The words on screen live in
    /// [`module_status_display`]; a reword there must never reach these.
    #[test]
    fn module_status_literals_are_a_pinned_wire_contract() {
        assert_eq!(MODULE_STATUS_INSTALLED, "installed");
        assert_eq!(MODULE_STATUS_ERROR, "error");
    }

    #[test]
    fn a_stored_token_maps_to_one_display_word_and_one_role() {
        assert_eq!(
            module_status_display(MODULE_STATUS_INSTALLED, false),
            ("Synced", Role::Ok)
        );
        assert_eq!(
            module_status_display(MODULE_STATUS_INSTALLED, true),
            ("Drifted", Role::Warn)
        );
        assert_eq!(
            module_status_display(MODULE_STATUS_ERROR, false),
            ("Failed", Role::Fail)
        );
        // A failed apply outranks a drift finding: the module is broken, not
        // merely out of date.
        assert_eq!(
            module_status_display(MODULE_STATUS_ERROR, true),
            ("Failed", Role::Fail)
        );
    }

    #[test]
    fn every_no_record_spelling_reads_not_applied() {
        for stored in ["not applied", "not yet applied", "pending", "available", ""] {
            assert_eq!(
                module_status_display(stored, false),
                ("NotApplied", Role::Pending),
                "{stored:?} should read NotApplied"
            );
        }
    }
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

/// Outcome of one declarative backup run (`spec.backups[]`).
///
/// Only two outcomes exist because the artifact is what the operator cares
/// about: either a snapshot was written (`Success`) or none was
/// (`Failed`). A `postBackup` hook that fails *after* a good copy leaves the
/// run `Success` with [`BackupRunRecord::error`] populated — see
/// [`crate::backup::run_backup`] for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupRunStatus {
    /// A snapshot was written to the destination.
    Success,
    /// No snapshot was written: a `preBackup` hook failed, or the copy did.
    Failed,
}

impl BackupRunStatus {
    /// The persisted token, and the one every `-o json` payload reports — the
    /// DB spelling and the wire spelling are the same string by construction.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackupRunStatus::Success => "success",
            BackupRunStatus::Failed => "failed",
        }
    }

    /// Parse the persisted form. An unrecognized token reads as `Failed`: a row
    /// cfgd cannot interpret must never be treated as a restorable snapshot.
    pub(in crate::state) fn from_str(s: &str) -> Self {
        match s {
            "success" => BackupRunStatus::Success,
            _ => BackupRunStatus::Failed,
        }
    }
}

/// The values a backup run supplies when it is recorded — every column of
/// `backup_runs` except the autoincrement `id`.
///
/// Separate from [`BackupRunRecord`] so the insert cannot be called with a
/// fabricated id, and so the returned record's id is always the real rowid.
#[derive(Debug, Clone)]
pub struct BackupRunDraft {
    pub name: String,
    /// Source path, posix-folded so a run recorded on Windows compares equal to
    /// the same source recorded on Unix.
    pub source: String,
    /// Posix-folded path of the snapshot on disk; `None` when the run produced
    /// no artifact. Retention pruning treats `Some` as "there is something to
    /// delete".
    pub destination_path: Option<String>,
    /// Bytes the snapshot occupies (file length, or the sum of the copied tree).
    pub size_bytes: Option<u64>,
    pub status: BackupRunStatus,
    /// Failure detail. Set on every `Failed` run, and on a `Success` run whose
    /// `postBackup` hook failed.
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

/// A recorded backup run, as read back from `backup_runs`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRunRecord {
    pub id: i64,
    pub name: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    pub status: BackupRunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

impl BackupRunRecord {
    /// Whether the run wrote a snapshot AND every hook succeeded — the
    /// predicate a caller uses to decide its exit code. A `Success` run
    /// carrying a `postBackup` failure is deliberately not clean.
    pub fn is_clean(&self) -> bool {
        self.status == BackupRunStatus::Success && self.error.is_none()
    }

    /// Whether this run left a snapshot on disk.
    pub fn has_artifact(&self) -> bool {
        self.destination_path.is_some()
    }
}

/// A journal entry for a single action within an apply. Internal-only DAO;
/// used by rollback and apply-recovery paths.
#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub id: i64,
    pub apply_id: i64,
    /// Where the action sits in the run's plan — the position in the flattened
    /// group order, over the actions that survive `--phase`. Not a dispatch
    /// counter: package work dispatches in Rule P's tiers, not in plan order.
    pub action_index: i64,
    /// When the action actually finished: a monotonic counter assigned on the
    /// coordinator thread at collection. `None` for a row whose run was killed
    /// between its begin and its collection.
    pub completion_index: Option<i64>,
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

impl JournalEntry {
    /// Whether this entry's writes are covered by the file-backup restore
    /// path, so rollback must not report it as an unrecoverable action.
    /// Module file deploys journal as `action_type = "module"` with a
    /// `<name>:files:<n>` resource id — their writes go through
    /// `store_file_backup` like plain file actions, only the id shape differs.
    /// Env rows are file work too: `env:write:*` / `env:inject:*` journal with
    /// the target path as the id and are captured via `action_target_path` —
    /// except the live-session refresh (`env:session:refresh`, id `"refresh"`),
    /// whose session-manager state has no backup to restore.
    ///
    /// Classification is by resource identity alone, never by the phase the row
    /// was written under: a module's encryption/strategy skip journals in the
    /// `files` phase without writing anything, so a phase term would report it
    /// as restorable file work.
    pub fn is_file_work(&self) -> bool {
        self.action_type == "file"
            || self.resource_id.starts_with("file:")
            || (self.action_type == "module" && self.resource_id.split(':').nth(1) == Some("files"))
            || (self.action_type == "env" && self.resource_id != "refresh")
    }
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
