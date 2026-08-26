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
use serde::{Deserialize, Serialize};

/// Apply status for a reconciliation run.
///
/// `rename_all = "camelCase"` makes the derived `Serialize` token match
/// [`Self::display_str`] for every variant (e.g. `inProgress`), so the CLI JSON
/// surface, the human display, and the `cfgd log` column never drift. The
/// snake_case state-store persistence form is the separate `as_str` /
/// `from_str` pair and is unaffected by this attribute.
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
    /// Distinct from `as_str`, which is the snake_case state-store
    /// persistence form that round-trips through `from_str`, and from
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
    use super::{ApplyStatus, BackupRunKind};

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

    /// Same contract for `backup_runs.kind`: the stored token, the `-o json`
    /// token every snapshot entry carries, and the column default a
    /// pre-migration row reads back as. Break this only on purpose — a reword
    /// here silently reclassifies every safety snapshot already on disk.
    #[test]
    fn backup_run_kind_literals_are_a_pinned_wire_contract() {
        let pins = [
            (BackupRunKind::Run, "run"),
            (BackupRunKind::Safety, "safety"),
        ];
        for (variant, stored) in pins {
            assert_eq!(variant.as_str(), stored, "stored token drifted");
            assert_eq!(
                serde_json::to_value(variant).expect("serialize BackupRunKind"),
                serde_json::Value::String(stored.to_string()),
                "json token drifted"
            );
            assert_eq!(
                BackupRunKind::from_str(stored),
                variant,
                "stored token no longer round-trips"
            );
        }
        // The column default, and what an unreadable token folds to.
        assert_eq!(BackupRunKind::from_str("unknown"), BackupRunKind::Run);
    }
}

/// A recorded apply operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRecord {
    pub id: i64,
    pub timestamp: String,
    /// What the run was SCOPED to, which is not always a profile: an isolated
    /// `--module` run records its `module:<name>` list, and a run that
    /// resolved no profile records an empty string. Named `profile` because
    /// the column and the `-o json` field are a wire contract that predates
    /// module-scoped runs; human surfaces label it `Scope`. Rows written by an
    /// older cfgd may still hold the literal `unknown`, which no surface
    /// renders.
    pub profile: String,
    pub plan_hash: String,
    pub status: ApplyStatus,
    pub summary: Option<String>,
}

