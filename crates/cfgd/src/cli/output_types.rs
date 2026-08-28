use std::collections::BTreeMap;

use serde::Serialize;

// --- Command output types ---

/// Machine-stable cause class for a degraded source-decision classification
/// (`classificationDegradedCode` in the `status` / `decide` payloads). A
/// closed set, deliberately coarse: each variant maps to a distinct operator
/// remedy, and the human detail stays in `classificationDegradedReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClassificationDegradedCode {
    /// The decision store itself could not be read — fix the state
    /// directory / database before trusting any decision surface.
    DecisionStoreUnreadable,
    /// A source's cached config failed to load or compose — re-sync or
    /// inspect the source.
    SourceUnreadable,
    /// A local package-manifest reference failed to resolve (Brewfile,
    /// `package.json`, `Cargo.toml`, apt list) — fix the referenced file.
    ManifestUnreadable,
    /// Anything the classes above cannot claim.
    ClassificationFailed,
}

impl ClassificationDegradedCode {
    /// Classify a degradation error by the FIRST typed cause in its chain —
    /// the innermost `anyhow` contexts wrap the typed error that actually
    /// failed, so the first hit names the failing input rather than a
    /// wrapper.
    pub fn from_error(e: &anyhow::Error) -> Self {
        use cfgd_core::errors::CfgdError;
        for cause in e.chain() {
            if let Some(err) = cause.downcast_ref::<CfgdError>() {
                return match err {
                    CfgdError::State(_) => Self::DecisionStoreUnreadable,
                    CfgdError::Source(_) | CfgdError::Composition(_) | CfgdError::Config(_) => {
                        Self::SourceUnreadable
                    }
                    CfgdError::Package(_) | CfgdError::File(_) | CfgdError::Io(_) => {
                        Self::ManifestUnreadable
                    }
                    _ => Self::ClassificationFailed,
                };
            }
            if cause
                .downcast_ref::<cfgd_core::errors::StateError>()
                .is_some()
            {
                return Self::DecisionStoreUnreadable;
            }
        }
        Self::ClassificationFailed
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogOutput {
    pub entries: Vec<cfgd_core::state::ApplyRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct LogShowOutputOutput {
    pub apply_id: i64,
    pub entries: Vec<LogShowEntryOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct LogShowEntryOutput {
    pub phase: String,
    pub resource_id: String,
    pub action_type: String,
    pub output: String,
}

/// Structured payload for `cfgd apply`. Carries the result of an apply run for
/// `-o json|yaml|jsonpath|template` consumers (CI, scripts, the operator). The
/// optional fields cover the no-op paths (`nothing_to_do`, `aborted`) where no
/// reconciler run happened and there is no apply_id to report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutput {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_id: Option<i64>,
    pub succeeded: usize,
    /// Actions that ran and changed nothing — the rollup's `N skipped`.
    pub skipped: usize,
    pub failed: usize,
    /// Actions the plan withheld before the run (the rollup's `(N not
    /// attempted — <reason>)`); outside `succeeded`/`skipped`/`failed` and
    /// outside the plan's `totalActions`, exactly as the human line prices it.
    pub not_attempted: usize,
    // `BTreeMap`, not `HashMap`: this field serializes into `-o json` /
    // `-o yaml`, and with no `preserve_order` feature on `serde_json` a
    // `HashMap` writes its keys in per-process-random order — byte-unstable
    // for a docs capture, a golden test, or a checksum-diffing consumer.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub source_commits: BTreeMap<String, String>,
    /// Schedule-less `spec.backups[]` runs executed alongside this apply.
    /// Empty (and omitted from the wire) on the no-op paths and whenever the
    /// profile declares no schedule-less backups.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub backups: Vec<BackupRunOutput>,
}

impl ApplyOutput {
    pub fn nothing_to_do() -> Self {
        Self {
            status: "nothingToDo".to_string(),
            apply_id: None,
            succeeded: 0,
            skipped: 0,
            failed: 0,
            not_attempted: 0,
            source_commits: BTreeMap::new(),
            backups: Vec::new(),
        }
    }

    pub fn aborted() -> Self {
        Self {
            status: "aborted".to_string(),
            apply_id: None,
            succeeded: 0,
            skipped: 0,
            failed: 0,
            not_attempted: 0,
            source_commits: BTreeMap::new(),
            backups: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackOutput {
    pub apply_id: i64,
    pub files_restored: usize,
    pub files_removed: usize,
    pub non_file_actions: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutput {
    pub local_pulled: bool,
    /// Why the local repository could not be pulled, absent when it was or
    /// when the config directory is under no version control. The human
    /// verdict withholds `Synced` on the same fact, so a consumer can see
    /// what the report said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_pull_error: Option<String>,
    pub sources: Vec<SourceSyncOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullOutput {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinOutput {
    pub server_status: String,
    pub config_changed: bool,
    pub drift_count: usize,
    pub drift_status: String,
    pub server_pushed_config: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffOutput {
    /// One record per managed file that does NOT match desired state, in the
    /// same shape `verify` reports a resource: what was expected, what was
    /// found. An unevaluable `strategy: Patch` file lands here with the reason
    /// as its `actual`, so a blocked filter is visible to a structured consumer
    /// and not only in the terminal.
    pub files: Vec<cfgd_core::providers::FileDriftResult>,
    pub packages: Vec<PackageDrift>,
    pub system: Vec<SystemDriftOutput>,
    /// One record per system configurator whose drift check could not run.
    /// Deliberately separate from `system`: a check that errored reports
    /// neither drift nor cleanliness, and a consumer reading only
    /// `summary.hasSystemDrift` would otherwise read "the check failed" as
    /// "the machine is in sync".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub system_errors: Vec<SystemCheckError>,
    /// One record per declared env var or alias whose line in the primary
    /// managed env file no longer matches what `spec.env`/`spec.aliases`
    /// declares — the same per-item check `cfgd verify` persists as drift,
    /// run here read-only.
    pub env: Vec<EnvDriftOutput>,
    /// Set only by `diff --module`: the env check there resolves the whole
    /// active profile (not just the named module), and that resolution is a
    /// single all-or-nothing call rather than a per-item one — an unrelated
    /// module's resolution failure lands here instead of aborting a diff
    /// whose Files/Packages phases already succeeded for the module asked
    /// about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_check_error: Option<String>,
    pub summary: DiffSummary,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub has_file_drift: bool,
    pub has_pkg_drift: bool,
    pub has_system_drift: bool,
    /// At least one configurator's drift check errored, so the system verdict
    /// is unknown rather than clean. Read alongside `has_system_drift` by
    /// every consumer that treats "no drift" as "nothing to do".
    pub system_check_failed: bool,
    pub has_env_drift: bool,
    /// The env check itself could not run (`diff --module` only — see
    /// `DiffOutput::env_check_error`), so `has_env_drift` is not a verdict.
    /// Read alongside `has_env_drift` the same way `system_check_failed` is
    /// read alongside `has_system_drift`.
    pub env_check_failed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageDrift {
    pub manager: String,
    /// `missing` | `extra` | `provision` | `refused`. `provision`/`refused` are
    /// package-less rows: the manager itself is what drifts, not a package it
    /// would install, so `packages` stays empty for both. `provision` names the
    /// plan-state fact (matches `ManagerAction::Provision`'s machine vocabulary
    /// across `diff`/`verify`/`status`); the mechanism itself keeps the
    /// "bootstrap" word in `bootstrap_method` and in the human render.
    pub shape: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    /// The method a `shape: "provision"` row would self-install with — naming
    /// precedent: `DoctorManagerCheck.bootstrap_method`. `Some` only when
    /// `shape == "provision"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap_method: Option<String>,
    /// Why a `shape: "refused"` row cannot self-install. `Some` only when
    /// `shape == "refused"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemDriftOutput {
    pub key: String,
    pub expected: String,
    pub actual: String,
}

/// A configurator whose drift check itself failed — the machine's state for
/// that key is unknown.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemCheckError {
    pub key: String,
    pub error: String,
}

/// One drifted env row: a declared env var or alias whose deployed line
/// diverges from what `spec.env`/`spec.aliases` declares (`kind` is
/// `"env-var"` or `"alias"`), the primary managed env file itself gone stale
/// (`kind` `"env"`), or a shell rc's `cfgd` source line missing (`kind`
/// `"env-rc"`). Matches `cfgd_core::reconciler::VerifyResult::resource_type`
/// for this check byte-for-byte, so a consumer joining this against a
/// `cfgd verify` or recorded-drift row needs no second vocabulary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvDriftOutput {
    pub kind: String,
    pub name: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSyncOutput {
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOutput {
    pub context: String,
    pub phases: Vec<PlanPhaseOutput>,
    pub total_actions: usize,
    /// The sources this plan's composition drew a layer from, in layering
    /// order — the structured counterpart of the header's `Sources` row.
    /// Without it a consumer reading a plan carrying `<- team` provenance on
    /// its actions has no way to learn what `team` is or which of its profiles
    /// this machine subscribed to. Empty (and omitted from the wire) for a run
    /// that composed none, so every existing payload stays byte-exact.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<cfgd_core::reconciler::ComposedSource>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Names of schedule-less `spec.backups[]` entries that a non-dry-run
    /// apply would run alongside this plan. Backups are not reconciler
    /// actions (no diff against desired state — they always run), so they
    /// are reported here rather than folded into `total_actions`. Empty (and
    /// omitted from the wire) when the profile declares none, or every
    /// declared backup carries a `schedule`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pending_backups: Vec<String>,
    /// The source decisions awaiting the operator, whose resources are
    /// withheld from `phases[]` and from `totalActions` above.
    ///
    /// The structured counterpart of the human preview's "Pending Decisions"
    /// block: under `-o json` that block is suppressed with every other human
    /// row, so without this key a consumer would see a smaller plan with
    /// nothing to explain it. Empty (and omitted from the wire) when no
    /// decision is outstanding. Every entry is unresolved by construction, so
    /// its `resolvedAt` / `resolution` are null — the answered rows are in
    /// `rejectedDecisions` below.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pending_decisions: Vec<cfgd_core::state::PendingDecision>,
    /// The source decisions the operator DECLINED, whose resources are
    /// withheld from `phases[]` and from `totalActions` just as an awaiting
    /// one's are.
    ///
    /// Separate from `pendingDecisions` because the two ask for different
    /// things — one wants an answer, the other already has one and would need
    /// reversing — but reported for the same reason: a resource absent from
    /// `phases[]` must always be explained by a decision the consumer can see.
    /// Every entry carries a populated `resolvedAt` / `resolution`. Empty (and
    /// omitted from the wire) when nothing this run declares was declined.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejected_decisions: Vec<cfgd_core::state::PendingDecision>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPhaseOutput {
    pub phase: String,
    /// The phase's owner groups, in `Owner::sort_key` order — the same order
    /// and the same grouping the human tree draws, so a consumer that renders
    /// the payload reproduces the CLI's ordering without a comparator of its
    /// own. Never empty: a phase whose every group was filtered away is
    /// dropped from `phases[]`.
    pub groups: Vec<PlanGroupOutput>,
}

/// One owner's slice of a phase: who declared the work, plus the actions.
///
/// `owner` is the reconciler's own [`cfgd_core::reconciler::Owner`], so the
/// wire's `kind` vocabulary cannot drift from the one the planner assigns, and
/// `token` is [`cfgd_core::reconciler::Owner::token`]'s rendering of it — the
/// exact string the tree prints, carried so a consumer never re-implements the
/// `kind:name` grammar.
///
/// The fields are private and [`PlanGroupOutput::new`] is the only constructor,
/// so a `token` naming an owner other than the group's own is unrepresentable
/// rather than merely discouraged: no caller can write a struct literal or
/// reassign `owner` out from under a token already derived from it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanGroupOutput {
    owner: cfgd_core::reconciler::Owner,
    token: String,
    actions: Vec<PlanActionOutput>,
}

impl PlanGroupOutput {
    /// Build a group from its owner, deriving `token` rather than taking it.
    pub fn new(owner: cfgd_core::reconciler::Owner, actions: Vec<PlanActionOutput>) -> Self {
        Self {
            token: owner.token(),
            owner,
            actions,
        }
    }

    pub fn owner(&self) -> &cfgd_core::reconciler::Owner {
        &self.owner
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn actions(&self) -> &[PlanActionOutput] {
        &self.actions
    }
}

/// The `cfgd:managers` group's per-action structured payload — spec §7's
/// `{manager, state, via, requires}` shape, populated only for
/// `Action::Manager` rows (`PlanActionOutput.manager`).
///
/// `manager` names the row's subject: the manager itself for
/// `RefreshIndex`/`Provision`/`Refuse`, and the TOOL for `Prerequisite` — the
/// installer goes in `via` instead, mirroring the human line's "{installer}
/// install {tool}" subject/actor split. `requires` is `ManagerAction::depends_on`
/// verbatim (full `manager:...` node ids), so a consumer resolves an entry
/// against a sibling row's `description` with no second id scheme.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerActionOutput {
    pub manager: String,
    /// `present` (refresh) | `provisioned` | `prerequisite` | `refused`.
    /// `refused` is not in spec §7's literal enum — the spec's variant list
    /// names `Refuse` as a node this task must give a payload, and a state
    /// enum a refusal cannot express in is a payload that silently drops it.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// The other managers this node's ONE `via` install also provisions.
    /// Non-empty only for `state == "provisioned"`, and omitted from the wire
    /// otherwise, so a consumer reading only `manager` sees exactly what it
    /// always saw. Read it to learn what a single provision row really
    /// delivers: `manager: "npm"` with `batched: ["pipx"]` is one
    /// `apt-get install` covering both.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batched: Vec<String>,
    /// Why this host cannot provision the manager. `Some` only when
    /// `state == "refused"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanActionOutput {
    pub description: String,
    #[serde(rename = "type")]
    pub action_type: String,
    /// Absolute filesystem path(s) this action writes. Empty (and omitted from
    /// the wire) for actions with no direct filesystem target — package
    /// installs, system-configurator writes, live-session refresh. Lets `-o
    /// json` consumers (CI, blast-radius tooling) read the target without
    /// scraping `description`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// The ConfigSource that delivered the resource's body, when not local.
    /// `Some(source_name)` for source-delivered modules/files/packages; omitted
    /// from the wire (and `None`) for consumer-local resources. Lets `-o json`
    /// consumers read provenance without scraping the ` <- <source>` suffix off
    /// `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The row's `cfgd:managers` detail, `Some` only for `Action::Manager`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager: Option<ManagerActionOutput>,
    /// What the action PRODUCES, as the tree states it beside the subject
    /// (`5 already deployed`, `3 vars, 3 aliases`) — the plan preview's
    /// bullet detail and the apply row's detail are this one string. Omitted
    /// for an action with no produced count; never folded into
    /// `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorOutput {
    pub config: DoctorConfigCheck,
    pub git: bool,
    pub secrets: DoctorSecretsCheck,
    pub package_managers: Vec<DoctorManagerCheck>,
    pub modules: Vec<DoctorModuleCheck>,
    pub system_configurators: Vec<DoctorConfiguratorCheck>,
    pub profiles: Vec<DoctorProfileLayoutCheck>,
}

/// Per-profile layout-form check: canonical bundle (`profiles/<name>/profile.yaml`)
/// vs the legacy flat form (`profiles/<name>.yaml`). `error` carries the
/// ambiguity message when both forms coexist on disk.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorProfileLayoutCheck {
    pub name: String,
    pub legacy: bool,
    pub path: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorConfigCheck {
    pub valid: bool,
    pub path: String,
    pub name: Option<String>,
    pub profile: Option<String>,
    pub error: Option<String>,
    /// Typed classification driving rendering and verdict scoring. Skipped
    /// from serialization: the consumer-facing JSON field set stays frozen —
    /// `valid`/`error` carry the same values as before this field existed.
    #[serde(skip)]
    pub state: DoctorConfigState,
}

/// How the doctor config check classified the config file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DoctorConfigState {
    /// Present and parseable.
    Valid,
    /// Absent at the derived default path — a fresh-machine state: rendered
    /// as a Warn and the verdict still passes.
    MissingAtDefault,
    /// Absent at a user-supplied `--config`/`CFGD_CONFIG`/`--config-dir`
    /// path — user error: rendered as a Fail and the verdict fails, so
    /// `cfgd doctor && cfgd apply` stops instead of apply hard-failing on
    /// the same path.
    MissingAtExplicit,
    /// Present but unparseable — Fail.
    Invalid,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSecretsCheck {
    pub sops_available: bool,
    pub sops_version: Option<String>,
    pub age_key_exists: bool,
    pub age_key_path: Option<String>,
    pub sops_config_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sops_config_path: Option<String>,
    pub providers: Vec<DoctorProviderCheck>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorProviderCheck {
    pub name: String,
    pub available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorManagerCheck {
    pub name: String,
    pub available: bool,
    pub declared: bool,
    pub can_bootstrap: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap_method: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorModuleCheck {
    pub name: String,
    pub valid: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub packages: Vec<DoctorModulePackageCheck>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorModulePackageCheck {
    pub name: String,
    pub resolved_name: String,
    pub manager: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorConfiguratorCheck {
    pub name: String,
    pub available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceListEntry {
    pub name: String,
    /// The declared origin. Every field below `name` and `status` is an
    /// `Option`, because a row is not always a `spec.sources[]` entry: the
    /// daemon reports the implicit `local` layer, which declares no origin,
    /// no priority and no signing demand. `None` renders `-` (or drops the
    /// column when no row can fill it) and serializes `null`; a default such
    /// as `0` or `false` would state a fact nobody declared.
    pub url: Option<String>,
    pub priority: Option<u32>,
    pub version: Option<String>,
    pub status: String,
    /// The ISO 8601 stamp of the last fetch. The human table humanizes it
    /// (`2h ago`); the payload keeps the instant, which is the only form a
    /// machine consumer can compare or re-render.
    pub last_fetched: Option<String>,
    /// Whether the fetched commit carried a signature cfgd accepts. `None` is
    /// "not known", never "unsigned".
    pub signed: Option<bool>,
    /// Whether the subscription DEMANDS a signed HEAD. Distinct from `signed`,
    /// which reports what the last fetch found: two sources with signed HEADs,
    /// one demanding signatures and one not, are not the same subscription.
    /// `None` is "nothing declared", never "not required".
    pub require_signed_commits: Option<bool>,
    /// The commit the cached checkout is at, full length. The human table
    /// shortens it through `short_commit`; the payload keeps the whole id,
    /// which is the only form a machine consumer can match against a remote.
    pub last_commit: Option<String>,
    /// Unresolved drift attributed to this source, when the surface rendering
    /// the row knows it. `None` is "not known" and renders `-`; every producer
    /// answers `None` today, so the column is dropped from every render (see
    /// `cfgd_core::daemon::SourceStatus::drift_count` for why the daemon's own
    /// rows cannot answer it either). The machine-wide total is a header fact,
    /// never a row's.
    pub drift_count: Option<u32>,
}

/// One `spec.backups[]` entry plus its last recorded run, for
/// `cfgd backup list`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupListEntry {
    pub name: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    pub retention: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// `Some(false)` when the last run wrote a snapshot but a `postBackup`
    /// hook still failed (`BackupRunRecord::is_clean() == false`) — lets a
    /// structured consumer gate on cleanliness without parsing
    /// `lastRunStatus` text. `None` when there is no recorded run yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_clean: Option<bool>,
    /// When the daemon's timer will next fire this unit, as an ISO 8601 UTC
    /// stamp on the same scale as `lastRunAt`. `None` for a schedule-less unit
    /// (it runs during `cfgd apply`, on no clock of its own) and for a
    /// schedule with no upcoming occurrence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    /// How many snapshots this unit currently holds on disk. `None` when the
    /// state store could not be read — an unknown count must not be reported
    /// as zero, which is a unit whose snapshots have all been pruned away.
    /// Backups of the unit only: the safety copy a restore leaves beside the
    /// source is a sidecar, not a snapshot, and is never counted here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<usize>,
}

/// One snapshot on disk, for `cfgd backup list <name> --snapshots`.
///
/// `name` is the snapshot's path relative to the unit's destination, so a
/// nested `namePattern` renders `daily/notes.txt.20260801T031500Z` — the exact
/// string `cfgd backup restore --at` accepts.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSnapshotEntry {
    pub name: String,
    /// ISO 8601 UTC time the run that wrote the snapshot finished, on the same
    /// scale as `BackupListEntry::last_run_at`.
    pub created: String,
    pub size_bytes: u64,
}

impl From<&cfgd_core::backup::SnapshotInfo> for BackupSnapshotEntry {
    fn from(info: &cfgd_core::backup::SnapshotInfo) -> Self {
        Self {
            name: info.name.clone(),
            created: info.created.clone(),
            size_bytes: info.size_bytes,
        }
    }
}

/// Outcome of `cfgd backup restore`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRestoreOutput {
    pub name: String,
    /// The snapshot restored from, as `BackupSnapshotEntry::name` spells it.
    pub snapshot: String,
    /// Where the snapshot landed — the unit's source, or `--to`.
    pub restored_to: String,
    /// Whether the overlay actually ran and completed.
    pub restored: bool,
    /// The overlay completed AND every hook succeeded — the same predicate
    /// `BackupRunOutput::clean` carries, and what the exit code gates on.
    pub clean: bool,
    /// Size recorded for the snapshot that was restored, matching
    /// `BackupSnapshotEntry::size_bytes` for the same snapshot.
    pub size_bytes: u64,
    /// The `.cfgd-backup` sidecar holding the source's previous contents,
    /// written beside it immediately before the overlay. Omitted when the
    /// restore was redirected away from the live source or the source did not
    /// exist yet. Not one of the unit's snapshots: it lists under neither
    /// `backup list` nor `--snapshots`, and no retention prunes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_copy: Option<String>,
    /// Whether `safety_copy` already held the source's bytes from an earlier
    /// displacement and was reused rather than written by this restore.
    /// Present exactly when `safety_copy` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_copy_reused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<&cfgd_core::backup::RestoreOutcome> for BackupRestoreOutput {
    fn from(outcome: &cfgd_core::backup::RestoreOutcome) -> Self {
        Self {
            name: outcome.name.clone(),
            snapshot: outcome.snapshot.clone(),
            restored_to: outcome.restored_to.clone(),
            restored: outcome.restored,
            clean: outcome.is_clean(),
            size_bytes: outcome.size_bytes,
            safety_copy: outcome
                .safety_copy
                .as_ref()
                .map(|s| cfgd_core::to_posix_string(&s.path)),
            safety_copy_reused: outcome.safety_copy.as_ref().map(|s| s.reused),
            error: outcome.error.clone(),
        }
    }
}

/// A restore the operator declined at the confirmation prompt.
///
/// Deliberately not a [`BackupRestoreOutput`] with `restored: false`: that
/// struct's `clean` is the predicate the exit code gates on, and a decline exits
/// `0` — reporting `clean: false` alongside a zero exit would make every
/// consumer that trusts one of the two wrong. `declined` says what happened
/// without claiming anything about a restore that never ran.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRestoreDeclinedOutput {
    pub name: String,
    /// The snapshot that would have been restored.
    pub snapshot: String,
    /// Where it would have landed.
    pub restored_to: String,
    /// Always `false`; present so a consumer can read the same key on both the
    /// declined and the completed payload.
    pub restored: bool,
    /// Always `true` — the discriminator between this payload and a restore
    /// that ran.
    pub declined: bool,
}

/// Outcome of one unit run by `cfgd backup run`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRunOutput {
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_path: Option<String>,
    /// [`cfgd_core::state::BackupRunRecord::is_clean`] — the run wrote a
    /// snapshot AND every hook succeeded. `false` on a run that recorded
    /// `Success` but a `postBackup` hook still failed.
    pub clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<&cfgd_core::state::BackupRunRecord> for BackupRunOutput {
    fn from(record: &cfgd_core::state::BackupRunRecord) -> Self {
        Self {
            name: record.name.clone(),
            status: record.status.as_str().to_string(),
            destination_path: record.destination_path.clone(),
            clean: record.is_clean(),
            error: record.error.clone(),
        }
    }
}

impl BackupRunOutput {
    /// The payload entry for one unit's [`cfgd_core::backup::BackupRunReport`].
    ///
    /// The ONE mapping, so `cfgd backup run`, `cfgd apply` and the daemon emit
    /// the same three shapes for the same three outcomes: a recorded run, a
    /// unit another writer held (`"skipped"`, which is not a failure of this
    /// run), and a state-store refusal (`"failed"`, which is). The unit's name
    /// is the caller's because a record-less report has no name of its own.
    pub fn from_report(name: &str, report: &cfgd_core::backup::BackupRunReport) -> Self {
        match (&report.record, &report.skipped) {
            (Some(record), _) => Self::from(record),
            (None, Some(holder)) => Self {
                name: name.to_string(),
                status: "skipped".to_string(),
                destination_path: None,
                clean: false,
                error: Some(format!("already running ({holder})")),
            },
            (None, None) => Self {
                name: name.to_string(),
                status: cfgd_core::state::BackupRunStatus::Failed
                    .as_str()
                    .to_string(),
                destination_path: None,
                clean: false,
                error: report.error.clone(),
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceShowOutput {
    pub name: String,
    pub url: String,
    pub branch: String,
    pub priority: u32,
    pub accept_recommended: bool,
    pub profile: Option<String>,
    pub sync_interval: String,
    pub auto_apply: bool,
    pub pin_version: Option<String>,
    pub state: Option<SourceStateInfo>,
    pub managed_resources: Vec<SourceResourceEntry>,
    /// Module names this source declares deliverable — its manifest
    /// `spec.provides.modules` allow-list (the module bodies it offers to
    /// subscribers). Empty (and omitted from the wire) when the source delivers
    /// no modules or its manifest could not be loaded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
    /// What this source enforces, combining the manifest's own
    /// `policy.constraints` with this subscriber's overrides
    /// (`subscription.allowScripts`, `subscription.requireSignedCommits`).
    /// `None` when the manifest could not be loaded, since the constraints it
    /// would combine with are unknown. Omitted from the wire in that case
    /// (matches the envelope discipline of dropping empty fields), rather
    /// than serializing as a `null` a consumer has to special-case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<SourcePolicyOutput>,
    /// What the source's own manifest DECLARES — the same facts the human
    /// render's `Manifest` and `Profiles` sections read. `None` when the
    /// manifest could not be loaded, and omitted from the wire in that case
    /// rather than serializing a `null` a consumer has to special-case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<SourceManifestOutput>,
}

/// A config source's manifest as a structured payload, shared by `source show`
/// and `source add` so both answer "what does this source provide" with one
/// shape.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceManifestOutput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub profiles: Vec<SourceManifestProfileOutput>,
    pub modules: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceManifestProfileOutput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inherits: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePolicyOutput {
    /// Whether this source's HEAD commit must carry a valid signature — the
    /// OR of the subscriber's own `subscription.requireSignedCommits` and the
    /// manifest's `policy.constraints.requireSignedCommits`. This is the
    /// DEMAND, not the enforcement: `spec.security.allowUnsigned` can bypass
    /// it entirely, which `signed_commits_bypassed` says explicitly rather
    /// than leaving this flag to read as unqualified enforcement.
    pub require_signed_commits: bool,
    /// Whether `spec.security.allowUnsigned` bypasses `require_signed_commits`
    /// for this subscriber — always `false` when the demand above is itself
    /// `false`, since there is nothing to bypass.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signed_commits_bypassed: bool,
    /// Whether this source's lifecycle scripts run — the subscriber's
    /// `allowScripts` opt-in OR the manifest not constraining scripts at all.
    pub scripts_allowed: bool,
    /// Whether this source may deliver `${secret:…}` references.
    pub secrets_read_allowed: bool,
    /// Whether this source may deliver `system:` configurator settings.
    pub system_changes_allowed: bool,
    /// Glob patterns restricting which file targets this source may deploy
    /// to. Empty means no restriction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_target_paths: Vec<String>,
    /// Encryption the manifest's `policy.constraints.encryption` imposes on
    /// files this source delivers. `None` when the manifest declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<SourceEncryptionOutput>,
}

/// The `policy.constraints.encryption` block, reshaped for display —
/// `cfgd_core::config::EncryptionConstraint` with its enum/optional fields
/// rendered as plain strings so a `source show` consumer needs no second
/// vocabulary for the same mode/backend names the manifest schema documents.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceEncryptionOutput {
    /// Glob patterns or explicit paths that must be encrypted.
    pub required_targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStateInfo {
    pub status: String,
    /// The ISO 8601 stamp of the last fetch; the human render humanizes it.
    pub last_fetched: Option<String>,
    pub last_commit: Option<String>,
    /// Whether the fetched commit carried a signature cfgd accepts. `None` is
    /// "not known", never "unsigned".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<bool>,
    pub version: Option<String>,
    /// Resolved tag name from sources.lock (None for HEAD-tracking sources).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_ref: Option<String>,
    /// 40-char commit SHA from sources.lock at time of last lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_commit: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceResourceEntry {
    pub resource_type: String,
    pub resource_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileListEntry {
    pub name: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherits: Option<String>,
    pub module_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ModuleSearchResult {
    pub name: String,
    pub registry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct RegistryListEntry {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasListEntry {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct KeyListEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

// --- Compliance command output types ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceSnapshotOutput {
    pub snapshot: cfgd_core::compliance::ComplianceSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceHistoryOutput {
    pub entries: Vec<cfgd_core::state::ComplianceHistoryRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct ComplianceDiffOutput {
    pub id1: i64,
    pub id2: i64,
    pub added: Vec<cfgd_core::compliance::ComplianceCheck>,
    pub removed: Vec<cfgd_core::compliance::ComplianceCheck>,
    pub changed: Vec<ComplianceCheckChange>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceCheckChange {
    pub key: String,
    pub old_status: String,
    pub new_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::compliance::{
        ComplianceCheck, ComplianceSnapshot, ComplianceStatus, ComplianceSummary, MachineInfo,
    };
    use cfgd_core::state::{ApplyRecord, ApplyStatus, ComplianceHistoryRow};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    /// The degradation code is a CLOSED, camelCase token set, and the
    /// error-chain mapping lands each cause class on its own token — the
    /// machine-stable half of the degraded payload pair.
    #[test]
    fn classification_degraded_code_maps_causes_to_stable_camelcase_tokens() {
        use cfgd_core::errors::{CfgdError, StateError};

        let store_err =
            anyhow::Error::from(CfgdError::State(StateError::Database("locked".into())))
                .context("source classification failed");
        assert_eq!(
            ClassificationDegradedCode::from_error(&store_err),
            ClassificationDegradedCode::DecisionStoreUnreadable
        );

        let bare = anyhow::anyhow!("something unexpected");
        assert_eq!(
            ClassificationDegradedCode::from_error(&bare),
            ClassificationDegradedCode::ClassificationFailed
        );

        for (code, token) in [
            (
                ClassificationDegradedCode::DecisionStoreUnreadable,
                "decisionStoreUnreadable",
            ),
            (
                ClassificationDegradedCode::SourceUnreadable,
                "sourceUnreadable",
            ),
            (
                ClassificationDegradedCode::ManifestUnreadable,
                "manifestUnreadable",
            ),
            (
                ClassificationDegradedCode::ClassificationFailed,
                "classificationFailed",
            ),
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(token));
        }
    }

    #[test]
    fn log_output_serializes_entries_array_under_camelcase_key() {
        let v = LogOutput {
            entries: vec![ApplyRecord {
                id: 7,
                timestamp: "2026-01-02T03:04:05Z".to_string(),
                profile: "default".to_string(),
                plan_hash: "deadbeef".to_string(),
                // InProgress is the variant where the apply/status/log tokens
                // historically drifted; assert the `cfgd log -o json` surface
                // now emits the unified camelCase token.
                status: ApplyStatus::InProgress,
                summary: Some("ok".to_string()),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        let entries = json["entries"].as_array().expect("entries is array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!(7));
        assert_eq!(entries[0]["planHash"], json!("deadbeef"));
        assert_eq!(entries[0]["status"], json!("inProgress"));
        assert_eq!(
            entries[0]["status"],
            json!(ApplyStatus::InProgress.display_str())
        );
    }

    #[test]
    fn log_show_output_output_pins_apply_id_and_nested_entries() {
        let v = LogShowOutputOutput {
            apply_id: 42,
            entries: vec![LogShowEntryOutput {
                phase: "pre".to_string(),
                resource_id: "res-1".to_string(),
                action_type: "install".to_string(),
                output: "ok\n".to_string(),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["applyId"], json!(42));
        let entries = json["entries"].as_array().expect("entries is array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["phase"], json!("pre"));
        assert_eq!(entries[0]["resourceId"], json!("res-1"));
        assert_eq!(entries[0]["actionType"], json!("install"));
        assert_eq!(entries[0]["output"], json!("ok\n"));
    }

    #[test]
    fn log_show_entry_output_uses_camelcase_for_every_field() {
        let v = LogShowEntryOutput {
            phase: "main".to_string(),
            resource_id: "res-2".to_string(),
            action_type: "configure".to_string(),
            output: "done".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["phase"], json!("main"));
        assert_eq!(json["resourceId"], json!("res-2"));
        assert_eq!(json["actionType"], json!("configure"));
        assert_eq!(json["output"], json!("done"));
    }

    #[test]
    fn apply_output_nothing_to_do_emits_sentinel_status_and_skips_empty_optionals() {
        let v = ApplyOutput::nothing_to_do();
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("nothingToDo"));
        assert_eq!(json["succeeded"], json!(0));
        assert_eq!(json["failed"], json!(0));
        assert!(
            json.get("applyId").is_none(),
            "applyId must be skipped when None"
        );
        assert!(
            json.get("sourceCommits").is_none(),
            "sourceCommits must be skipped when BTreeMap is empty"
        );
        assert!(
            json.get("backups").is_none(),
            "backups must be skipped when Vec is empty"
        );
    }

    #[test]
    fn apply_output_aborted_emits_aborted_status_and_zero_counts() {
        let v = ApplyOutput::aborted();
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("aborted"));
        assert_eq!(json["succeeded"], json!(0));
        assert_eq!(json["failed"], json!(0));
        assert!(json.get("applyId").is_none());
        assert!(json.get("sourceCommits").is_none());
        assert!(json.get("backups").is_none());
    }

    #[test]
    fn apply_output_with_backups_includes_backup_results() {
        let v = ApplyOutput {
            status: "partial".to_string(),
            apply_id: Some(7),
            succeeded: 2,
            skipped: 0,
            failed: 0,
            not_attempted: 0,
            source_commits: BTreeMap::new(),
            backups: vec![BackupRunOutput {
                name: "photos".to_string(),
                status: "success".to_string(),
                destination_path: Some("/backups/photos/20260801T000000Z".to_string()),
                clean: false,
                error: Some("postBackup hook failed".to_string()),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["backups"][0]["name"], json!("photos"));
        assert_eq!(json["backups"][0]["clean"], json!(false));
        assert_eq!(json["backups"][0]["error"], json!("postBackup hook failed"));
    }

    #[test]
    fn apply_output_populated_includes_apply_id_and_source_commits() {
        let mut commits = BTreeMap::new();
        commits.insert("origin".to_string(), "abc123".to_string());
        let v = ApplyOutput {
            status: "success".to_string(),
            apply_id: Some(99),
            succeeded: 3,
            skipped: 0,
            failed: 1,
            not_attempted: 0,
            source_commits: commits,
            backups: Vec::new(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("success"));
        assert_eq!(json["applyId"], json!(99));
        assert_eq!(json["succeeded"], json!(3));
        assert_eq!(json["failed"], json!(1));
        assert_eq!(json["sourceCommits"]["origin"], json!("abc123"));
    }

    #[test]
    fn apply_output_source_commits_serialize_in_key_order_every_run() {
        // A `HashMap` here would print `sourceCommits` keys in a
        // per-process-random order, byte-unstable for a docs capture, a
        // golden test, or a checksum-diffing `-o json` consumer.
        // `BTreeMap` fixes the order to key-sorted regardless of insertion
        // order or how many times this test (or the process) runs.
        let mut commits = BTreeMap::new();
        commits.insert("zeta".to_string(), "z-sha".to_string());
        commits.insert("alpha".to_string(), "a-sha".to_string());
        commits.insert("mid".to_string(), "m-sha".to_string());
        let v = ApplyOutput {
            status: "success".to_string(),
            apply_id: Some(1),
            succeeded: 1,
            skipped: 0,
            failed: 0,
            not_attempted: 0,
            source_commits: commits,
            backups: Vec::new(),
        };
        let first = serde_json::to_string(&v).unwrap();
        let second = serde_json::to_string(&v).unwrap();
        assert_eq!(first, second, "identical value must serialize identically");
        let alpha_pos = first.find("\"alpha\"").unwrap();
        let mid_pos = first.find("\"mid\"").unwrap();
        let zeta_pos = first.find("\"zeta\"").unwrap();
        assert!(
            alpha_pos < mid_pos && mid_pos < zeta_pos,
            "sourceCommits keys must serialize in sorted order: {first}"
        );
    }

    #[test]
    fn rollback_output_camelcases_all_fields_and_includes_action_list() {
        let v = RollbackOutput {
            apply_id: 12,
            files_restored: 4,
            files_removed: 2,
            non_file_actions: vec!["svc:restart".to_string()],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["applyId"], json!(12));
        assert_eq!(json["filesRestored"], json!(4));
        assert_eq!(json["filesRemoved"], json!(2));
        assert_eq!(json["nonFileActions"], json!(["svc:restart"]));
    }

    #[test]
    fn sync_output_pins_local_pulled_flag_and_sources_array() {
        let v = SyncOutput {
            local_pulled: true,
            local_pull_error: None,
            sources: vec![SourceSyncOutput {
                name: "main".to_string(),
                status: "synced".to_string(),
                commit: Some("c0ffee".to_string()),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["localPulled"], json!(true));
        let sources = json["sources"].as_array().expect("sources is array");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["name"], json!("main"));
        assert_eq!(sources[0]["status"], json!("synced"));
        assert_eq!(sources[0]["commit"], json!("c0ffee"));
    }

    #[test]
    fn pull_output_skips_error_when_none_and_keeps_status() {
        let v = PullOutput {
            status: "upToDate".to_string(),
            error: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("upToDate"));
        assert!(
            json.get("error").is_none(),
            "error must be skipped when None"
        );
    }

    #[test]
    fn pull_output_includes_error_when_some() {
        let v = PullOutput {
            status: "failed".to_string(),
            error: Some("network unreachable".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("failed"));
        assert_eq!(json["error"], json!("network unreachable"));
    }

    #[test]
    fn checkin_output_camelcases_all_fields() {
        let v = CheckinOutput {
            server_status: "ok".to_string(),
            config_changed: true,
            drift_count: 5,
            drift_status: "warning".to_string(),
            server_pushed_config: false,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["serverStatus"], json!("ok"));
        assert_eq!(json["configChanged"], json!(true));
        assert_eq!(json["driftCount"], json!(5));
        assert_eq!(json["driftStatus"], json!("warning"));
        assert_eq!(json["serverPushedConfig"], json!(false));
    }

    #[test]
    fn diff_output_nests_files_packages_system_and_summary() {
        let v = DiffOutput {
            files: vec![cfgd_core::providers::FileDriftResult {
                target: "~/.config/app/x.ini".to_string(),
                matches: false,
                expected: "content satisfies patch spec".to_string(),
                actual: "cannot evaluate patch spec: blocked".to_string(),
                unmanaged: false,
            }],
            packages: vec![PackageDrift {
                manager: "brew".to_string(),
                shape: "missing".to_string(),
                packages: vec!["ripgrep".to_string()],
                bootstrap_method: None,
                reason: None,
            }],
            system: vec![SystemDriftOutput {
                key: "sysctl.kernel.x".to_string(),
                expected: "1".to_string(),
                actual: "0".to_string(),
            }],
            system_errors: vec![SystemCheckError {
                key: "launchd".to_string(),
                error: "permission denied".to_string(),
            }],
            env: vec![EnvDriftOutput {
                kind: "alias".to_string(),
                name: "ll".to_string(),
                expected: r#"alias ll="ls -la""#.to_string(),
                actual: "missing or changed".to_string(),
            }],
            summary: DiffSummary {
                has_file_drift: true,
                has_pkg_drift: true,
                has_system_drift: true,
                system_check_failed: true,
                has_env_drift: true,
                env_check_failed: false,
            },
            env_check_error: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        let files = json["files"].as_array().expect("files is array");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["resourceType"], json!("file"));
        assert_eq!(files[0]["resourceId"], json!("~/.config/app/x.ini"));
        assert_eq!(files[0]["matches"], json!(false));
        assert_eq!(
            files[0]["actual"],
            json!("cannot evaluate patch spec: blocked"),
            "the reason a file could not be evaluated must survive to the structured path"
        );
        let pkgs = json["packages"].as_array().expect("packages is array");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0]["manager"], json!("brew"));
        assert_eq!(pkgs[0]["packages"], json!(["ripgrep"]));
        let sys = json["system"].as_array().expect("system is array");
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0]["key"], json!("sysctl.kernel.x"));
        assert_eq!(json["summary"]["hasFileDrift"], json!(true));
        assert_eq!(json["summary"]["hasPkgDrift"], json!(true));
        assert_eq!(json["summary"]["hasSystemDrift"], json!(true));
        let errs = json["systemErrors"]
            .as_array()
            .expect("systemErrors is array");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["key"], json!("launchd"));
        assert_eq!(errs[0]["error"], json!("permission denied"));
        assert_eq!(json["summary"]["systemCheckFailed"], json!(true));
        let env = json["env"].as_array().expect("env is array");
        assert_eq!(env.len(), 1);
        assert_eq!(env[0]["kind"], json!("alias"));
        assert_eq!(env[0]["name"], json!("ll"));
        assert_eq!(env[0]["expected"], json!(r#"alias ll="ls -la""#));
        assert_eq!(env[0]["actual"], json!("missing or changed"));
        assert_eq!(json["summary"]["hasEnvDrift"], json!(true));
    }

    #[test]
    fn diff_summary_camelcases_drift_flags() {
        let v = DiffSummary {
            has_file_drift: false,
            has_pkg_drift: true,
            has_system_drift: false,
            system_check_failed: false,
            has_env_drift: true,
            env_check_failed: true,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["hasFileDrift"], json!(false));
        assert_eq!(json["hasPkgDrift"], json!(true));
        assert_eq!(json["hasSystemDrift"], json!(false));
        assert_eq!(json["systemCheckFailed"], json!(false));
        assert_eq!(json["hasEnvDrift"], json!(true));
        assert_eq!(json["envCheckFailed"], json!(true));
    }

    #[test]
    fn diff_output_omits_system_errors_when_every_check_ran() {
        // The common shape: the key is absent rather than an empty array, so a
        // consumer's `if .systemErrors` reads false on a complete run.
        let json = serde_json::to_value(DiffOutput::default()).unwrap();
        assert!(
            json.get("systemErrors").is_none(),
            "a complete run carries no error list: {json}"
        );
        assert_eq!(json["summary"]["systemCheckFailed"], json!(false));
        assert!(
            json.get("envCheckError").is_none(),
            "a complete run carries no env check error: {json}"
        );
        assert_eq!(json["summary"]["envCheckFailed"], json!(false));
    }

    #[test]
    fn package_drift_skips_empty_packages() {
        let v = PackageDrift {
            manager: "apt".to_string(),
            shape: "extra".to_string(),
            packages: vec![],
            bootstrap_method: None,
            reason: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["manager"], json!("apt"));
        assert_eq!(json["shape"], json!("extra"));
        assert!(
            json.get("packages").is_none(),
            "packages must be skipped when Vec is empty"
        );
    }

    #[test]
    fn package_drift_emits_packages_when_populated() {
        let v = PackageDrift {
            manager: "cargo".to_string(),
            shape: "missing".to_string(),
            packages: vec!["bat".to_string(), "fd-find".to_string()],
            bootstrap_method: None,
            reason: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["packages"], json!(["bat", "fd-find"]));
    }

    #[test]
    fn system_drift_output_camelcases_fields() {
        let v = SystemDriftOutput {
            key: "kernel.parameter".to_string(),
            expected: "1024".to_string(),
            actual: "512".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["key"], json!("kernel.parameter"));
        assert_eq!(json["expected"], json!("1024"));
        assert_eq!(json["actual"], json!("512"));
    }

    #[test]
    fn source_sync_output_skips_none_commit() {
        let v = SourceSyncOutput {
            name: "infra".to_string(),
            status: "pending".to_string(),
            commit: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("infra"));
        assert_eq!(json["status"], json!("pending"));
        assert!(
            json.get("commit").is_none(),
            "commit must be skipped when None"
        );
    }

    #[test]
    fn plan_output_skips_empty_warnings_and_emits_phases() {
        let v = PlanOutput {
            context: "default".to_string(),
            phases: vec![PlanPhaseOutput {
                phase: "pre".to_string(),
                groups: vec![PlanGroupOutput::new(
                    cfgd_core::reconciler::Owner::profile("work"),
                    vec![PlanActionOutput {
                        description: "install pkg".to_string(),
                        action_type: "package".to_string(),
                        targets: vec![],
                        origin: None,
                        manager: None,
                        detail: None,
                    }],
                )],
            }],
            total_actions: 1,
            sources: vec![],
            warnings: vec![],
            pending_backups: vec![],
            pending_decisions: vec![],
            rejected_decisions: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["context"], json!("default"));
        assert_eq!(json["totalActions"], json!(1));
        assert!(
            json.get("warnings").is_none(),
            "warnings must be skipped when Vec is empty"
        );
        assert!(
            json.get("pendingBackups").is_none(),
            "pendingBackups must be skipped when Vec is empty"
        );
        let phases = json["phases"].as_array().expect("phases is array");
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0]["phase"], json!("pre"));
        let groups = phases[0]["groups"].as_array().expect("groups is array");
        let actions = groups[0]["actions"].as_array().expect("actions is array");
        assert_eq!(actions[0]["description"], json!("install pkg"));
    }

    #[test]
    fn plan_output_emits_warnings_when_populated() {
        let v = PlanOutput {
            context: "default".to_string(),
            phases: vec![],
            total_actions: 0,
            sources: vec![],
            warnings: vec!["missing tool".to_string()],
            pending_backups: vec![],
            pending_decisions: vec![],
            rejected_decisions: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["warnings"], json!(["missing tool"]));
    }

    #[test]
    fn plan_output_includes_pending_backups_when_present() {
        let v = PlanOutput {
            context: "default".to_string(),
            phases: vec![],
            total_actions: 0,
            sources: vec![],
            warnings: vec![],
            pending_backups: vec!["photos".to_string()],
            pending_decisions: vec![],
            rejected_decisions: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["pendingBackups"], json!(["photos"]));
    }

    #[test]
    fn plan_phase_output_emits_phase_name_and_owner_groups() {
        let v = PlanPhaseOutput {
            phase: "main".to_string(),
            groups: vec![PlanGroupOutput::new(
                cfgd_core::reconciler::Owner::module("nvim"),
                vec![PlanActionOutput {
                    description: "render file".to_string(),
                    action_type: "file".to_string(),
                    targets: vec!["/etc/hosts".to_string()],
                    origin: None,
                    manager: None,
                    detail: None,
                }],
            )],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["phase"], json!("main"));
        let groups = json["groups"].as_array().expect("groups is array");
        assert_eq!(groups.len(), 1);
        // The wire vocabulary of `owner`, pinned here so a rename inside the
        // reconciler's `Owner` cannot silently reshape the `-o json` payload.
        assert_eq!(
            groups[0]["owner"],
            json!({"kind": "module", "name": "nvim"})
        );
        assert_eq!(
            groups[0]["token"],
            json!("module:nvim"),
            "token is Owner::token()'s rendering, so a consumer never rebuilds the grammar"
        );
        let actions = groups[0]["actions"].as_array().expect("actions is array");
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn plan_action_output_renames_action_type_to_type() {
        let v = PlanActionOutput {
            description: "configure systemd".to_string(),
            action_type: "system".to_string(),
            targets: vec![],
            origin: None,
            manager: None,
            detail: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["description"], json!("configure systemd"));
        assert_eq!(
            json["type"],
            json!("system"),
            "action_type must rename to `type` on the wire"
        );
        assert!(
            json.get("targets").is_none(),
            "targets must be omitted from the wire when empty"
        );
        assert!(
            json.get("actionType").is_none(),
            "actionType camelCase must not appear; #[serde(rename)] takes precedence"
        );
    }

    #[test]
    fn plan_action_output_emits_targets_when_populated() {
        let v = PlanActionOutput {
            description: "create /etc/hosts".to_string(),
            action_type: "file.create".to_string(),
            targets: vec!["/etc/hosts".to_string()],
            origin: None,
            manager: None,
            detail: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["targets"],
            json!(["/etc/hosts"]),
            "targets must serialize as a string array when present"
        );
    }

    #[test]
    fn doctor_output_nests_all_subchecks() {
        let v = DoctorOutput {
            config: DoctorConfigCheck {
                valid: true,
                path: "/etc/cfgd.yaml".to_string(),
                name: Some("host".to_string()),
                profile: Some("default".to_string()),
                error: None,
                state: DoctorConfigState::Valid,
            },
            git: true,
            secrets: DoctorSecretsCheck {
                sops_available: true,
                sops_version: Some("3.8.0".to_string()),
                age_key_exists: true,
                age_key_path: Some("/home/u/.age".to_string()),
                sops_config_exists: false,
                sops_config_path: None,
                providers: vec![DoctorProviderCheck {
                    name: "sops".to_string(),
                    available: true,
                }],
            },
            package_managers: vec![DoctorManagerCheck {
                name: "brew".to_string(),
                available: true,
                declared: true,
                can_bootstrap: false,
                bootstrap_method: None,
            }],
            modules: vec![DoctorModuleCheck {
                name: "shell".to_string(),
                valid: true,
                error: None,
                packages: vec![],
            }],
            system_configurators: vec![DoctorConfiguratorCheck {
                name: "systemd".to_string(),
                available: true,
            }],
            profiles: vec![DoctorProfileLayoutCheck {
                name: "work".to_string(),
                legacy: true,
                path: Some("/etc/cfgd/profiles/work.yaml".to_string()),
                error: None,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["config"]["valid"], json!(true));
        assert_eq!(json["git"], json!(true));
        assert_eq!(json["secrets"]["sopsAvailable"], json!(true));
        assert_eq!(json["packageManagers"][0]["name"], json!("brew"));
        assert_eq!(json["modules"][0]["name"], json!("shell"));
        assert_eq!(json["systemConfigurators"][0]["name"], json!("systemd"));
        assert_eq!(json["profiles"][0]["name"], json!("work"));
        assert_eq!(json["profiles"][0]["legacy"], json!(true));
    }

    #[test]
    fn doctor_config_check_camelcases_fields_and_emits_nulls() {
        let v = DoctorConfigCheck {
            valid: false,
            path: "/x".to_string(),
            name: None,
            profile: None,
            error: Some("missing".to_string()),
            state: DoctorConfigState::Invalid,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["valid"], json!(false));
        assert_eq!(json["path"], json!("/x"));
        assert_eq!(json["name"], Value::Null);
        assert_eq!(json["profile"], Value::Null);
        assert_eq!(json["error"], json!("missing"));
    }

    #[test]
    fn doctor_secrets_check_skips_sops_config_path_when_none() {
        let v = DoctorSecretsCheck {
            sops_available: true,
            sops_version: Some("3.8.0".to_string()),
            age_key_exists: false,
            age_key_path: None,
            sops_config_exists: false,
            sops_config_path: None,
            providers: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["sopsAvailable"], json!(true));
        assert_eq!(json["sopsVersion"], json!("3.8.0"));
        assert_eq!(json["ageKeyExists"], json!(false));
        assert_eq!(json["ageKeyPath"], Value::Null);
        assert_eq!(json["sopsConfigExists"], json!(false));
        assert!(
            json.get("sopsConfigPath").is_none(),
            "sopsConfigPath must be skipped when None"
        );
        assert_eq!(json["providers"], json!([]));
    }

    #[test]
    fn doctor_secrets_check_includes_sops_config_path_when_some() {
        let v = DoctorSecretsCheck {
            sops_available: true,
            sops_version: None,
            age_key_exists: true,
            age_key_path: None,
            sops_config_exists: true,
            sops_config_path: Some("/etc/sops.yaml".to_string()),
            providers: vec![],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["sopsConfigPath"], json!("/etc/sops.yaml"));
    }

    #[test]
    fn doctor_provider_check_emits_name_and_available() {
        let v = DoctorProviderCheck {
            name: "vault".to_string(),
            available: false,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("vault"));
        assert_eq!(json["available"], json!(false));
    }

    #[test]
    fn doctor_manager_check_skips_bootstrap_method_when_none() {
        let v = DoctorManagerCheck {
            name: "apt".to_string(),
            available: true,
            declared: false,
            can_bootstrap: false,
            bootstrap_method: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("apt"));
        assert_eq!(json["available"], json!(true));
        assert_eq!(json["declared"], json!(false));
        assert_eq!(json["canBootstrap"], json!(false));
        assert!(
            json.get("bootstrapMethod").is_none(),
            "bootstrapMethod must be skipped when None"
        );
    }

    #[test]
    fn doctor_manager_check_includes_bootstrap_method_when_some() {
        let v = DoctorManagerCheck {
            name: "brew".to_string(),
            available: false,
            declared: true,
            can_bootstrap: true,
            bootstrap_method: Some("curl-installer".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["bootstrapMethod"], json!("curl-installer"));
    }

    #[test]
    fn doctor_module_check_emits_packages_array_and_null_error() {
        let v = DoctorModuleCheck {
            name: "git".to_string(),
            valid: true,
            error: None,
            packages: vec![DoctorModulePackageCheck {
                name: "git".to_string(),
                resolved_name: "git".to_string(),
                manager: "apt".to_string(),
                installed: true,
                version: Some("2.40.1".to_string()),
                skip_reason: None,
                error: None,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("git"));
        assert_eq!(json["valid"], json!(true));
        assert_eq!(json["error"], Value::Null);
        let pkgs = json["packages"].as_array().expect("packages is array");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0]["resolvedName"], json!("git"));
    }

    #[test]
    fn doctor_module_package_check_skips_all_none_optionals() {
        let v = DoctorModulePackageCheck {
            name: "ripgrep".to_string(),
            resolved_name: "ripgrep".to_string(),
            manager: "brew".to_string(),
            installed: false,
            version: None,
            skip_reason: None,
            error: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("ripgrep"));
        assert_eq!(json["resolvedName"], json!("ripgrep"));
        assert_eq!(json["manager"], json!("brew"));
        assert_eq!(json["installed"], json!(false));
        assert!(json.get("version").is_none(), "version must be skipped");
        assert!(
            json.get("skipReason").is_none(),
            "skipReason must be skipped"
        );
        assert!(json.get("error").is_none(), "error must be skipped");
    }

    #[test]
    fn doctor_module_package_check_includes_all_optionals_when_populated() {
        let v = DoctorModulePackageCheck {
            name: "bat".to_string(),
            resolved_name: "bat-cat".to_string(),
            manager: "cargo".to_string(),
            installed: false,
            version: Some("0.24.0".to_string()),
            skip_reason: Some("offline".to_string()),
            error: Some("network".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["version"], json!("0.24.0"));
        assert_eq!(json["skipReason"], json!("offline"));
        assert_eq!(json["error"], json!("network"));
    }

    #[test]
    fn doctor_configurator_check_emits_name_and_available() {
        let v = DoctorConfiguratorCheck {
            name: "launchd".to_string(),
            available: true,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("launchd"));
        assert_eq!(json["available"], json!(true));
    }

    #[test]
    fn source_list_entry_camelcases_last_fetched_and_emits_nulls() {
        let v = SourceListEntry {
            name: "main".to_string(),
            url: Some("https://example.com/repo.git".to_string()),
            priority: Some(100),
            version: None,
            status: "synced".to_string(),
            last_fetched: Some("2026-01-01T00:00:00Z".to_string()),
            signed: Some(true),
            require_signed_commits: Some(true),
            last_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            drift_count: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("main"));
        assert_eq!(json["url"], json!("https://example.com/repo.git"));
        assert_eq!(json["priority"], json!(100));
        assert_eq!(json["version"], Value::Null);
        assert_eq!(json["status"], json!("synced"));
        assert_eq!(json["lastFetched"], json!("2026-01-01T00:00:00Z"));
        assert_eq!(json["requireSignedCommits"], json!(true));
        assert_eq!(
            json["lastCommit"],
            json!("0123456789abcdef0123456789abcdef01234567"),
            "the payload keeps the full id; only the column shortens it"
        );
        assert_eq!(json["driftCount"], Value::Null);
    }

    #[test]
    fn source_show_output_camelcases_all_fields_and_nests_state() {
        let v = SourceShowOutput {
            name: "infra".to_string(),
            url: "https://example.com/r.git".to_string(),
            branch: "main".to_string(),
            priority: 50,
            accept_recommended: true,
            profile: Some("default".to_string()),
            sync_interval: "5m".to_string(),
            auto_apply: false,
            pin_version: Some("v1.2.3".to_string()),
            state: Some(SourceStateInfo {
                status: "fresh".to_string(),
                last_fetched: Some("2026-01-01T00:00:00Z".to_string()),
                last_commit: Some("abc".to_string()),
                signed: Some(true),
                version: Some("v1.2.3".to_string()),
                locked_ref: None,
                locked_commit: None,
            }),
            managed_resources: vec![SourceResourceEntry {
                resource_type: "Module".to_string(),
                resource_id: "shell".to_string(),
            }],
            modules: vec!["dev-tools".to_string()],
            policy: Some(SourcePolicyOutput {
                require_signed_commits: true,
                signed_commits_bypassed: true,
                scripts_allowed: false,
                secrets_read_allowed: false,
                system_changes_allowed: false,
                allowed_target_paths: vec!["~/.config/**".to_string()],
                encryption: Some(crate::cli::output_types::SourceEncryptionOutput {
                    required_targets: vec!["secrets/**".to_string()],
                    backend: Some("sops".to_string()),
                    mode: Some("Always".to_string()),
                }),
            }),
            manifest: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["modules"], json!(["dev-tools"]));
        assert_eq!(json["name"], json!("infra"));
        assert_eq!(json["url"], json!("https://example.com/r.git"));
        assert_eq!(json["branch"], json!("main"));
        assert_eq!(json["priority"], json!(50));
        assert_eq!(json["acceptRecommended"], json!(true));
        assert_eq!(json["profile"], json!("default"));
        assert_eq!(json["syncInterval"], json!("5m"));
        assert_eq!(json["autoApply"], json!(false));
        assert_eq!(json["pinVersion"], json!("v1.2.3"));
        assert_eq!(json["state"]["status"], json!("fresh"));
        assert_eq!(json["managedResources"][0]["resourceType"], json!("Module"));
        assert_eq!(json["policy"]["requireSignedCommits"], json!(true));
        assert_eq!(json["policy"]["signedCommitsBypassed"], json!(true));
        assert_eq!(json["policy"]["scriptsAllowed"], json!(false));
        assert_eq!(json["policy"]["secretsReadAllowed"], json!(false));
        assert_eq!(json["policy"]["systemChangesAllowed"], json!(false));
        assert_eq!(
            json["policy"]["allowedTargetPaths"],
            json!(["~/.config/**"])
        );
        assert_eq!(
            json["policy"]["encryption"]["requiredTargets"],
            json!(["secrets/**"])
        );
        assert_eq!(json["policy"]["encryption"]["backend"], json!("sops"));
        assert_eq!(json["policy"]["encryption"]["mode"], json!("Always"));
    }

    /// `signedCommitsBypassed` and `encryption` both take `skip_serializing_if`
    /// — omitted from the wire rather than serialized as `false`/`null` when
    /// there is nothing to report, matching the envelope discipline every
    /// other optional field on this struct already follows.
    #[test]
    fn source_policy_output_omits_bypass_and_encryption_when_absent() {
        let policy = SourcePolicyOutput {
            require_signed_commits: false,
            signed_commits_bypassed: false,
            scripts_allowed: true,
            secrets_read_allowed: true,
            system_changes_allowed: true,
            allowed_target_paths: Vec::new(),
            encryption: None,
        };
        let json = serde_json::to_value(&policy).unwrap();
        assert!(
            json.get("signedCommitsBypassed").is_none(),
            "false bypass must be omitted: {json}"
        );
        assert!(
            json.get("encryption").is_none(),
            "absent encryption constraint must be omitted: {json}"
        );
    }

    #[test]
    fn source_show_output_omits_policy_when_manifest_unavailable() {
        let v = SourceShowOutput {
            name: "infra".to_string(),
            url: "https://example.com/r.git".to_string(),
            branch: "main".to_string(),
            priority: 50,
            accept_recommended: false,
            profile: None,
            sync_interval: "5m".to_string(),
            auto_apply: false,
            pin_version: None,
            state: None,
            managed_resources: Vec::new(),
            modules: Vec::new(),
            policy: None,
            manifest: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert!(
            json.get("manifest").is_none(),
            "an unloadable manifest is omitted from the wire, never null: {json}"
        );
        assert!(
            json.get("policy").is_none(),
            "no manifest means no effective policy to report — the key must be \
             omitted, not serialized as null: {json}"
        );
    }

    #[test]
    fn source_state_info_emits_camelcase_keys() {
        let v = SourceStateInfo {
            status: "stale".to_string(),
            last_fetched: Some("2026-01-01T00:00:00Z".to_string()),
            last_commit: Some("c0ffee".to_string()),
            signed: None,
            version: Some("v0.1".to_string()),
            locked_ref: Some("v2.1.0".to_string()),
            locked_commit: Some("a".repeat(40)),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["status"], json!("stale"));
        assert_eq!(json["lastFetched"], json!("2026-01-01T00:00:00Z"));
        assert_eq!(json["lastCommit"], json!("c0ffee"));
        assert_eq!(json["version"], json!("v0.1"));
        assert_eq!(json["lockedRef"], json!("v2.1.0"));
        assert_eq!(json["lockedCommit"], json!("a".repeat(40)));
    }

    #[test]
    fn source_resource_entry_camelcases_resource_type_and_id() {
        let v = SourceResourceEntry {
            resource_type: "Profile".to_string(),
            resource_id: "dev".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["resourceType"], json!("Profile"));
        assert_eq!(json["resourceId"], json!("dev"));
    }

    #[test]
    fn profile_list_entry_skips_none_inherits_and_emits_module_count() {
        let v = ProfileListEntry {
            name: "default".to_string(),
            active: true,
            inherits: None,
            module_count: 3,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("default"));
        assert_eq!(json["active"], json!(true));
        assert_eq!(json["moduleCount"], json!(3));
        assert!(
            json.get("inherits").is_none(),
            "inherits must be skipped when None"
        );
    }

    #[test]
    fn profile_list_entry_includes_inherits_when_some() {
        let v = ProfileListEntry {
            name: "dev".to_string(),
            active: false,
            inherits: Some("base".to_string()),
            module_count: 5,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["inherits"], json!("base"));
    }

    #[test]
    fn module_search_result_skips_none_description_and_version() {
        let v = ModuleSearchResult {
            name: "shell".to_string(),
            registry: "official".to_string(),
            description: None,
            version: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("shell"));
        assert_eq!(json["registry"], json!("official"));
        assert!(json.get("description").is_none());
        assert!(json.get("version").is_none());
    }

    #[test]
    fn module_search_result_includes_description_and_version_when_some() {
        let v = ModuleSearchResult {
            name: "git".to_string(),
            registry: "official".to_string(),
            description: Some("Git tooling".to_string()),
            version: Some("1.0.0".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["description"], json!("Git tooling"));
        assert_eq!(json["version"], json!("1.0.0"));
    }

    #[test]
    fn registry_list_entry_emits_name_and_url() {
        let v = RegistryListEntry {
            name: "official".to_string(),
            url: "oci://registry.example.com".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("official"));
        assert_eq!(json["url"], json!("oci://registry.example.com"));
    }

    #[test]
    fn key_list_entry_skips_none_fingerprint_and_created() {
        let v = KeyListEntry {
            name: "signing".to_string(),
            fingerprint: None,
            created: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["name"], json!("signing"));
        assert!(json.get("fingerprint").is_none());
        assert!(json.get("created").is_none());
    }

    #[test]
    fn key_list_entry_includes_fingerprint_and_created_when_some() {
        let v = KeyListEntry {
            name: "signing".to_string(),
            fingerprint: Some("SHA256:abc".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["fingerprint"], json!("SHA256:abc"));
        assert_eq!(json["created"], json!("2026-01-01T00:00:00Z"));
    }

    fn sample_snapshot() -> ComplianceSnapshot {
        ComplianceSnapshot {
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            machine: MachineInfo {
                hostname: "host".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            profile: "default".to_string(),
            sources: vec!["main".to_string()],
            checks: vec![],
            summary: ComplianceSummary {
                compliant: 1,
                warning: 0,
                violation: 0,
            },
        }
    }

    #[test]
    fn compliance_snapshot_output_nests_snapshot_payload() {
        let v = ComplianceSnapshotOutput {
            snapshot: sample_snapshot(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["snapshot"]["profile"], json!("default"));
        assert_eq!(json["snapshot"]["summary"]["compliant"], json!(1));
    }

    #[test]
    fn compliance_history_output_emits_entries_array() {
        let v = ComplianceHistoryOutput {
            entries: vec![ComplianceHistoryRow {
                id: 1,
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                compliant: 10,
                warning: 2,
                violation: 1,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        let entries = json["entries"].as_array().expect("entries is array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!(1));
        assert_eq!(entries[0]["compliant"], json!(10));
    }

    #[test]
    fn compliance_diff_output_camelcases_id_fields_and_nests_changes() {
        let v = ComplianceDiffOutput {
            id1: 1,
            id2: 2,
            added: vec![ComplianceCheck {
                category: "pkg".to_string(),
                status: ComplianceStatus::Compliant,
                ..ComplianceCheck::default()
            }],
            removed: vec![],
            changed: vec![ComplianceCheckChange {
                key: "pkg/git".to_string(),
                old_status: "Compliant".to_string(),
                new_status: "Violation".to_string(),
                detail: Some("missing".to_string()),
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["id1"], json!(1));
        assert_eq!(json["id2"], json!(2));
        let added = json["added"].as_array().expect("added is array");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0]["category"], json!("pkg"));
        assert_eq!(json["removed"], json!([]));
        let changed = json["changed"].as_array().expect("changed is array");
        assert_eq!(changed[0]["key"], json!("pkg/git"));
        assert_eq!(changed[0]["oldStatus"], json!("Compliant"));
        assert_eq!(changed[0]["newStatus"], json!("Violation"));
        assert_eq!(changed[0]["detail"], json!("missing"));
    }

    #[test]
    fn compliance_check_change_skips_none_detail() {
        let v = ComplianceCheckChange {
            key: "pkg/x".to_string(),
            old_status: "Warning".to_string(),
            new_status: "Compliant".to_string(),
            detail: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["key"], json!("pkg/x"));
        assert_eq!(json["oldStatus"], json!("Warning"));
        assert_eq!(json["newStatus"], json!("Compliant"));
        assert!(
            json.get("detail").is_none(),
            "detail must be skipped when None"
        );
    }
}