/// What the `applies.summary` column holds, and the ONE prose rendering every
/// human surface reads it back through.
///
/// The column used to be a `serde_json::json!` literal built at each of its
/// three write sites and printed VERBATIM by the two surfaces that show it —
/// `cfgd status`'s `Summary  {"failed":0,"succeeded":22,"total":22}` row and
/// `cfgd log`'s Summary column. A stored wire shape is not a sentence, and
/// `-o json` is where a machine consumer reads the shape; the human column
/// reads [`Self::prose`].
///
/// Untagged, and `Rollback` first: only a rollback row carries `rollback_of`,
/// so the two shapes cannot be confused for one another. Every `Actions` field
/// a run may not write is `#[serde(default)]`, so a row written by an older
/// cfgd — before `skipped` was split out of `succeeded` — still parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApplySummary {
    /// `cfgd rollback`'s own row: the files it put back and the files it took
    /// away.
    Rollback {
        rollback_of: i64,
        restored: usize,
        removed: usize,
    },
    /// An apply's action tally.
    #[serde(rename_all = "camelCase")]
    Actions {
        total: usize,
        succeeded: usize,
        #[serde(default)]
        skipped: usize,
        failed: usize,
        /// Actions the plan withheld before the run began (a session publish
        /// with no session manager to reach): outside `total`, which counts
        /// what the run attempted, and outside `skipped`, which ran.
        #[serde(default, skip_serializing_if = "is_zero")]
        not_attempted: usize,
        /// Actions the run planned and never reached, recorded only by the
        /// cooperative-abort close.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_run: Option<usize>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        aborted: bool,
    },
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl ApplySummary {
    /// The stored column value.
    pub fn to_column(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The sentence a human column renders for a stored value.
    ///
    /// A value nothing in this workspace writes is handed back as it stands:
    /// hiding an unreadable row's content leaves a reader with a dash and no
    /// way to find out what is in the database.
    pub fn prose(stored: &str) -> String {
        match serde_json::from_str::<Self>(stored) {
            Ok(summary) => summary.to_string(),
            Err(_) => stored.to_string(),
        }
    }
}

impl std::fmt::Display for ApplySummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rollback {
                rollback_of,
                restored,
                removed,
            } => write!(
                f,
                "{} restored, {removed} removed (rollback of apply {rollback_of})",
                restored
            ),
            Self::Actions {
                succeeded,
                skipped,
                failed,
                not_attempted,
                not_run,
                aborted,
                ..
            } => {
                write!(f, "{succeeded} succeeded")?;
                // Only when there is one to report: a clean run's row would
                // otherwise carry two zeroes for outcomes that did not occur.
                if *skipped > 0 {
                    write!(f, ", {skipped} skipped")?;
                }
                write!(f, ", {failed} failed")?;
                if *not_attempted > 0 {
                    write!(f, ", {not_attempted} not attempted")?;
                }
                if let Some(not_run) = not_run.filter(|n| *n > 0) {
                    write!(f, ", {not_run} not run")?;
                }
                if *aborted {
                    f.write_str(" (aborted)")?;
                }
                Ok(())
            }
        }
    }
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
    /// The DISPLAY operands a surface recomputed from the machine for this row,
    /// when it could — the real declared line and the real line the managed env
    /// file holds, in place of the opaque `current` / `missing or changed`
    /// markers `verify_env_items` persists.
    ///
    /// Additive and skip-if-empty, and deliberately NOT written over
    /// `expected`/`actual`: those describe the row that was STORED under this
    /// `id` at this `timestamp`, and a keyed record whose operands were
    /// silently replaced with a fresher reading describes a row nobody wrote.
    /// A consumer wanting today's truth reads these; one reconciling against
    /// the stored row reads the pair above. Both empty on a row nothing
    /// recomputed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub want: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub have: Option<String>,
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
    /// Whether the commit at `last_commit` carried a signature cfgd accepts.
    /// `None` is "not known" — never fetched since the column existed, or a
    /// checkout git could not answer for — and is not the same fact as
    /// `Some(false)`.
    pub last_commit_signed: Option<bool>,
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
    /// A digest of what the source declared for this item when the row was
    /// written. The classifier re-asks when what it now declares no longer
    /// matches; `None` is a row written before the item was fingerprinted, and
    /// reads as "no disagreement recorded", never as "it changed".
    pub content_hash: Option<String>,
}

/// A stored config hash for detecting source changes. Internal-only DAO.
#[derive(Debug, Clone)]
pub struct SourceConfigHash {
    pub source: String,
    pub config_hash: String,
    pub merged_at: String,
}

/// The `config_sources.status` token for a source whose last fetch succeeded.
/// Written by the schema's own column default on first record, and restored by
/// every later successful upsert.
pub const SOURCE_STATUS_ACTIVE: &str = "active";
/// The `config_sources.status` token for a source whose last fetch failed.
pub const SOURCE_STATUS_ERROR: &str = "error";

/// The human vocabulary for a config source's state, the source counterpart of
/// [`module_status_display`]: one derivation of the word a person reads, so
/// `cfgd source list` and `cfgd source show` can never call one recorded state
/// by two names.
///
/// The stored tokens ([`SOURCE_STATUS_ACTIVE`] / [`SOURCE_STATUS_ERROR`]) and
/// the spellings the CLI substitutes when there is no row to read (`pending`
/// for a source resolved in `sources.lock` but never fetched, `unknown` for one
/// with no record at all) are untouched WIRE values — a `-o json` consumer sees
/// exactly what it saw before.
///
/// Also what `cfgd daemon status` renders its Sources column from
/// ([`crate::daemon::SourceStatus`], which the running loop fills with
/// [`SOURCE_STATUS_ACTIVE`]), so the two screens naming the same sources cannot
/// call one state by two words. An arm is added here only when something
/// WRITES the token it maps.
pub fn source_status_display(stored: &str) -> (&'static str, Role) {
    match stored {
        SOURCE_STATUS_ACTIVE => ("Active", Role::Ok),
        SOURCE_STATUS_ERROR => ("Failed", Role::Fail),
        "pending" => ("Pending", Role::Pending),
        _ => ("Unknown", Role::Pending),
    }
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
mod apply_summary_tests {
    use super::*;

    /// The stored column is a wire shape and the human column is a sentence.
    /// `Summary  {"failed":0,"succeeded":22,"total":22}` was the stored value
    /// printed verbatim.
    #[test]
    fn a_stored_summary_reads_back_as_prose_on_a_human_surface() {
        let clean = ApplySummary::Actions {
            total: 22,
            succeeded: 22,
            skipped: 0,
            failed: 0,
            not_attempted: 0,
            not_run: None,
            aborted: false,
        };
        assert_eq!(
            ApplySummary::prose(&clean.to_column()),
            "22 succeeded, 0 failed"
        );

        let split = ApplySummary::Actions {
            total: 13,
            succeeded: 12,
            skipped: 1,
            failed: 0,
            not_attempted: 0,
            not_run: None,
            aborted: false,
        };
        assert_eq!(
            ApplySummary::prose(&split.to_column()),
            "12 succeeded, 1 skipped, 0 failed"
        );

        let aborted = ApplySummary::Actions {
            total: 9,
            succeeded: 4,
            skipped: 0,
            failed: 1,
            not_attempted: 0,
            not_run: Some(4),
            aborted: true,
        };
        assert_eq!(
            ApplySummary::prose(&aborted.to_column()),
            "4 succeeded, 1 failed, 4 not run (aborted)"
        );

        // A withheld action is outside `total` and named after the counts
        // that reconcile against it; a row with none carries no field for it.
        let withheld = ApplySummary::Actions {
            total: 2,
            succeeded: 2,
            skipped: 0,
            failed: 0,
            not_attempted: 1,
            not_run: None,
            aborted: false,
        };
        assert_eq!(
            ApplySummary::prose(&withheld.to_column()),
            "2 succeeded, 0 failed, 1 not attempted"
        );
        assert!(
            !clean.to_column().contains("notAttempted"),
            "a clean row does not carry a zero for an outcome that did not occur"
        );

        let rollback = ApplySummary::Rollback {
            rollback_of: 7,
            restored: 3,
            removed: 1,
        };
        assert_eq!(
            ApplySummary::prose(&rollback.to_column()),
            "3 restored, 1 removed (rollback of apply 7)"
        );

        for stored in [
            clean.to_column(),
            split.to_column(),
            aborted.to_column(),
            rollback.to_column(),
        ] {
            let prose = ApplySummary::prose(&stored);
            assert!(
                !prose.contains('{') && !prose.contains('"'),
                "a human surface must not read a wire shape: {prose}"
            );
        }
    }

    /// A row an older cfgd wrote — before `skipped` was split out of
    /// `succeeded` — still parses, and a value nothing can parse falls back to
    /// itself rather than vanishing.
    #[test]
    fn an_unparseable_or_older_summary_still_renders() {
        assert_eq!(
            ApplySummary::prose(r#"{"total":3,"succeeded":3,"failed":0}"#),
            "3 succeeded, 0 failed"
        );
        assert_eq!(ApplySummary::prose("running"), "running");
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

    /// Wire contract, pinned byte-for-byte: the four words this function
    /// returns are ALSO the `state` field of the `cfgd status <module>`
    /// `-o json` payload, so a machine consumer matches on them. A reword is a
    /// wire break and has to be made on purpose here rather than land as an
    /// incidental find-and-replace.
    #[test]
    fn module_status_display_words_are_a_pinned_wire_contract() {
        assert_eq!(
            module_status_display(MODULE_STATUS_INSTALLED, false).0,
            "Synced"
        );
        assert_eq!(
            module_status_display(MODULE_STATUS_INSTALLED, true).0,
            "Drifted"
        );
        assert_eq!(
            module_status_display(MODULE_STATUS_ERROR, false).0,
            "Failed"
        );
        assert_eq!(module_status_display("", false).0, "NotApplied");
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

    /// Wire contract, pinned byte-for-byte: the two tokens the
    /// `config_sources.status` column ever holds. The words on screen live in
    /// [`source_status_display`]; a reword there must never reach these.
    #[test]
    fn source_status_literals_are_a_pinned_wire_contract() {
        assert_eq!(SOURCE_STATUS_ACTIVE, "active");
        assert_eq!(SOURCE_STATUS_ERROR, "error");
    }

    #[test]
    fn a_source_token_maps_to_one_display_word_and_one_role() {
        assert_eq!(
            source_status_display(SOURCE_STATUS_ACTIVE),
            ("Active", Role::Ok)
        );
        assert_eq!(
            source_status_display(SOURCE_STATUS_ERROR),
            ("Failed", Role::Fail)
        );
        assert_eq!(source_status_display("pending"), ("Pending", Role::Pending));
        for stored in ["unknown", "syncing", ""] {
            assert_eq!(
                source_status_display(stored),
                ("Unknown", Role::Pending),
                "{stored:?} should read Unknown"
            );
        }
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

/// The word a person reads for a recorded backup run's outcome, with the role
/// that tints it — the display counterpart of [`BackupRunStatus::as_str`],
/// which stays the untouched wire token, and the backup analogue of
/// [`source_status_display`]. Its arms are matched against `as_str()` rather
/// than against literals of their own, so the stored spelling and the shown one
/// cannot drift apart.
///
/// A token neither arm recognises is shown VERBATIM rather than renamed:
/// `BackupRunStatus::from_str` reads one as `Failed` for safety, and a screen
/// asserting "Failed" about a row cfgd could not interpret would be a claim it
/// has no basis for. It carries `Role::Pending` for the same reason
/// `source_status_display`'s unknown arm does: cfgd cannot say.
pub fn backup_run_status_display(stored: &str) -> (&str, Role) {
    if stored == BackupRunStatus::Success.as_str() {
        ("Success", Role::Ok)
    } else if stored == BackupRunStatus::Failed.as_str() {
        ("Failed", Role::Fail)
    } else {
        (stored, Role::Pending)
    }
}

/// What produced a `backup_runs` row.
///
/// The ledger holds both, because retention prunes and
/// [`crate::backup::list_snapshots`] restores from every payload on disk
/// whatever wrote it. What the two kinds are NOT interchangeable for is the
/// question "when did this unit last back up": a restore's safety snapshot is a
/// side effect of putting data back, so counting it as the unit's last run
/// re-anchors an interval schedule on the restore's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupRunKind {
    /// A backup of the unit: `cfgd backup run`, an apply's pending backups, or
    /// the daemon's timer.
    Run,
    /// The pre-restore copy `cfgd backup restore` takes of what it is about to
    /// overwrite.
    Safety,
}

impl BackupRunKind {
    /// The persisted token, and the one every `-o json` payload reports — the
    /// DB spelling and the wire spelling are the same string by construction.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackupRunKind::Run => "run",
            BackupRunKind::Safety => "safety",
        }
    }

    /// The word a person reads in the `Kind` column, the display counterpart of
    /// [`BackupRunKind::as_str`] — which stays the untouched DB and `-o json`
    /// token, exactly as [`super::ApplyStatus`] and
    /// [`super::backup_run_status_display`] split the two.
    ///
    /// Without it the human table printed the wire token `run` beside a `Last
    /// Run` cell that already said `Success`: two stored enums, adjacent
    /// surfaces of one command, two policies.
    pub fn display_str(&self) -> &'static str {
        match self {
            BackupRunKind::Run => "Run",
            BackupRunKind::Safety => "Safety",
        }
    }

    /// Whether this row is a restore's safety copy rather than a backup of the
    /// unit.
    pub fn is_safety(&self) -> bool {
        matches!(self, BackupRunKind::Safety)
    }

    /// Parse the persisted form. An unrecognized token reads as `Run`, matching
    /// the column default every pre-migration row reads back as: a row cfgd
    /// cannot interpret is history it must not silently hide from the unit's
    /// last-run answer.
    pub(in crate::state) fn from_str(s: &str) -> Self {
        match s {
            "safety" => BackupRunKind::Safety,
            _ => BackupRunKind::Run,
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
    pub kind: BackupRunKind,
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
    pub kind: BackupRunKind,
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

/// The recorded id of the live-session refresh, once
/// `env:session:refresh` has been split into its `(type, id)` halves.
/// Two readers match on it — the rollback classifier and the status
/// dashboard's Managed Resources table — and a second spelling would let one
/// of them treat the session surface as an ordinary env file.
pub const ENV_SESSION_RESOURCE_ID: &str = "refresh";

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
            || (self.action_type == "env" && self.resource_id != ENV_SESSION_RESOURCE_ID)
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
