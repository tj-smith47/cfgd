use super::*;
use cfgd_core::config::LOCAL_LAYER;
use cfgd_core::output::{
    Doc, KvPair, Printer, Role, SectionBuilder, condense_script_label, renderer::Table,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusOutput {
    pub last_apply: Option<cfgd_core::state::ApplyRecord>,
    pub drift: Vec<cfgd_core::state::DriftEvent>,
    pub sources: Vec<cfgd_core::state::ConfigSourceRecord>,
    pub pending_decisions: Vec<cfgd_core::state::PendingDecision>,
    pub modules: Vec<ModuleStatusEntry>,
    pub managed_resources: Vec<cfgd_core::state::ManagedResource>,
    /// Source batches no decision row can name (a dotted custom manager) —
    /// withheld from every plan fail-closed, so the dashboard names them here
    /// instead of showing clean-empty. Same lines the `plan` payload's
    /// `warnings` carries; absent when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// True when the source-decision classification failed and
    /// `pendingDecisions` is missing the classified-but-unrecorded items — a
    /// degraded listing is otherwise indistinguishable from a clean empty one
    /// to a `-o json` consumer.
    pub classification_degraded: bool,
    /// The machine-stable cause class, present only when degraded — the
    /// reason string beside it is the human detail and carries no stability
    /// promise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification_degraded_code: Option<super::output_types::ClassificationDegradedCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification_degraded_reason: Option<String>,
    /// Whether `drift` is the verdict of a LIVE scan of this machine or the
    /// events something previously recorded. Plain `status` is the fast
    /// recorded-drift dashboard, so on a host with no daemon its `drift` is
    /// empty however far the machine has drifted; only `--scan` (and
    /// `--exit-code`, which implies it) scans.
    /// A consumer differencing an empty list needs to know which of those two
    /// it is holding, and the human line says the same thing in words.
    pub drift_checked_live: bool,
    /// When this machine was last scanned for live drift (`--scan`,
    /// `--exit-code`, `diff`, `verify`, or a daemon reconcile tick) — `None`
    /// when it never has been.
    ///
    /// A scanning run reports its OWN scan here, so the field always describes
    /// the `drift` array beside it. The recorded-state header's age line is
    /// computed from the value read BEFORE any scan, because that line exists
    /// to date state the run did NOT check — and it renders only on the
    /// non-scanning branch, where the two values are the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_at: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatusEntry {
    pub name: String,
    /// What this host manages for the module — the counts its Managed
    /// Resources rows add up to, from `recorded_module_tallies`, never a
    /// second count taken off the resolved declaration.
    pub packages: usize,
    pub files: usize,
    /// How many scripts the module's recorded `script` row stands for — the
    /// number that row's own cell prints, and 0 for a module with no such row.
    pub scripts: usize,
    pub status: String,
    /// Why this host resolves the module to nothing — the reason the header's
    /// `Modules` row prints in its skipped annotation. Absent for a module
    /// that applies here. Without it a gated module reached `-o json` as an
    /// ordinary entry reading `not applied`, and a consumer could not
    /// reconstruct what the human dashboard states.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_skip_reason: Option<String>,
    /// What resolution can still say about the module's recorded Managed
    /// Resources rows. Display-only — the recorded row is the fact a consumer
    /// reads out of `managedResources`, so the payload shape is the same with
    /// or without it.
    #[serde(skip)]
    pub declared: ModuleDeclared,
}

/// The detail the Managed Resources table renders beside one module's recorded
/// rows: the id records WHAT was applied, and this is what the current
/// resolution can add about it (which directory the files land in, which
/// manager installs a package, which hooks the module declares).
///
/// Empty for a module the config no longer carries — the row still names what
/// cfgd manages, with only the recorded id behind it.
#[derive(Debug, Clone, Default)]
pub struct ModuleDeclared {
    /// Directory the module's declared file targets share, POSIX-folded.
    pub file_root: Option<String>,
    /// Resolved package name to every manager the module declares it under.
    ///
    /// A SET per name, because one name can be declared twice: `nvim` resolves
    /// `neovim` under the host's native manager AND under `npm`. Keyed
    /// name-to-one-manager, the second declaration overwrote the first, and the
    /// native row — whose names then disagreed about who installs them — lost
    /// its manager prefix entirely and rendered a bare package list beside rows
    /// spelled `apt:`, `npm:`, `pipx:`.
    pub package_managers: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    /// `3 preApply, 6 postApply`, from [`cfgd_core::modules::ModuleSurfaces`] —
    /// the same tally, and the same rendering, `cfgd status <module>` reports.
    pub script_summary: Option<String>,
    /// The total [`Self::script_summary`] breaks down, from the same
    /// `ModuleSurfaces` — the headline slot's number, so the summary line and
    /// the row it summarizes cannot count one module's hooks twice.
    pub scripts: usize,
}

impl ModuleDeclared {
    fn of(module: &cfgd_core::modules::ResolvedModule) -> Self {
        let surfaces = cfgd_core::modules::ModuleSurfaces::of_resolved(module);
        Self {
            file_root: common_target_root(&module.files),
            package_managers: module.packages.iter().fold(
                std::collections::BTreeMap::new(),
                |mut map, p| {
                    map.entry(p.resolved_name.clone())
                        .or_default()
                        .insert(p.manager.clone());
                    map
                },
            ),
            script_summary: surfaces.script_summary(),
            scripts: surfaces.script_total(),
        }
    }
}

/// The deepest directory every one of `files` deploys under, POSIX-folded.
///
/// A module deploying one file answers with that file: it IS the whole of what
/// the module put on the machine, and naming its parent would claim a
/// directory cfgd does not manage.
///
/// Breadth is the accepted degrade: a module scattering dotfiles directly under
/// `$HOME` answers with the home directory, which is true but says little. What
/// is NOT accepted is answering with the filesystem root — targets sharing no
/// directory at all have no common root worth naming, and the row falls back to
/// the count alone.
fn common_target_root(files: &[cfgd_core::modules::ResolvedFile]) -> Option<String> {
    let mut targets = files.iter().map(|f| f.target.as_path());
    let mut root: Vec<std::path::Component<'_>> = targets.next()?.components().collect();
    for target in targets {
        let shared = root
            .iter()
            .zip(target.components())
            .take_while(|(a, b)| **a == *b)
            .count();
        root.truncate(shared);
    }
    // `RootDir` (and a Windows `Prefix`) survives truncation, so an emptiness
    // test would let `/` and `C:\` through as roots. The question is whether a
    // DIRECTORY below the filesystem root is still shared.
    if !root
        .iter()
        .any(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(cfgd_core::to_posix_string(
        root.iter().collect::<std::path::PathBuf>(),
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatus {
    pub name: String,
    pub packages: usize,
    pub files: usize,
    /// Counts of the declared surfaces apply runs a phase for but that carry
    /// no per-item recorded state: a phase that ran with nothing to say about
    /// it in status is a phase the reader watched happen and then could not
    /// find. `cfgd module show` itemizes what these summarize.
    ///
    /// Aliases precede env vars here as they do on every surface naming the
    /// pair, so the wire order and the rendered order read alike.
    pub aliases: usize,
    pub env: usize,
    /// Lifecycle hooks the module declares, by name (`preApply`, `onDrift`, …).
    pub scripts: Vec<String>,
    /// The declared surfaces this report renders from: the counts above come
    /// from it, and the wide inventories list its items. Display-only — every
    /// fact it carries that a consumer needs is already a field of its own
    /// beside it, so the payload shape is the same with or without it.
    #[serde(skip)]
    pub declared: cfgd_core::modules::ModuleSurfaces,
    /// System configurators the module contributes settings to, by name.
    pub system: Vec<String>,
    pub depends: Vec<String>,
    pub status: String,
    pub last_applied: Option<String>,
    /// What the run that last applied this module was scoped to, when that was
    /// an isolated `--module` run (`module:nvim`). A profile-wide run leaves it
    /// empty: the profile applied every module it carries, and naming it here
    /// would answer a question about the machine on a report about one module.
    ///
    /// Omitted rather than serialized as `null`, so a payload that has no scope
    /// to state is byte-identical to the one this field was added to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// One row per DECLARED package, carrying what the machine holds — the
    /// state half of the count above. Every row reads `notScanned` unless
    /// `--scan` asked a manager.
    pub package_state: Vec<ModulePackageStatus>,
    /// One row per file this module has deployed, carrying the same verdict
    /// the drift scan reached. Never a bare presence check: a drifted file is
    /// present, and reporting presence as health is the contradiction this
    /// field exists to make unrepresentable.
    pub deployed_files: Vec<ModuleFileStatus>,
    /// Live drift found for this module's files and packages. Always empty
    /// unless `--scan` (or `--exit-code`, which implies it) requested the live
    /// scan — see `drift_checked_live`.
    pub drift: Vec<ModuleDrift>,
    /// Whether `drift` is the verdict of a live scan of this module or just
    /// an unchecked empty default. Mirrors `StatusOutput::drift_checked_live`
    /// so the two `-o json` shapes read the same way.
    pub drift_checked_live: bool,
}

impl ModuleStatus {
    /// The ONE derivation of this module's verdict: the word the human `Status`
    /// row renders and the word the payload's `state` field carries come from
    /// this single call, so a reader and a machine consumer can never be shown
    /// two different answers about one module.
    ///
    /// `Drifted` is derived, never stored: it is read off the very scan whose
    /// findings fill the sections below, so the two can never disagree. Without
    /// a live scan there is no verdict to derive one from.
    fn state_display(&self) -> (&'static str, Role) {
        let drifted = self.drift_checked_live && !self.drift.is_empty();
        cfgd_core::state::module_status_display(&self.status, drifted)
    }
}

/// The `-o json` body of a module status report: every field of
/// [`ModuleStatus`] plus the derived verdict the human `Status` row shows.
///
/// `state` is composed HERE rather than stored on [`ModuleStatus`], because a
/// stored copy is a second thing to keep in step with the row it must agree
/// with — both halves read one [`ModuleStatus::state_display`] call instead.
/// `status` beside it stays the untouched stored token.
/// `scriptCounts` is derived the same way: `scripts` beside it keeps the hook
/// NAMES it always carried, and the per-hook tally is additive rather than a
/// reshaping of that field.
#[derive(Serialize)]
struct ModuleStatusPayload<'a> {
    #[serde(flatten)]
    module: &'a ModuleStatus,
    state: &'static str,
    /// One entry per declaring hook, in EXECUTION order — an array of
    /// `{hook, count}` rather than an object, because the payload is rebuilt
    /// through `serde_json::Value` on its way out and a JSON object there is a
    /// sorted map: as an object the pairs came back alphabetically, which is
    /// not the order the hooks run in.
    #[serde(rename = "scriptCounts")]
    script_counts: Vec<HookCount>,
}

#[derive(Serialize)]
struct HookCount {
    hook: String,
    count: usize,
}

/// One live-scan finding, carrying both the recorded-shape event a consumer
/// reads and the three display slots a drift row renders from.
///
/// The two halves live on ONE value rather than in two parallel lists so a row
/// can never name an owner, a surface or an item that belongs to a different
/// finding. The event is flattened, so `-o json` sees exactly the
/// [`cfgd_core::state::DriftEvent`] object it always did.
#[derive(Serialize)]
pub struct ModuleDrift {
    #[serde(flatten)]
    pub event: cfgd_core::state::DriftEvent,
    /// The module the finding belongs to — the asked module, or one of the
    /// dependencies its resolution pulled in.
    #[serde(skip)]
    pub owner: String,
    /// Which declared surface it was found on: [`SURFACE_FILES`] or
    /// [`SURFACE_PACKAGES`].
    #[serde(skip)]
    pub surface: &'static str,
    /// The thing itself — a deployed path, or the package name(s).
    #[serde(skip)]
    pub item: String,
}

/// The two surfaces a per-module live scan looks at, spelled as the module
/// document's own `spec` keys so a drift row names the block the reader edits.
pub const SURFACE_FILES: &str = "files";
pub const SURFACE_PACKAGES: &str = "packages";

/// What a manager reports about one declared package.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ModulePackagePresence {
    Installed,
    NotInstalled,
    /// Nothing asked: no `--scan`, a `script` package with no manager to ask,
    /// or a manager this host does not have registered.
    NotScanned,
    /// The module's own `platforms` gate rules this package out on this host,
    /// so nothing was ever going to install it. Distinct from `NotScanned`,
    /// which says nobody looked: here the answer is known and `cfgd module
    /// show` renders the same words for the same package.
    PlatformSkipped,
}

impl ModulePackagePresence {
    fn role(self) -> Role {
        match self {
            Self::Installed => Role::Ok,
            Self::NotInstalled => Role::Warn,
            Self::NotScanned | Self::PlatformSkipped => Role::Info,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::NotInstalled => cfgd_core::Absence::NotInstalled.as_str(),
            Self::NotScanned => NOT_SCANNED,
            Self::PlatformSkipped => PLATFORM_SKIPPED,
        }
    }
}

/// What this run can say about one file the module deployed.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ModuleFilePresence {
    Deployed,
    /// Present, and its content is not what the module declares — the same
    /// verdict the Drift section reports for it, so the two can never disagree.
    Drifted,
    Missing,
    /// Present on disk, content unchecked (no `--scan`). Presence alone is not
    /// health.
    NotScanned,
}

impl ModuleFilePresence {
    fn role(self) -> Role {
        match self {
            Self::Deployed => Role::Ok,
            Self::Drifted => Role::Warn,
            Self::Missing => Role::Fail,
            Self::NotScanned => Role::Info,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Deployed => "deployed",
            Self::Drifted => "drifted",
            Self::Missing => cfgd_core::Absence::Missing.as_str(),
            Self::NotScanned => NOT_SCANNED,
        }
    }
}

/// The one spelling of "cfgd did not ask", shared by both state vocabularies
/// so a reader meets one phrase per report rather than one per section.
const NOT_SCANNED: &str = "not scanned";

/// The wording `cfgd module show` renders for a platform-gated package
/// (`module/list_show.rs`); the two surfaces answer about one declared package
/// and must say the same thing.
pub(in crate::cli) const PLATFORM_SKIPPED: &str = "skipped (platform filter)";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModulePackageStatus {
    pub name: String,
    /// The manager that answered. `None` when nothing asked, so the row can
    /// never name a manager as the authority for a verdict it did not give.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager: Option<String>,
    pub state: ModulePackagePresence,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleFileStatus {
    pub path: String,
    pub state: ModuleFilePresence,
}

/// The "Drift" section's frame, shared by the fleet-wide and per-module status
/// docs: both state the same thing about an empty scan, and only the rows
/// inside differ. The empty branch lives here so the one distinction that
/// matters — a live scan may report a detection, a recorded dashboard may only
/// report what it holds — cannot be made twice and answered differently.
///
/// `verified` is whether ANY check stands behind an empty section: this run
/// scanned, or a scan is on record. Without one the verdict is a report of
/// absence, and it takes `Role::Info` — the role its sibling `No applies
/// recorded yet` already takes on the same screen — never `Role::Ok`, which
/// `diff` refuses for a check that could not run (`Drift undetermined`) and
/// which here painted a green tick over `never scanned`.
fn drift_section<T>(
    doc: Doc,
    drift: &[T],
    checked_live: bool,
    verified: bool,
    scan_note: Option<&str>,
    row: impl Fn(SectionBuilder, &T) -> SectionBuilder,
) -> Doc {
    doc.section("Drift", |s| {
        if drift.is_empty() {
            let role = if verified { Role::Ok } else { Role::Info };
            // Only the live scan may claim a detection. The recorded dashboard
            // has asked nothing of the machine, and "No drift detected" over a
            // host whose last apply left a declared package uninstalled is an
            // assurance no query backs.
            // No hint on this line: the report closes with ONE, and the two
            // together put two spellings of "go check the machine" on a single
            // screen — this one and the header's `--scan` line.
            let subject = if checked_live {
                "No drift detected"
            } else {
                "No drift recorded"
            };
            // When the recording was taken qualifies what "no drift" is worth,
            // so it rides the verdict rather than sitting in a header row the
            // reader has to carry down the page.
            s.status_with(role, subject, |f| match scan_note {
                Some(note) => f.detail(note),
                None => f,
            })
        } else {
            // The same fact, one line above the findings: with rows to state,
            // the verdict line the detail would ride does not exist. It is a
            // label for the rows beneath it rather than a finding of its own,
            // so it takes the iconless role — a glyph here would read as an
            // eighth drift row.
            let s = match scan_note {
                Some(note) => s.status(Role::Secondary, note),
                None => s,
            };
            drift.iter().fold(s, &row)
        }
    })
}

/// How a recorded-state dashboard dates its drift: the scan's age, or that no
/// scan has ever run. `None` is the LIVE branch — a `--scan` run just asked the
/// machine, so its findings need no timestamp — and every surface holding no
/// scan stamp at all.
fn scan_note(last_scan_at: Option<&str>, now: &str) -> String {
    match last_scan_at {
        Some(ts) => {
            let age = cfgd_core::humanize_age_since(ts, now).unwrap_or_else(|| ts.to_string());
            format!("scanned {age}")
        }
        None => "never scanned".to_string(),
    }
}

/// Render the fleet-wide "Drift" section: one row per recorded event, named by
/// the resource type and id the event was stored under.
fn render_drift_section(
    doc: Doc,
    drift: &[cfgd_core::state::DriftEvent],
    checked_live: bool,
    verified: bool,
    scan_note: Option<&str>,
) -> Doc {
    let drop_env_file_row = cfgd_core::output::env_file_row_is_redundant(
        drift.iter().map(|e| e.resource_type.as_str()),
    );
    let rows: Vec<&cfgd_core::state::DriftEvent> = drift
        .iter()
        .filter(|e| !(drop_env_file_row && e.resource_type == "env"))
        .collect();
    drift_section(doc, &rows, checked_live, verified, scan_note, |s, event| {
        // A "script" / "Running script" resource_id is the raw run_str body
        // (preserved byte-identical for UPSERT matching against prior drift
        // rows) — condense only here, at the point it enters a status subject,
        // so a multi-line inline script never lands raw. Two type strings exist
        // because two producers persist script actions: `apply_script_action`
        // (main pre/post-apply phase scripts, format.rs's
        // `format_action_description`) stamps "script"; `execute_script`
        // (onChange / module-onChange scripts, reconciler/scripts.rs) stamps
        // "Running script: {body}" — both must condense here.
        // Folded to `~/` like every other display slot of the report; the
        // recorded id and the `-o json` payload keep the absolute path.
        let display_id =
            if event.resource_type == "script" || event.resource_type == "Running script" {
                condense_script_label(&event.resource_id)
            } else {
                cfgd_core::fold_home_in_text(&event.resource_id)
            };
        let subject = cfgd_core::output::drift_item_subject(&event.resource_type, &display_id);
        // The recomputed pair when a surface could read one off the machine,
        // the stored pair otherwise — the human row states today's truth while
        // the payload keeps the bytes the row was stored with.
        let (expected, actual) = cfgd_core::output::drift_operands(
            &event.resource_type,
            event
                .want
                .as_deref()
                .or(event.expected.as_deref())
                .unwrap_or("?"),
            event
                .have
                .as_deref()
                .or(event.actual.as_deref())
                .unwrap_or("?"),
        );
        if event.source != LOCAL_LAYER {
            // Source attribution renders in `secondary` (pink/magenta) at
            // end-of-subject; the StatusBuilder API guarantees the label lands
            // last so the inner SGR reset is never followed by outer-role-styled
            // text. The token is the vocabulary `cfgd sync` and `cfgd source *`
            // head their groups with, so a reader carries one spelling across
            // the three surfaces that name a source.
            let label_text = cfgd_core::reconciler::Owner::source(&event.source).token();
            s.status_with(Role::Warn, subject, |f| {
                f.drift(expected, actual).label(Role::Secondary, label_text)
            })
        } else {
            s.status_with(Role::Warn, subject, |f| f.drift(expected, actual))
        }
    })
}

/// The order a drift row's surface sorts in: the `spec` blocks in the order
/// [`SURFACE_ORDER`] lists them, then anything else, alphabetically.
fn surface_rank(surface: &str) -> usize {
    SURFACE_ORDER
        .iter()
        .position(|s| *s == surface)
        .unwrap_or(SURFACE_ORDER.len())
}

/// The surface grouping of the per-module Drift section, in render order.
const SURFACE_ORDER: [&str; 2] = [SURFACE_FILES, SURFACE_PACKAGES];

/// Render the per-module "Drift" section: one row per finding, named by the
/// owner and surface it was found on rather than by the id it is stored under
/// (`module:nvim:files /home/u/.zshrc — content differs`).
///
/// Rows are grouped by surface and alphabetical by item within each group,
/// stated here rather than inherited from scan order: a scan visits files and
/// packages in whatever order resolution reached them, so an unsorted section
/// re-orders itself between two runs that found the same drift.
fn render_module_drift_section(doc: Doc, drift: &[ModuleDrift], checked_live: bool) -> Doc {
    let mut ordered: Vec<&ModuleDrift> = drift.iter().collect();
    ordered.sort_by(|a, b| {
        surface_rank(a.surface)
            .cmp(&surface_rank(b.surface))
            .then_with(|| a.surface.cmp(b.surface))
            .then_with(|| a.item.cmp(&b.item))
    });
    // No scan stamp reaches this surface, so the only check that can stand
    // behind an empty section is the one this run made.
    drift_section(doc, &ordered, checked_live, checked_live, None, |s, d| {
        let subject = format!(
            "{}:{} {}",
            cfgd_core::reconciler::Owner::module(&d.owner).token(),
            d.surface,
            cfgd_core::fold_home_in_text(&d.item)
        );
        let cause = cfgd_core::output::drift_terse_cause(
            &d.event.resource_type,
            d.event.expected.as_deref().unwrap_or_default(),
            d.event.actual.as_deref().unwrap_or_default(),
        );
        s.status_with(Role::Warn, subject, |f| f.detail(cause))
    })
}

/// The profile name a `Profile` row may state, or `None` when there is none to
/// state.
///
/// A module-scoped run resolves no profile and stamps the placeholder its
/// callers fall back to (`active_profile_name`), which then reaches the
/// recorded apply and the dashboard reading it. A row saying the profile is
/// `unknown` tells a reader that cfgd lost track of something, when the truth
/// is that the run was never scoped to a profile at all — so the row is left
/// out instead of naming a profile nothing has. The placeholder is no longer
/// RECORDED (an isolated run records its modules, and an underivable profile
/// records nothing), and is still refused here because a store written by an
/// older cfgd still holds rows carrying it.
pub(super) fn derivable_profile(name: &str) -> Option<&str> {
    match name.trim() {
        "" | "unknown" => None,
        name => Some(name),
    }
}

/// The row a recorded apply's scope renders as: `Profile base` for a
/// profile-scoped run, `Scope module:nvim` for an isolated one, nothing when
/// the run named neither.
///
/// The key is decided by the value because the two are the same column: a
/// module-scoped run has no profile, and labelling `module:nvim` as a profile
/// would name a profile that does not exist.
pub(super) fn recorded_scope_row(recorded: &str) -> Option<(&'static str, &str)> {
    let value = derivable_profile(recorded)?;
    let owner_prefix = format!("{}:", cfgd_core::reconciler::OwnerKind::Module.as_str());
    Some(if value.starts_with(&owner_prefix) {
        ("Scope", value)
    } else {
        // header-row-ok: what the RECORDED run was scoped to, not the profile
        // this report is running under
        ("Profile", value)
    })
}

/// The `Scope` row of a per-module report: the recorded owner tokens painted
/// through [`cfgd_core::reconciler::Owner::label`], the tri-colour `kind:name`
/// form the apply tree's group headings and the Managed Resources Owner column
/// already render — one owner spelled one way, whichever surface names it.
///
/// The recorded column holds one token or the `, `-joined list an isolated run
/// over several modules writes. A token that does not read back as an owner
/// keeps the recorded string, so nothing a run recorded is dropped from the
/// row for being unparseable.
fn scope_row(recorded: &str) -> KvPair {
    let owners: Option<Vec<cfgd_core::output::OwnerLabel>> = recorded
        .split(cfgd_core::reconciler::Owner::TOKEN_SEPARATOR)
        .map(|token| owner_from_token(token).map(|owner| owner.label()))
        .collect();
    match owners {
        Some(owners) => KvPair::owner_valued("Scope", owners),
        None => KvPair::new("Scope", recorded),
    }
}

/// The recorded-state header's staleness threshold: a daemon's default
/// reconcile interval. Past this age, the recorded drift a plain `cfgd status`
/// shows could easily be older than a live daemon would ever let it get, so
/// the header hints at `--scan` instead of leaving the reader to guess.
const SCAN_STALENESS_SECS: i64 = cfgd_core::daemon::DEFAULT_RECONCILE_SECS as i64;

/// Build the fleet-wide `cfgd status` Doc. Caller supplies the precomputed
/// payload, the four header facts every surface reporting on a resolved
/// configuration states ([`cfgd_core::output::ConfigHeader`], whose `profile`
/// the caller has already put through `derivable_profile`) and the declared
/// source catalog, which carries the columns the status payload does not
/// (priority, origin, signing demand).
pub fn build_fleet_status_doc(
    output: &StatusOutput,
    head: &cfgd_core::output::ConfigHeader<'_>,
    configured_sources: &[SourceListEntry],
    now: &str,
    decision_contents: &super::DecisionContents,
) -> Doc {
    // One derivation for the whole document: the header's `Profile` row and
    // the Managed Resources Owner column name the same profile or neither does.
    let profile = head.profile;
    let mut doc = Doc::new()
        .heading("Status")
        .kv_rows(cfgd_core::output::config_header_rows(head));

    // Only the recorded-state dashboard needs a staleness signal: a `--scan`/
    // `--exit-code` run just checked the machine itself, so its Drift section
    // already speaks for how current the display is.
    //
    // The date rides the Drift verdict it qualifies, and the hint it earns is
    // emitted ONCE at the foot of the report: a header row, a line inside
    // Drift and a closing hint were three spellings of one suggestion.
    let (stale, note) = if output.drift_checked_live {
        (false, None)
    } else {
        let stale = output
            .last_scan_at
            .as_deref()
            .is_none_or(|ts| cfgd_core::is_stale_since(ts, now, SCAN_STALENESS_SECS));
        (stale, Some(scan_note(output.last_scan_at.as_deref(), now)))
    };

    match &output.last_apply {
        Some(last) => {
            doc = doc.section("Last Apply", |s| {
                // The verdict leads: a reader scanning the dashboard needs to
                // know whether the apply succeeded before they need to know
                // what it was scoped to or how stale it is.
                let mut s = s.kv("Result", last.status.human_str());
                // decision-summary-ok: the `applies` record's own summary column, not a pending decision's
                if let Some(summary) = &last.summary {
                    // Prose, never the stored column: the wire shape is what
                    // `-o json` carries, and a human row reading
                    // `{"failed":0,"succeeded":22,"total":22}` makes the reader
                    // parse JSON to learn the apply went fine.
                    s = s.kv("Summary", cfgd_core::state::ApplySummary::prose(summary));
                }
                if let Some((key, value)) = recorded_scope_row(&last.profile) {
                    s = s.kv(key, value);
                }
                // `Age`, not the stored instant: `-o json`'s `lastApply.timestamp`
                // is where an exact moment is read from, and the dashboard row
                // is answering how stale the machine's last apply is.
                s.kv(
                    "Age",
                    cfgd_core::humanize_age_magnitude_cell(Some(&last.timestamp), now),
                )
            });
        }
        None => {
            doc = doc.status(Role::Info, "No applies recorded yet");
        }
    }

    doc = render_drift_section(
        doc,
        &output.drift,
        output.drift_checked_live,
        output.drift_checked_live || output.last_scan_at.is_some(),
        note.as_deref(),
    );

    if !configured_sources.is_empty() {
        doc = doc.section(super::source::list::SOURCES_SECTION, |s| {
            s.table(super::source::list::sources_table(
                configured_sources,
                false,
                now,
            ))
        });
    }

    doc = doc.section_if_nonempty(
        cfgd_core::reconciler::pending_decisions_title(
            output.pending_decisions.len(),
            cfgd_core::reconciler::DecisionsTitleScope::Listing,
        ),
        &output.pending_decisions,
        |s, rows| super::build_pending_decisions_table_section(s, rows, decision_contents),
    );
    // Rendered beside the pending rows those batches would otherwise be:
    // "why isn't requests installed?" must be answerable from the dashboard,
    // not only from a plan/apply run header.
    doc = output
        .warnings
        .iter()
        .fold(doc, |d, w| d.status(Role::Warn, w));

    doc = doc.section_if_nonempty("Modules", &output.modules, |s, mods| {
        mods.iter().fold(s, |s, m| {
            let summary = format!(
                "{}, {}, {}",
                cfgd_core::pluralize(m.packages, "package"),
                cfgd_core::pluralize(m.files, "file"),
                cfgd_core::pluralize(m.scripts, "script")
            );
            // The dashboard reads RECORDED state only, so no row here can
            // claim `Drifted` — this surface's Drift section is what reports
            // that, and `cfgd status --module --scan` is what derives it.
            let (state_word, role) = cfgd_core::state::module_status_display(&m.status, false);
            // Subject is the owner token, exactly as the tree that applied the
            // module heads its group; the counts and the state are what the
            // line reports about it.
            //
            // The verdict LEADS and the inventory is parenthesised under it:
            // comma-joined onto the end, `Synced` read as a fourth inventory
            // item, and `Failed` — the one word the reader is scanning for —
            // landed last and least prominent behind three counts.
            s.status_with(
                role,
                cfgd_core::reconciler::Owner::module(&m.name).token(),
                |f| f.detail(format!("{state_word} ({summary})")),
            )
        })
    });

    doc = doc.section_if_nonempty(
        "Managed Resources",
        &output.managed_resources,
        |s, items| {
            let mut t = Table::new(["Type", "Owner", "Resource", "Source"])
                // A package list is what the reader acts on, so a narrow
                // terminal wraps it rather than cutting names off the tail.
                .wrapping();
            for row in managed_resource_rows(items, &output.modules, profile) {
                t = t.row(row);
            }
            s.table(t.without_unfillable_columns())
        },
    );

    // A report that FOUND drift owes the reader the command that heals it, and
    // that outranks the invitation to look again: the looking has been done.
    if !output.drift.is_empty() && output.drift_checked_live {
        doc = doc.hint(super::heal_drift_hint(None));
    } else if stale {
        doc = doc.hint(SCAN_HINT);
    }

    doc.with_data(output)
}

/// The ONE line a recorded-state report closes with when its drift is
/// unchecked or stale: the one command that looks, at the foot of the report.
pub(super) const SCAN_HINT: &str = "`cfgd diff` checks the live machine for drift";

use cfgd_core::reconciler::ENV_RESOURCE_TYPE;

/// Stand-in for a resource column with nothing left to say — the same `-` the
/// Config Sources table renders for a version nobody has fetched.
const NO_DETAIL: &str = cfgd_core::ABSENT;

/// The Managed Resources rows, as `[Type, Owner, Resource, Source]`.
///
/// A recorded row is a state-matching key rather than a report: a `module`
/// row's id carries the owner and the surface inside it, and a `package` row
/// is ONE package where a reader wants the list a manager installed. Both are
/// split out here, so the table can say whose each resource is and can render
/// one row per manager rather than one per package.
///
/// `profile` is what [`derivable_profile`] answered for the resolved name,
/// which the recorded row does not carry: the Owner column reads the same
/// vocabulary the reconciler assigns the very actions that wrote these rows,
/// so a package the profile declared reads
/// [`cfgd_core::reconciler::Owner::profile`]'s token here exactly as the plan
/// and apply trees head its group and as `diff` reports its drift. The header
/// leaves its `Profile` row out for a run that resolved none, so the same
/// derivation decides the column: a nameless token here would name a profile
/// the row above says nothing has, which is the shape `derivable_profile`
/// exists to refuse. Those rows carry [`NO_DETAIL`] instead.
fn managed_resource_rows(
    items: &[cfgd_core::state::ManagedResource],
    modules: &[ModuleStatusEntry],
    profile: Option<&str>,
) -> Vec<[String; 4]> {
    let mut rows: Vec<[String; 4]> = Vec::new();
    let profile_owner = profile.map_or_else(
        || NO_DETAIL.to_string(),
        |p| cfgd_core::reconciler::Owner::profile(p).token(),
    );
    // Keyed by (manager, source) rather than manager alone: two sources
    // delivering one manager's packages are two facts, and a merged row would
    // attribute both to whichever source sorted first.
    let mut own_packages: std::collections::BTreeMap<(&str, &str), Vec<&str>> =
        std::collections::BTreeMap::new();

    for r in items {
        if let Some((manager, package)) = package_id_parts(&r.resource_type, &r.resource_id) {
            own_packages
                .entry((manager, r.source.as_str()))
                .or_default()
                .push(package);
            continue;
        }
        let Some((module, rest)) = module_id_parts(&r.resource_type, &r.resource_id) else {
            // Same rationale as the Drift section above: condense a "script" /
            // "Running script" resource_id only for this table cell, never the
            // stored id itself.
            let resource = if r.resource_type == "script" || r.resource_type == "Running script" {
                condense_script_label(&r.resource_id)
            } else if is_session_env_row(r) {
                session_env_resource()
            } else {
                cfgd_core::fold_home_in_text(&r.resource_id)
            };
            rows.push([
                display_type(&r.resource_type),
                recorded_owner(r, &profile_owner),
                resource,
                r.source.clone(),
            ]);
            continue;
        };
        let owner = cfgd_core::reconciler::Owner::module(module).token();
        let declared = modules
            .iter()
            .find(|m| m.name == module)
            .map(|m| &m.declared);
        let (surface, detail) = rest.split_once(':').unwrap_or((rest, ""));
        let resource = match surface {
            "files" => module_files_resource(detail, declared),
            "packages" => module_packages_resource(detail, declared),
            "script" => declared
                .and_then(|d| d.script_summary.clone())
                .unwrap_or_else(|| NO_DETAIL.to_string()),
            _ if detail.is_empty() => NO_DETAIL.to_string(),
            _ => detail.to_string(),
        };
        rows.push([display_type(surface), owner, resource, r.source.clone()]);
    }

    for ((manager, source), mut packages) in own_packages {
        packages.sort_unstable();
        rows.push([
            display_type("package"),
            profile_owner.clone(),
            format!("{manager}: {}", packages.join(", ")),
            source.to_string(),
        ]);
    }
    // The recorded order is the state store's (type, id); grouping and
    // splitting break it, so the table sorts what it renders instead of
    // letting the shape of the rows decide the order. Owner first, through the
    // reconciler's own comparator, so a reader moving between this table and a
    // plan or apply tree meets the owners in one order.
    let order = owner_render_order(&rows);
    let rank = |token: &String| order.iter().position(|o| o == token).unwrap_or(order.len());
    rows.sort_by(|a, b| rank(&a[1]).cmp(&rank(&b[1])).then_with(|| a.cmp(b)));
    rows
}

/// The Owner column's token for a recorded row that names no module.
///
/// The split is the reconciler's own: an `env` row is a file cfgd authored or
/// a session cfgd published, so it is cfgd's own and carries the same group
/// suffix the tree heads it with — [`cfgd_core::reconciler::SESSION_GROUP`]
/// for the one row whose id names the act, [`cfgd_core::reconciler::ENV_GROUP`]
/// for the files. Everything else in this branch — a package, a managed file,
/// a profile script, a system setting, a secret — is work a user document
/// declared, which is the profile's.
fn recorded_owner(r: &cfgd_core::state::ManagedResource, profile_owner: &str) -> String {
    if r.resource_type != ENV_RESOURCE_TYPE {
        return profile_owner.to_string();
    }
    let group = if is_session_env_row(r) {
        cfgd_core::reconciler::SESSION_GROUP
    } else {
        cfgd_core::reconciler::ENV_GROUP
    };
    cfgd_core::reconciler::Owner::cfgd(group).token()
}

/// The owner a rendered token names, read back so the table can be ordered by
/// the same comparator that ordered the run.
///
/// A cell naming an owner comes from [`cfgd_core::reconciler::Owner::token`]
/// and parses. The one cell that does not is [`NO_DETAIL`], the `-` a
/// profile-declared row carries when the run resolved no profile to name it
/// after: `None` is that cell, and [`owner_render_order`] leaves it out of the
/// order, which sorts every such row after the last named owner.
fn owner_from_token(token: &str) -> Option<cfgd_core::reconciler::Owner> {
    let (kind, name) = token.split_once(':')?;
    Some(cfgd_core::reconciler::Owner {
        kind: cfgd_core::reconciler::OwnerKind::from_token(kind)?,
        name: name.to_string(),
    })
}

/// The Owner column's tokens in the order the plan and apply trees render
/// their groups, asked of [`cfgd_core::reconciler::Owner::order`] — the one
/// way to order owners outside the phase builder, so this table and the tree
/// beside it read the same sequence rather than two.
///
/// A cell naming no owner ([`NO_DETAIL`]) is not in the returned order, so the
/// caller's rank puts it past every named one: a row nothing can attribute
/// sorts under the rows that can be.
fn owner_render_order(rows: &[[String; 4]]) -> Vec<String> {
    let mut owners: Vec<cfgd_core::reconciler::Owner> = rows
        .iter()
        .filter_map(|r| owner_from_token(&r[1]))
        .collect();
    cfgd_core::reconciler::Owner::order(&mut owners);
    owners
        .iter()
        .map(cfgd_core::reconciler::Owner::token)
        .collect()
}

/// Whether this recorded row is the live-session env surface — the one row
/// whose stored id names the ACT (`refresh`) rather than a resource.
fn is_session_env_row(r: &cfgd_core::state::ManagedResource) -> bool {
    r.resource_type == ENV_RESOURCE_TYPE
        && r.resource_id == cfgd_core::state::ENV_SESSION_RESOURCE_ID
}

/// What the Resource column calls the live-session env surface.
///
/// A heading reading `Managed Resources` has to name a resource, and on a host
/// with nothing to publish INTO, the row says so with the same sentence the
/// apply settles that action with — the dashboard resolving it from the same
/// probe rather than reporting a surface it cannot reach as ordinary state.
fn session_env_resource() -> String {
    if cfgd_core::session_manager_available() {
        "session env".to_string()
    } else {
        format!("session env — {}", cfgd_core::NO_SESSION_MANAGER)
    }
}

/// The `(manager, package)` halves of a `package` row's `<manager>/<package>`
/// id. `None` for every other resource type, and for an id missing either
/// half — an id cfgd cannot read is rendered as it stands rather than
/// half-parsed into a row claiming a manager it never named.
fn package_id_parts<'a>(resource_type: &str, resource_id: &'a str) -> Option<(&'a str, &'a str)> {
    if resource_type != "package" {
        return None;
    }
    resource_id
        .split_once('/')
        .filter(|(manager, package)| !manager.is_empty() && !package.is_empty())
}

/// The `(module, remainder)` halves of a `module` row's `<name>:<surface>[…]`
/// id.
fn module_id_parts<'a>(resource_type: &str, resource_id: &'a str) -> Option<(&'a str, &'a str)> {
    if resource_type != "module" {
        return None;
    }
    resource_id
        .split_once(':')
        .filter(|(module, rest)| !module.is_empty() && !rest.is_empty())
}

/// A module's file deployment: where the files land, and how many the apply
/// that recorded the row declared. The count is the recorded fact; the root is
/// what the current resolution says about it. Full paths are
/// `cfgd status --module -o wide`'s job.
fn module_files_resource(recorded_count: &str, declared: Option<&ModuleDeclared>) -> String {
    let count = module_files_count(recorded_count).map(|n| cfgd_core::pluralize(n, "file"));
    let root = declared
        .and_then(|d| d.file_root.as_deref())
        .map(cfgd_core::fold_home_in_text);
    match (root, count) {
        (Some(root), Some(count)) => format!("{root} ({count})"),
        (Some(root), None) => root,
        (None, Some(count)) => count,
        (None, None) => NO_DETAIL.to_string(),
    }
}

/// A module's package install: the names the apply recorded, alphabetical, led
/// by the manager that installs them.
///
/// One recorded row is one manager's group (the planner groups them that way),
/// but the manager itself is not part of the id — it is recovered from the
/// resolution, and only when every name in the row agrees on one, so the row
/// can never name a manager that installs some other part of its own list.
fn module_packages_resource(recorded: &str, declared: Option<&ModuleDeclared>) -> String {
    let names = module_package_names(recorded);
    if names.is_empty() {
        return NO_DETAIL.to_string();
    }
    let list = names.join(", ");
    match row_manager(&names, declared) {
        Some(manager) => format!("{manager}: {list}"),
        None => list,
    }
}

/// The manager that installed every name in one recorded row, or `None` when
/// resolution cannot say.
///
/// The INTERSECTION of the per-name manager sets, not their union: the planner
/// emits one action — and so one recorded row — per manager, so every name in a
/// row shares an installer, and a name declared under two managers narrows to
/// the one its row-mates agree on. Taking one manager per name instead let
/// `neovim`, declared natively AND under npm, decide the whole native row's
/// answer was ambiguous.
fn row_manager<'a>(names: &[&str], declared: Option<&'a ModuleDeclared>) -> Option<&'a str> {
    let declared = declared?;
    let mut shared: Option<std::collections::BTreeSet<&str>> = None;
    for name in names {
        let managers: std::collections::BTreeSet<&str> = declared
            .package_managers
            .get(*name)?
            .iter()
            .map(String::as_str)
            .collect();
        shared = Some(match shared {
            Some(acc) => acc.intersection(&managers).copied().collect(),
            None => managers,
        });
    }
    let shared = shared?;
    (shared.len() == 1)
        .then(|| shared.into_iter().next())
        .flatten()
}

/// The package names one `module:<name>:packages:<a,b,c>` id records, sorted.
///
/// The ONE read of that field, shared by the table cell and the per-module
/// tally the Modules headline reports — a count derived a second way is how the
/// headline came to claim 28 packages over a table listing 24.
fn module_package_names(recorded: &str) -> Vec<&str> {
    let mut names: Vec<&str> = recorded.split(',').filter(|n| !n.is_empty()).collect();
    names.sort_unstable();
    names
}

/// The file count one `module:<name>:files:<n>` id records.
fn module_files_count(recorded: &str) -> Option<usize> {
    recorded.parse::<usize>().ok()
}

/// One module's share of the Managed Resources table: a slot per module-owned
/// kind the Type column spells (`env` is cfgd's own, so it has none here).
///
/// A named struct rather than a tuple because the headline reads every slot in
/// order and a fourth kind reaching the table has to be given one — an unnamed
/// position is what let the `script` rows fall out of the summary silently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ModuleTally {
    pub packages: usize,
    pub files: usize,
    pub scripts: usize,
}

/// What the Managed Resources table says this host manages for each module.
///
/// The Modules headline reports these counts rather than the module's resolved
/// declaration, because the two answer different questions and the report put
/// them one section apart: resolution says what the module WOULD put on a host,
/// the recorded rows say what cfgd HAS put on this one. Only the second is
/// what the table beneath the headline lists, and a headline that disagrees
/// with the table under it is a report arguing with itself.
///
/// `declared` is what the table's own cells read for the same rows, so the two
/// renderings of one row cannot name different numbers.
pub(super) fn recorded_module_tallies(
    items: &[cfgd_core::state::ManagedResource],
    declared: &std::collections::BTreeMap<String, ModuleDeclared>,
) -> std::collections::BTreeMap<String, ModuleTally> {
    let mut tallies: std::collections::BTreeMap<String, ModuleTally> =
        std::collections::BTreeMap::new();
    for r in items {
        let Some((module, rest)) = module_id_parts(&r.resource_type, &r.resource_id) else {
            continue;
        };
        let (surface, detail) = rest.split_once(':').unwrap_or((rest, ""));
        let entry = tallies.entry(module.to_string()).or_default();
        match surface {
            "packages" => entry.packages += module_package_names(detail).len(),
            "files" => entry.files += module_files_count(detail).unwrap_or(0),
            // Every hook a module runs collapses onto one `module:<name>:script`
            // id, so the recorded row carries no count of its own and the number
            // is the one its table cell prints — assigned, not accumulated,
            // because a second such row is the same row.
            "script" => entry.scripts = declared.get(module).map_or(0, |d| d.scripts),
            _ => {}
        }
    }
    tallies
}

/// The word the Type column reads for a recorded `resource_type` or for a
/// module row's own surface.
///
/// The recorded vocabularies are state-matching keys and stay exactly as they
/// are; the column names the KIND, and names it in the singular the way
/// `kubectl get` does — the Resource column beside it already carries the
/// count, so a plural here only made one table spell one kind two ways
/// depending on which producer recorded the row.
fn display_type(kind: &str) -> String {
    match kind {
        "file" | "files" => "file",
        "package" | "packages" => "package",
        "script" | "Running script" => "script",
        other => other,
    }
    .to_string()
}

/// Build the per-module `cfgd status <module>` Doc.
///
/// Every row's subject is the thing's identity and its detail is what the
/// machine holds — the same grammar the fleet doc's module rows read in, so
/// one report never states a fact the other contradicts.
///
/// `now` is a parameter, not a clock read, so the `Last Applied` age pins in a
/// golden.
pub fn build_module_status_doc(output: &ModuleStatus, view: ModuleStatusView, now: &str) -> Doc {
    // One aligned block: the Status row needs a role-tinted value, which only
    // `kv_rows` can carry, and `kv_rows` does not coalesce with a preceding
    // `kv` block — so every row of the header is built here.
    let (state_word, role) = output.state_display();
    let mut rows = vec![KvPair::role_valued("Status", state_word, role)];
    // Directly under `Status`, so the two rows a reader scans for the module's
    // standing lead the block together. Only an isolated run's scope:
    // `recorded_scope_row` answers `Profile` for a profile-wide apply, which
    // belongs to `cfgd status` rather than to one module's report.
    if let Some(("Scope", scope)) = output.scope.as_deref().and_then(recorded_scope_row) {
        rows.push(scope_row(scope));
    }
    if let Some(last) = &output.last_applied {
        // The age, not the recorded instant: `-o json`'s `lastApplied` carries
        // the exact moment, and the row a person reads answers how long ago.
        rows.push(KvPair::new(
            "Last Applied",
            cfgd_core::humanize_age_cell(Some(last), now),
        ));
    }
    // The counts are what the compact view has INSTEAD of the inventories: a
    // report that showed both would state every fact twice.
    if view == ModuleStatusView::Compact {
        rows.push(KvPair::new("Packages", output.packages.to_string()));
        rows.push(KvPair::new("Files", output.files.to_string()));
        // `Aliases` and `Env` are the two halves of the shell surface `diff`
        // reports under `Shell` and the drift engine records as the `shell`
        // kind, so the dashboard names them the same way: a total with the
        // halves nested under it, the shape `Scripts` already uses. Aliases
        // lead, the order every surface naming the pair renders them in.
        if output.env > 0 || output.aliases > 0 {
            rows.push(KvPair::new(
                "Shell",
                (output.env + output.aliases).to_string(),
            ));
            if output.aliases > 0 {
                rows.push(KvPair::nested("Aliases", output.aliases.to_string()));
            }
            if output.env > 0 {
                rows.push(KvPair::nested("Env", output.env.to_string()));
            }
        }
        // A total with one row per declaring hook beneath it: a single-line
        // summary reads as the one hook that declares most and hides the rest.
        let hooks = output.declared.script_counts();
        if !hooks.is_empty() {
            rows.push(KvPair::new(
                "Scripts",
                output.declared.script_total().to_string(),
            ));
            rows.extend(
                hooks
                    .into_iter()
                    .map(|(hook, count)| KvPair::nested(hook, count.to_string())),
            );
        }
        if !output.system.is_empty() {
            rows.push(KvPair::new("System", output.system.join(", ")));
        }
        if !output.depends.is_empty() {
            rows.push(KvPair::new("Dependencies", output.depends.join(", ")));
        }
    }

    let mut doc = Doc::new()
        .heading_title("Status", &output.name)
        .kv_rows(rows);

    doc = match view {
        ModuleStatusView::Compact => {
            let doc = render_module_drift_section(doc, &output.drift, output.drift_checked_live);
            // Same rule as the fleet report: the Drift line states what is
            // recorded, and the ONE hint about checking the machine closes the
            // report. This surface holds no scan timestamp, so "unchecked" is
            // the whole of its staleness.
            if !output.drift_checked_live {
                doc.hint(SCAN_HINT)
            } else if output.drift.is_empty() {
                doc
            } else {
                // The scan found drift, so the report closes on the command
                // that heals it, scoped to the module the report is about.
                doc.hint(super::heal_drift_hint(Some(&output.name)))
            }
        }
        // No Drift section: every finding is already an inline verdict on the
        // inventory row for the thing it was found on, and repeating it below
        // would let one report state a verdict twice.
        ModuleStatusView::Inventory { show_values } => {
            render_module_inventories(doc, output, show_values)
        }
    };

    doc.with_data(ModuleStatusPayload {
        module: output,
        state: state_word,
        script_counts: output
            .declared
            .script_counts()
            .into_iter()
            .map(|(hook, count)| HookCount { hook, count })
            .collect(),
    })
}

/// How much of a module a status report itemizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleStatusView {
    /// Counts, then the drift the scan found (the default).
    Compact,
    /// One row per declared item, with each one's verdict inline (`-o wide`).
    /// `show_values` renders the declared value beside a name and a script's
    /// whole body in place of its condensed label.
    Inventory { show_values: bool },
}

/// The wide view's inventories: one section per declared surface, each row
/// carrying its own verdict.
fn render_module_inventories(doc: Doc, output: &ModuleStatus, show_values: bool) -> Doc {
    let mut doc =
        doc.section_if_nonempty("Installed Packages", &output.package_state, |s, pkgs| {
            let mut sorted: Vec<&ModulePackageStatus> = pkgs.iter().collect();
            sorted.sort_by(|a, b| a.name.cmp(&b.name));
            sorted.into_iter().fold(s, |s, pkg| {
                // An installed row names the manager that has it and nothing else
                // — the section heading already says installed. Every other row
                // leads with the verdict, because that is the exception it reports.
                let detail = match (&pkg.manager, pkg.state) {
                    (Some(m), ModulePackagePresence::Installed) => m.clone(),
                    (Some(m), state) => format!("{} ({m})", state.label()),
                    (None, state) => state.label().to_string(),
                };
                s.status_with(pkg.state.role(), &pkg.name, |f| f.detail(detail))
            })
        });

    // The cause a drifted file's row carries, keyed through the id producer
    // both halves already agree on (`drifted_ids` matches the same way): a
    // deployed path and a finding's item are two spellings of one file, and
    // comparing them directly misses whenever they differ (a relative
    // manifest path against an absolute finding).
    let file_causes: std::collections::HashMap<String, String> = output
        .drift
        .iter()
        .filter(|d| d.surface == SURFACE_FILES)
        .map(|d| {
            (
                super::live_drift::module_file_resource_id(&d.owner, &d.item),
                cfgd_core::output::drift_terse_cause(
                    &d.event.resource_type,
                    d.event.expected.as_deref().unwrap_or_default(),
                    d.event.actual.as_deref().unwrap_or_default(),
                ),
            )
        })
        .collect();

    doc = doc.section_if_nonempty("Deployed Files", &output.deployed_files, |s, files| {
        files.iter().fold(s, |s, file| {
            // The row folds home like every display slot; the cause lookup
            // and the payload keep the absolute path the id was recorded with.
            let shown = cfgd_core::fold_home_in_text(&file.path);
            match file.state {
                // A converged row says the path and stops: "deployed" under a
                // heading that already says Deployed Files is a word per row
                // that adds nothing.
                ModuleFilePresence::Deployed => s.status(Role::Ok, shown),
                ModuleFilePresence::Drifted => {
                    let cause = file_causes
                        .get(&super::live_drift::module_file_resource_id(
                            &output.name,
                            &file.path,
                        ))
                        .cloned()
                        .unwrap_or_else(|| file.state.label().to_string());
                    s.status_with(file.state.role(), shown, |f| f.detail(cause))
                }
                _ => s.status_with(file.state.role(), shown, |f| f.detail(file.state.label())),
            }
        })
    });

    // The same grouping the compact header uses, and the one `diff` reports
    // under: env vars and aliases are two halves of one surface, and listing
    // them as siblings of `Files` said they were two.
    if !output.declared.env.is_empty() || !output.declared.aliases.is_empty() {
        doc = doc.section("Shell", |s| {
            let s = s.subsection_if_nonempty("Aliases", &output.declared.aliases, |s, aliases| {
                let mut sorted: Vec<&cfgd_core::config::ShellAlias> = aliases.iter().collect();
                sorted.sort_by(|a, b| a.name.cmp(&b.name));
                sorted.into_iter().fold(s, |s, alias| {
                    let subject = if show_values {
                        super::helpers::quoted_assignment(&alias.name, &alias.command)
                    } else {
                        alias.name.clone()
                    };
                    s.status(
                        Role::Ok,
                        super::module::list_show::gated_value(subject, alias),
                    )
                })
            });
            s.subsection_if_nonempty("Env", &output.declared.env, |s, env| {
                let mut sorted: Vec<&cfgd_core::config::EnvVar> = env.iter().collect();
                sorted.sort_by(|a, b| a.name.cmp(&b.name));
                sorted.into_iter().fold(s, |s, ev| {
                    let subject = if show_values {
                        super::helpers::quoted_assignment(&ev.name, &ev.value)
                    } else {
                        ev.name.clone()
                    };
                    // Declared state, so a gated entry is listed and annotated
                    // exactly as `module show` annotates it.
                    s.status(Role::Ok, super::module::list_show::gated_value(subject, ev))
                })
            })
        });
    }

    // Execution order, never alphabetical: the order is the fact — a
    // `postApply` that runs after a `preApply` is the only thing the list says
    // about when either one happens.
    doc.section_if_nonempty("Scripts", &output.declared.scripts, |s, hooks| {
        hooks.iter().fold(s, |s, hook| {
            hook.bodies.iter().fold(s, |s, body| {
                // The whole body under `--show-values`, line breaks intact:
                // the renderer lays a multi-line subject out as continuations
                // indented to its own marker column, so the body stays part of
                // the row that names the hook it runs in.
                let subject = if show_values {
                    cfgd_core::reconciler::DisplaySubject {
                        marker: Some(hook.hook.to_string()),
                        body: body.clone(),
                    }
                } else {
                    cfgd_core::reconciler::hook_script_subject(hook.hook, body)
                };
                s.status(Role::Ok, subject.to_string())
            })
        })
    })
}

/// Doc for the `cfgd status <module>` not-found path. Renders the module
/// header and an info note; structured consumers get a payload with packages=0
/// and `status: "not found"`. Returns Ok(()) — no main-side error rendering.
pub fn build_module_status_not_found_doc(name: &str) -> Doc {
    let payload = ModuleStatus {
        name: name.to_string(),
        packages: 0,
        files: 0,
        env: 0,
        aliases: 0,
        scripts: Vec::new(),
        declared: cfgd_core::modules::ModuleSurfaces::default(),
        system: Vec::new(),
        depends: Vec::new(),
        status: "not found".into(),
        last_applied: None,
        scope: None,
        package_state: Vec::new(),
        deployed_files: Vec::new(),
        drift: Vec::new(),
        drift_checked_live: false,
    };
    let (state_word, _) = payload.state_display();
    Doc::new()
        .heading_title("Status", name)
        .status(Role::Info, format!("Module '{}' not found", name))
        .with_data(ModuleStatusPayload {
            module: &payload,
            state: state_word,
            script_counts: Vec::new(),
        })
}

pub(super) fn cmd_status(
    cli: &Cli,
    printer: &Printer,
    module_filter: Option<&str>,
    exit_code: bool,
    scan: bool,
    show_values: bool,
) -> anyhow::Result<()> {
    // `--exit-code` implies the live scan `--scan` names explicitly: a CI
    // gate has to reflect reality regardless of whether the caller also asked
    // to see it. `exit_code` alone still decides whether the run EXITS
    // nonzero on drift — `--scan` on its own never changes the exit code.
    let do_scan = exit_code || scan;
    let ctx = RunContext::new(cli, printer);
    if let Some(mod_name) = module_filter {
        // `--show-values` is a request to see the declared items themselves,
        // which only the itemized view has rows for — so it implies it rather
        // than silently doing nothing beside the counts.
        let view = if printer.is_wide() || show_values {
            ModuleStatusView::Inventory { show_values }
        } else {
            ModuleStatusView::Compact
        };
        return cmd_status_module(&ctx, mod_name, exit_code, do_scan, view);
    }

    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let state = ctx.state()?;

    let last_apply = state.last_apply()?;
    // Read before this run's own scan (if any) overwrites it: the header's
    // staleness signal is about what the RECORDED state was last checked
    // against, not about the scan this very invocation is about to perform.
    let last_scan_at = state.last_scan_at()?;
    let drift_events = state.unresolved_drift()?;
    let source_records = if !cfg.spec.sources.is_empty() {
        state.config_sources()?
    } else {
        vec![]
    };
    // Only rows `cfgd decide` can still act on: a decision outliving the source
    // that raised it withholds nothing from a plan, so listing it here would
    // report work awaiting an answer that no answer can release.
    let mut pending = reconciler::Subscriptions::known(cfg.spec.sources.iter().map(|s| &s.name))
        .answerable(state.pending_decisions()?);
    let resources = state.managed_resources()?;

    let config_dir = config_dir(cli);

    // Compose with sources (cache-only — read paths stay offline) and resolve the
    // effective module set once, so the module dashboard and the `-e` live scan
    // both reflect the same source-composed desired state that `apply` writes.
    let mut desired = resolve_desired_state(
        &ctx,
        cfg,
        local_resolved,
        &[],
        false,
        printer,
        false,
        composition::ConstraintMode::Report,
    )?;
    // Taken ONLY for the live scan below, the one half that reads a registry: a
    // plain `cfgd status` is an offline dashboard, and building a registry it
    // never reads would construct every package manager and configurator the
    // host supports for nothing. Taken here rather than at the scan because the
    // two field moves below are partial moves out of `desired`, which block
    // the `&mut self` the accessor needs — and `Some` exactly when `do_scan`,
    // so the scan below can bind it instead of re-testing the flag.
    let registry = do_scan.then(|| desired.take_registry(cfg));
    let composed_sources = desired.sources;
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;
    // The ownership record the machine will honour, read by the decision rows
    // below so an item a module or a higher layer outranks says so.
    let entry_owners = reconciler::merged_entry_owners(&resolved, &resolved_modules);
    // ONE merge for the whole command: every recompute below asks the same
    // declaration, and building it per drift row clones the profile's env, its
    // aliases and both origin maps once per finding.
    let merged_env_items = cfgd_core::reconciler::MergedEnvItems::new(
        &resolved.merged.env,
        &resolved.merged.aliases,
        &resolved.merged.entry_owners,
        &resolved_modules,
        &cfgd_core::reconciler::recorded_manager_path_dirs(
            state,
            &resolved.merged,
            &resolved_modules,
        ),
    );

    // The plan withholds items no run has recorded a row for yet; a dashboard
    // that hides them contradicts the plan it summarizes. Same classification
    // source `plan` reads, still read-only — the `id` 0 rows mark items whose
    // row `cfgd decide` (or the next apply/tick) will mint. Unlike the gate in
    // plan/apply, a dashboard DEGRADES rather than dying: a classification
    // failure (a malformed package manifest, say) costs the unrecorded rows
    // and says so, never the whole status surface. And with no sources there
    // is nothing to classify, so none of the classification's work runs.
    let mut classification_degraded: Option<(
        super::output_types::ClassificationDegradedCode,
        String,
    )> = None;
    let mut warnings: Vec<String> = Vec::new();
    if !cfg.spec.sources.is_empty() {
        // The dashboard enumerates no package state (it is offline by design),
        // so the classification sees an empty observation and auto-accepts
        // nothing — installed-but-undecided items keep their pending rows
        // here and are released by the next plan/apply/tick, which does
        // enumerate.
        match plan_ops::withheld_for_run(
            &ctx,
            state,
            cfg,
            plan_ops::DesiredOwnership {
                resolved: &resolved,
                entry_owners: &entry_owners,
            },
            true,
            plan_ops::DecisionWrites::ReadOnly,
            &reconciler::ActualPackages::default(),
        ) {
            Ok((withheld, _review)) => {
                warnings = withheld.undecidable.iter().map(|b| b.warning()).collect();
                pending.extend(withheld.pending.into_iter().filter(|d| d.id == 0));
            }
            Err(e) => {
                let code = super::output_types::ClassificationDegradedCode::from_error(&e);
                let reason = cfgd_core::output::collapse_to_subject_line(format!("{e:#}"));
                printer.status_simple(
                    Role::Warn,
                    format!("Source decisions not classified: {reason}"),
                );
                classification_degraded = Some((code, reason));
            }
        }
    }

    let state_map = module_state_map(state);
    let declared: std::collections::BTreeMap<String, ModuleDeclared> = resolved_modules
        .iter()
        .map(|module| (module.name.clone(), ModuleDeclared::of(module)))
        .collect();
    let tallies = recorded_module_tallies(&resources, &declared);
    let module_entries: Vec<ModuleStatusEntry> = resolved_modules
        .iter()
        .map(|module| {
            let status = state_map
                .get(&module.name)
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "not applied".into());
            let tally = tallies.get(&module.name).copied().unwrap_or_default();
            ModuleStatusEntry {
                name: module.name.clone(),
                packages: tally.packages,
                files: tally.files,
                scripts: tally.scripts,
                status,
                platform_skip_reason: module.platform_skip_reason.clone(),
                declared: declared.get(&module.name).cloned().unwrap_or_default(),
            }
        })
        .collect();

    // The declared catalog, not just the names: the shared `Sources` table
    // carries columns (origin, priority, signing demand) the status payload
    // never held.
    let configured_sources = super::source::list::configured_source_entries(cfg, state);

    let mut output = StatusOutput {
        last_apply,
        drift: drift_events,
        sources: source_records,
        pending_decisions: pending,
        modules: module_entries,
        managed_resources: resources,
        warnings,
        classification_degraded: classification_degraded.is_some(),
        classification_degraded_code: classification_degraded.as_ref().map(|(c, _)| *c),
        classification_degraded_reason: classification_degraded.map(|(_, r)| r),
        drift_checked_live: do_scan,
        last_scan_at,
    };

    // A RECORDED env-var/alias row holds the opaque markers `verify_env_items`
    // persists, so without this the dashboard and `--scan` word the same env
    // var two different ways — one naming no value at all. The recompute lands
    // in the ADDITIVE display pair rather than over `expected`/`actual`: those
    // describe the row stored under this `id`, and a fresher reading written
    // into them describes a row nobody wrote. Nothing here is written back.
    //
    // Only a shell kind can answer at all, and the gate is what keeps a file,
    // package or system row from paying the profile-and-modules env merge to
    // be told so. A `--scan` run skips the recompute outright: the live scan
    // below clears this vector and refills it from its own findings, so any
    // work spent on the recorded rows here would be discarded unrendered.
    if !do_scan {
        output.drift.retain_mut(|event| {
            if !cfgd_core::output::is_shell_drift_kind(&event.resource_type) {
                return true;
            }
            let Some((want, have)) =
                merged_env_items.display_values(&event.resource_type, &event.resource_id)
            else {
                return true;
            };
            // The recompute just read the machine: a row whose declared line
            // is the line the file holds has HEALED since it was recorded,
            // and rendering it would put `want: X, have: X` under a warning
            // glyph — a finding that refutes itself. It leaves the `-o json`
            // payload with the human row (this vector IS `drift[]`), which is
            // the point: a machine consumer must read the same converged
            // verdict a reader does. The exit code stays in agreement for
            // free — it counts the LIVE scan's findings, and a content-aware
            // scan does not report a converged entry either. Nothing is
            // written back: plain `status` reads state, and the stored row
            // clears on the next apply or scan that touches it.
            if want == have {
                return false;
            }
            event.want = Some(want);
            event.have = Some(have);
            true
        });
    }

    // Plain `status` (no --scan/--exit-code) keeps the fast RECORDED-drift
    // dashboard by deliberate design. `--scan` (and `--exit-code`, which
    // implies it), however, must reflect REALITY: a host with no daemon and no
    // prior scan has zero recorded events even when a managed file was just
    // edited out-of-band. So when scanning, run the LIVE scan (the same
    // checks `diff`/`verify` run — the engine records every finding and
    // resolves what it did not re-find, and the last-scan stamp this header
    // reads back next time is written here) BEFORE emitting, fold its
    // findings into the displayed Drift section, then exit 5 if
    // `--exit-code` asked for it and any drift was found. This keeps the
    // human verdict and the exit code in agreement instead of printing "No
    // drift detected" alongside exit 5.
    let live_drift = if let Some(mut registry) = registry {
        ctx.resolve_manifest_packages(&mut resolved.merged.packages)?;
        registry.set_system_config_dir(&config_dir);
        let cfgd_installed = cfgd_installed_packages(state)?;
        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
        let drift = super::live_drift::live_drift_results(
            &config_dir,
            &resolved,
            &registry,
            &resolved_modules,
            &cfgd_installed,
            state,
            &pkg_cx,
        )?;
        // The payload's `lastScanAt` must describe the scan that PRODUCED it,
        // or a consumer pairing it with `driftCheckedLive: true` reads
        // "scanned live, last scanned two hours ago". A refused write leaves
        // the pre-scan value read above standing, so the field never names a
        // stamp the store does not hold. The header row read that same value
        // and is not rendered on this branch anyway.
        if let Some(stamped) = state.record_scan() {
            output.last_scan_at = Some(stamped);
        }
        // The scan is a FULL-machine check: it just recorded its findings and
        // resolved every recorded row it did not re-find, so the recorded
        // rows read above are stale on this branch — a row the scan re-found
        // is worded fresher by the scan itself, and a row it did not re-find
        // was just resolved as healed. The displayed set IS the scan's.
        output.drift.clear();
        for r in &drift {
            output
                .drift
                .push(super::live_drift::drift_event_from(r, &merged_env_items));
        }
        drift
    } else {
        Vec::new()
    };

    // Built from the composition this command already resolved: the rows say
    // what each withheld item would put on the machine, and re-deriving that
    // at the render would be a second parse of the same config.
    let decision_contents = super::DecisionContents::for_decisions(
        &resolved,
        &output.pending_decisions,
        &config_dir,
        &entry_owners,
    );
    printer.emit(build_fleet_status_doc(
        &output,
        &cfgd_core::output::ConfigHeader {
            config_path: Some(&cli.config),
            sources: &composed_sources,
            profile: derivable_profile(profile_name),
            profile_inherits: &resolved.inherits_chain(),
            modules: &cfgd_core::output::HeaderModule::of_resolved(&resolved_modules),
        },
        &configured_sources,
        &cfgd_core::utc_now_iso8601(),
        &decision_contents,
    ));

    if exit_code && !live_drift.is_empty() {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

/// Pair each DECLARED package with the scan verdict resolution produced for it.
///
/// The two lists are joined by name and by ORDER, never by name alone: one name
/// may be declared twice under two managers (the `brew` / `brew-cask` shape),
/// and a single slot per name kept only the last verdict and rendered both rows
/// as that one manager. A gated entry is answered before the queue is drawn
/// from — it produced no resolution, so consuming one would hand it the verdict
/// belonging to its same-named sibling.
fn join_package_state(
    declared: &[cfgd_core::config::ModulePackageEntry],
    scanned: &mut std::collections::HashMap<
        String,
        std::collections::VecDeque<(String, ModulePackagePresence)>,
    >,
    here: &Platform,
) -> Vec<ModulePackageStatus> {
    declared
        .iter()
        .map(|p| {
            if !here.matches_any(&p.platforms) {
                return ModulePackageStatus {
                    name: p.name.clone(),
                    manager: None,
                    state: ModulePackagePresence::PlatformSkipped,
                };
            }
            match scanned
                .get_mut(&p.name)
                .and_then(std::collections::VecDeque::pop_front)
            {
                Some((manager, state)) => ModulePackageStatus {
                    name: p.name.clone(),
                    manager: Some(manager),
                    state,
                },
                None => ModulePackageStatus {
                    name: p.name.clone(),
                    manager: None,
                    state: ModulePackagePresence::NotScanned,
                },
            }
        })
        .collect()
}

pub(super) fn cmd_status_module(
    ctx: &RunContext<'_>,
    mod_name: &str,
    exit_code: bool,
    do_scan: bool,
    view: ModuleStatusView,
) -> anyhow::Result<()> {
    let cli = ctx.cli();
    let printer = ctx.printer();
    let config_dir = ctx.config_dir();
    // Propagate (vs. unwrap_or_default in cmd_status): the module-scoped path
    // queries a single named module, so a missing cache dir means the query
    // cannot be answered, and it must error rather than silently claim the
    // module was not found.
    let cache_base = module_cache_dir(cli)?;
    let all_modules = modules::load_all_modules(config_dir, &cache_base, &[], printer)?;

    let module = match all_modules.get(mod_name) {
        Some(m) => m,
        None => {
            printer.emit(build_module_status_not_found_doc(mod_name));
            return Ok(());
        }
    };

    let state = ctx.state()?;
    let state_rec = state.module_state_by_name(mod_name)?;

    let status = state_rec
        .as_ref()
        .map(|s| s.status.clone())
        .unwrap_or_else(|| "not applied".into());
    let last_applied = state_rec.as_ref().map(|s| s.installed_at.clone());
    // The apply this module's recorded state came from is what knows the scope
    // it ran under; the module row itself records only when it landed.
    let scope = state_rec
        .as_ref()
        .and_then(|s| s.last_applied)
        .and_then(|id| state.get_apply(id).transpose())
        .transpose()?
        .map(|record| record.profile);

    // Same live, read-only re-check `diff --module` performs, and the same
    // deliberate gate as the profile-wide command: plain `status --module`
    // stays a fast recorded-only dashboard (this module surface has no
    // recorded drift rows of its own to fall back to — module drift is only
    // ever LIVE), and only `--scan`/`--exit-code` (which implies `--scan`)
    // pays for a real scan of the file content and installed packages.
    // Without this, a module that was sabotaged out-of-band read as clean
    // forever, because "Deployed Files" below only checks presence.
    // The scan's findings are recorded MODULE-SCOPED (`live_drift`'s module
    // doc): its own rows land and resolve, and nothing beyond them.
    // Deliberately no `record_scan` below, unlike the fleet-wide path and the
    // sibling scans in `diff`/`verify`: the stamp dates the FLEET-wide
    // dashboard's header, and one module's files and packages are not evidence
    // the machine was checked.
    let mut drift: Vec<ModuleDrift> = Vec::new();
    // The verify ids of the files this scan found drifted. The Deployed Files
    // rows are judged against it, so the two sections state one verdict per
    // file instead of a content check and a presence check disagreeing.
    let mut drifted_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Keyed by DECLARED name, so the rows below can be built from the declared
    // list and the two can never differ in length: a package resolution
    // dropped (a platform gate) is a package nothing asked about, not a
    // package that vanished from the report.
    // One name may be declared TWICE under two managers (the `brew` /
    // `brew-cask` shape), so each key holds a QUEUE in resolution order and
    // each declared row consumes its own verdict. A single slot per name kept
    // only the last, and rendered both rows as that one manager.
    let mut scanned_packages: std::collections::HashMap<
        String,
        std::collections::VecDeque<(String, ModulePackagePresence)>,
    > = std::collections::HashMap::new();
    if do_scan {
        let platform = Platform::current();
        // Deliberately the config-FREE registry: a module resolves against the
        // managers it declares and cannot reach the profile's `packages.custom`,
        // so resolving it through a config-aware registry would map a module
        // package onto a manager the module cannot use.
        let registry = ctx.base_registry();
        let mgr_map = registry.manager_map();
        let pkg_cx = ctx.package_context()?;
        let resolved_modules = modules::resolve_modules(
            &[mod_name.to_string()],
            config_dir,
            &cache_base,
            &[],
            platform,
            &mgr_map,
            Some(&pkg_cx),
            printer,
        )?;
        let resolved = empty_resolved_profile(&[mod_name.to_string()], &ctx.active_profile_name());
        // File and package rows only — the recompute is a no-op for both, but
        // `drift_event_from` takes the merge rather than deciding per row.
        let merged_env_items = cfgd_core::reconciler::MergedEnvItems::new(
            &resolved.merged.env,
            &resolved.merged.aliases,
            &resolved.merged.entry_owners,
            &resolved_modules,
            &cfgd_core::reconciler::recorded_manager_path_dirs(
                state,
                &resolved.merged,
                &resolved_modules,
            ),
        );
        let fm = CfgdFileManager::new(config_dir, &resolved)?;
        // One spinner across this module's live scan, narrated per pass.
        printer.narrate(
            format!("Scanning module:{mod_name} files"),
            |sp| -> anyhow::Result<()> {
                // The scoped record's two halves: every key this scan
                // re-checked, and the non-matching subset in its producer's
                // own literals.
                let mut checked: Vec<(String, String)> = Vec::new();
                let mut findings: Vec<cfgd_core::reconciler::VerifyResult> = Vec::new();
                let file_results = super::live_drift::module_file_verify_results(
                    &fm,
                    config_dir,
                    &resolved,
                    &resolved_modules,
                    registry.default_file_strategy,
                    state,
                )?;
                for r in &file_results {
                    checked.push((r.resource_type.clone(), r.resource_id.clone()));
                }
                for r in file_results.into_iter().filter(|r| !r.matches) {
                    findings.push(r.clone());
                    drifted_ids.insert(r.resource_id.clone());
                    // The id is where a file finding carries its owner and its
                    // path; a row names them separately. An id that splits into
                    // neither attributes itself to the module under report,
                    // which is the only module a caller asked about.
                    let (owner, item) =
                        super::live_drift::split_module_file_resource_id(&r.resource_id)
                            .map(|(m, target)| (m.to_string(), target))
                            .unwrap_or_else(|| (mod_name.to_string(), r.resource_id.clone()));
                    drift.push(ModuleDrift {
                        event: super::live_drift::drift_event_from(&r, &merged_env_items),
                        owner,
                        surface: SURFACE_FILES,
                        item,
                    });
                }

                sp.set_message(format!("Scanning module:{mod_name} packages"));
                // ONE context across every package of every resolved module,
                // so a manager is enumerated once however many packages name
                // it (`PackageContext::installed_for`'s memo).
                let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);
                for resolved_module in &resolved_modules {
                    for pkg in &resolved_module.packages {
                        // A `script` package and a manager this host has not
                        // registered are both questions nothing can answer —
                        // `package_missing_drift` returns `None` for each, and
                        // reading that as "installed" would report a verdict
                        // no manager gave. Only an answerable entry joins the
                        // scope the record can vouch for.
                        let scannable =
                            pkg.manager != "script" && mgr_map.contains_key(pkg.manager.as_str());
                        if scannable {
                            checked.push((
                                "package".to_string(),
                                super::diff::package_resource_id(
                                    &pkg.manager,
                                    std::slice::from_ref(&pkg.resolved_name),
                                ),
                            ));
                        }
                        let presence = if !scannable {
                            ModulePackagePresence::NotScanned
                        } else if let Some(pd) =
                            super::diff::package_missing_drift(pkg, &mgr_map, &pkg_cx)
                        {
                            let finding = cfgd_core::reconciler::VerifyResult {
                                resource_type: "package".to_string(),
                                resource_id: super::diff::package_resource_id(
                                    &pd.manager,
                                    &pd.packages,
                                ),
                                matches: false,
                                expected: "installed".to_string(),
                                actual: cfgd_core::Absence::Missing.to_string(),
                                unmanaged: false,
                            };
                            findings.push(finding.clone());
                            drift.push(ModuleDrift {
                                event: super::live_drift::drift_event_from(
                                    &finding,
                                    &merged_env_items,
                                ),
                                // The module whose resolution declared this
                                // package, which is not always the one under
                                // report: a dependency's missing package is
                                // still why the asked module does not work.
                                owner: resolved_module.name.clone(),
                                surface: SURFACE_PACKAGES,
                                item: pd.packages.join(", "),
                            });
                            ModulePackagePresence::NotInstalled
                        } else {
                            ModulePackagePresence::Installed
                        };
                        // Drift is collected for the dependency modules this
                        // resolution pulled in too (they are why the named
                        // module works); the package ROWS report the module
                        // the reader asked about, whose declared count heads
                        // the report.
                        if resolved_module.name == mod_name {
                            scanned_packages
                                .entry(pkg.canonical_name.clone())
                                .or_default()
                                .push_back((pkg.manager.clone(), presence));
                        }
                    }
                }
                super::live_drift::record_scoped_scan_findings(state, &checked, &findings);
                Ok(())
            },
        )?;
    }

    let package_state = join_package_state(
        &module.spec.packages,
        &mut scanned_packages,
        cfgd_core::platform::Platform::current(),
    );

    let deployed_files: Vec<ModuleFileStatus> = state
        .module_deployed_files(mod_name)?
        .into_iter()
        .map(|f| {
            // Absence is definite whether or not a scan ran; presence is not.
            // Without a live check the honest verdict on a file that is THERE
            // is that nothing looked inside it — `Path::exists` cannot tell a
            // converged file from a tampered one.
            let state = if !std::path::Path::new(&f.file_path).exists() {
                ModuleFilePresence::Missing
            } else if !do_scan {
                ModuleFilePresence::NotScanned
            } else if drifted_ids.contains(&super::live_drift::module_file_resource_id(
                mod_name,
                &f.file_path,
            )) {
                ModuleFilePresence::Drifted
            } else {
                ModuleFilePresence::Deployed
            };
            ModuleFileStatus {
                path: f.file_path,
                state,
            }
        })
        .collect();

    // Every declared count and list in the report is read off ONE tally, so
    // the count a summary row states and the list the inventory renders cannot
    // describe different modules.
    let declared = cfgd_core::modules::ModuleSurfaces::of(&module.spec);
    let output = ModuleStatus {
        name: mod_name.to_string(),
        packages: declared.packages,
        files: declared.files,
        env: declared.env.len(),
        aliases: declared.aliases.len(),
        scripts: declared.hook_names(),
        system: declared.system.clone(),
        depends: declared.depends.clone(),
        declared,
        status,
        last_applied,
        scope,
        package_state,
        deployed_files,
        drift_checked_live: do_scan,
        drift,
    };

    printer.emit(build_module_status_doc(
        &output,
        view,
        &cfgd_core::utc_now_iso8601(),
    ));

    if exit_code && !output.drift.is_empty() {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::OwnerLabel;
    use cfgd_core::output::Printer;
    use cfgd_core::output::Verbosity;
    use cfgd_core::state::{ApplyRecord, ApplyStatus};

    /// The `cfgd status -o json` surface must emit the unified camelCase status
    /// token at `.lastApply.status`. InProgress is the variant where the apply/
    /// status/log spellings historically drifted (`InProgress`/`in_progress`/
    /// `inProgress`); this pins the JSON path to `display_str`.
    #[test]
    fn status_json_last_apply_status_is_camelcase_token() {
        let output = StatusOutput {
            last_apply: Some(ApplyRecord {
                id: 1,
                timestamp: "2026-01-02T03:04:05Z".to_string(),
                profile: "default".to_string(),
                plan_hash: "deadbeef".to_string(),
                status: ApplyStatus::InProgress,
                summary: Some("running".to_string()),
            }),
            drift: Vec::new(),
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: Vec::new(),
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: false,
            classification_degraded_code: None,
            classification_degraded_reason: None,
            drift_checked_live: false,
            last_scan_at: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["lastApply"]["status"], serde_json::json!("inProgress"));
        assert_eq!(
            json["lastApply"]["status"],
            serde_json::json!(ApplyStatus::InProgress.display_str())
        );
        assert_eq!(json["classificationDegraded"], serde_json::json!(false));
        assert!(
            json.get("classificationDegradedCode").is_none()
                && json.get("classificationDegradedReason").is_none(),
            "a clean payload carries no code or reason field"
        );
    }

    fn recorded(resource_type: &str, resource_id: &str) -> cfgd_core::state::ManagedResource {
        cfgd_core::state::ManagedResource {
            resource_type: resource_type.to_string(),
            resource_id: resource_id.to_string(),
            source: "local".to_string(),
            last_hash: None,
            last_applied: None,
        }
    }

    /// A declared-package map keyed the way `ModuleDeclared::of` builds it:
    /// one name can be declared under two managers, so the value is a set.
    fn declared_managers(
        pairs: &[(&str, &str)],
    ) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
        let mut map: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for (package, manager) in pairs {
            map.entry((*package).to_string())
                .or_default()
                .insert((*manager).to_string());
        }
        map
    }

    fn nvim_entry(declared: ModuleDeclared) -> ModuleStatusEntry {
        ModuleStatusEntry {
            name: "nvim".to_string(),
            packages: 3,
            files: 6,
            scripts: declared.scripts,
            status: "installed".to_string(),
            platform_skip_reason: None,
            declared,
        }
    }

    fn deployed_to(target: &str) -> cfgd_core::modules::ResolvedFile {
        cfgd_core::modules::ResolvedFile {
            source: std::path::PathBuf::from("/src"),
            target: std::path::PathBuf::from(target),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        }
    }

    /// `Component::RootDir` survives the common-prefix truncation, so files
    /// sharing no directory at all would otherwise report `/` — a root cfgd
    /// does not manage. The files row falls back to its count instead.
    #[test]
    fn files_sharing_no_directory_report_no_root() {
        assert_eq!(
            common_target_root(&[
                deployed_to("/etc/ssh/sshd_config"),
                deployed_to("/home/u/.bashrc"),
            ]),
            None
        );
        assert_eq!(
            module_files_resource("2", Some(&ModuleDeclared::default())),
            "2 files"
        );
    }

    /// A shared directory below the root is still a root worth naming, and one
    /// file answers with itself.
    #[test]
    fn files_sharing_a_directory_report_it() {
        assert_eq!(
            common_target_root(&[
                deployed_to("/home/u/.config/nvim/init.lua"),
                deployed_to("/home/u/.config/nvim/lua/opts.lua"),
            ])
            .as_deref(),
            Some("/home/u/.config/nvim")
        );
        assert_eq!(
            common_target_root(&[deployed_to("/home/u/.bashrc")]).as_deref(),
            Some("/home/u/.bashrc")
        );
    }

    /// Profile-level `package` rows are recorded one package per row, which is
    /// a state-matching key rather than a report. The table renders one row per
    /// manager, names alphabetical whatever order they were recorded in, and
    /// under the owner the plan and apply trees head their group with — the
    /// profile declared them, so `cfgd` would name the wrong document.
    #[test]
    fn profile_packages_collapse_into_one_row_per_manager() {
        let rows = managed_resource_rows(
            &[
                recorded("package", "brew/ripgrep"),
                recorded("package", "apt/git"),
                recorded("package", "brew/bat"),
            ],
            &[],
            Some("base"),
        );
        assert_eq!(
            rows,
            vec![
                [
                    "package".to_string(),
                    "profile:base".to_string(),
                    "apt: git".to_string(),
                    "local".to_string()
                ],
                [
                    "package".to_string(),
                    "profile:base".to_string(),
                    "brew: bat, ripgrep".to_string(),
                    "local".to_string()
                ],
            ]
        );
    }

    /// One owner vocabulary across the report and the run that produced it: a
    /// resource the profile declared is the PROFILE's, spelled with the token
    /// the plan and apply trees head its group with and `diff` reports its
    /// drift under, and `cfgd` is left for what cfgd manages on its own behalf.
    /// The dashboard showed `package  cfgd  brew: gum` two commands after the
    /// apply that installed it printed the same work under `profile:base`.
    ///
    /// Both halves are compared against the reconciler's own owner values
    /// rather than literals — the constructors the planner heads its groups
    /// with, put in order by [`cfgd_core::reconciler::Owner::order`], the ONE
    /// comparator — so neither the spelling nor the order of the Owner column
    /// can drift from the tree without failing here.
    #[test]
    fn a_profile_declared_row_carries_the_owner_the_apply_tree_heads_its_group_with() {
        use cfgd_core::reconciler::{ENV_GROUP, Owner, SESSION_GROUP};

        let rows = managed_resource_rows(
            &[
                recorded("package", "brew/gum"),
                recorded("module", "nvim:files:1"),
                recorded("file", "/home/u/.gitconfig"),
                recorded("env", "/home/u/.cfgd.env"),
                recorded("env", cfgd_core::state::ENV_SESSION_RESOURCE_ID),
            ],
            &[],
            Some("base"),
        );

        let mut expected = vec![
            Owner::module("nvim"),
            Owner::cfgd(SESSION_GROUP),
            Owner::profile("base"),
            Owner::cfgd(ENV_GROUP),
        ];
        Owner::order(&mut expected);
        let expected: Vec<String> = expected.iter().map(Owner::token).collect();

        let mut owners: Vec<String> = rows.iter().map(|r| r[1].clone()).collect();
        owners.dedup();
        assert_eq!(owners, expected, "{rows:?}");
        // The two `env` rows are cfgd's own and carry the group suffix the
        // tree heads them with; the file and the package are the profile's.
        assert_eq!(
            rows.iter()
                .map(|r| (r[0].as_str(), r[1].as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("file", "profile:base"),
                ("package", "profile:base"),
                ("env", "cfgd:env"),
                ("env", "cfgd:session"),
                ("file", "module:nvim"),
            ],
            "{rows:?}"
        );
    }

    /// Two sources delivering one manager's packages are two facts. Merging
    /// them would attribute both to whichever source sorted first.
    #[test]
    fn one_manager_delivered_by_two_sources_stays_two_rows() {
        let mut remote = recorded("package", "brew/fd");
        remote.source = "acme".to_string();
        let rows = managed_resource_rows(
            &[recorded("package", "brew/bat"), remote],
            &[],
            Some("base"),
        );
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0][2], "brew: bat");
        assert_eq!(rows[0][3], "local");
        assert_eq!(rows[1][2], "brew: fd");
        assert_eq!(rows[1][3], "acme");
    }

    /// A run that resolved no profile names one nowhere: the header leaves its
    /// `Profile` row out and the Owner column says `-` rather than inventing a
    /// token for a profile nothing has.
    ///
    /// The row also sorts BELOW every named owner — a module row included —
    /// because a row nothing can attribute belongs under the rows that can be.
    #[test]
    fn a_row_no_profile_can_be_named_for_reads_a_dash_and_sorts_last() {
        let rows = managed_resource_rows(
            &[
                recorded("package", "brew/bat"),
                recorded("module", "nvim:files:2"),
                recorded("env", "~/.cfgd.env"),
            ],
            &[],
            None,
        );
        let owners: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
        assert_eq!(
            owners,
            ["cfgd:env", "module:nvim", cfgd_core::ABSENT],
            "{rows:?}"
        );
    }

    /// A module row's id carries the owner and the surface; the detail the
    /// reader wants (where files land, which manager installs, how many hooks)
    /// lives in the resolution beside it.
    #[test]
    fn a_module_row_names_its_owner_and_reads_its_detail_from_the_resolution() {
        let declared = ModuleDeclared {
            file_root: Some("/home/u/.config/nvim".to_string()),
            package_managers: declared_managers(&[("git", "apt"), ("gcc", "apt")]),
            script_summary: Some("preApply (3 scripts), postApply (6 scripts)".to_string()),
            scripts: 9,
        };
        let rows = managed_resource_rows(
            &[
                recorded("module", "nvim:files:6"),
                recorded("module", "nvim:packages:git,gcc"),
                recorded("module", "nvim:script"),
            ],
            &[nvim_entry(declared)],
            Some("base"),
        );
        let resources: Vec<&str> = rows.iter().map(|r| r[2].as_str()).collect();
        assert!(rows.iter().all(|r| r[1] == "module:nvim"), "{rows:?}");
        assert_eq!(
            resources,
            vec![
                "/home/u/.config/nvim (6 files)",
                "apt: gcc, git",
                "preApply (3 scripts), postApply (6 scripts)",
            ]
        );
    }

    /// The manager prefix is recovered from the resolution, so a row whose
    /// names do not all agree on one manager names none — better silent than
    /// claiming a manager that installs only part of its own list.
    #[test]
    fn a_package_row_names_a_manager_only_when_every_name_agrees() {
        let split = ModuleDeclared {
            package_managers: declared_managers(&[("git", "apt"), ("neovim", "brew")]),
            ..ModuleDeclared::default()
        };
        let rows = managed_resource_rows(
            &[recorded("module", "nvim:packages:neovim,git")],
            &[nvim_entry(split)],
            Some("base"),
        );
        assert_eq!(rows[0][2], "git, neovim");
    }

    /// A module the current config no longer resolves still has recorded rows.
    /// Each keeps whatever the id itself carries and says nothing it cannot
    /// know.
    #[test]
    fn an_unresolvable_module_keeps_the_facts_its_own_id_carries() {
        let rows = managed_resource_rows(
            &[
                recorded("module", "gone:files:4"),
                recorded("module", "gone:packages:zsh"),
                recorded("module", "gone:script"),
            ],
            &[],
            Some("base"),
        );
        let resources: Vec<&str> = rows.iter().map(|r| r[2].as_str()).collect();
        assert_eq!(resources, vec!["4 files", "zsh", "-"]);
    }

    /// The live-session row names the RESOURCE cfgd manages, not the act it
    /// performed on it — every other row in the table is a noun — and when the
    /// host has no session manager the row says so, from the same probe the
    /// apply resolves the action with. A bare `refresh` left the reader with a
    /// verb and no way to tell whether anything is being managed at all.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    #[serial_test::serial]
    fn the_session_env_row_names_the_resource_and_says_when_none_can_be_reached() {
        let session_cell = || {
            let rows = managed_resource_rows(
                &[recorded("env", cfgd_core::state::ENV_SESSION_RESOURCE_ID)],
                &[],
                Some("base"),
            );
            rows[0][2].clone()
        };

        let _missing = cfgd_core::test_helpers::EnvVarGuard::set(
            cfgd_core::SYSTEMCTL_BIN_ENV,
            "/no/such/systemctl",
        );
        assert_eq!(
            session_cell(),
            format!("session env — {}", cfgd_core::NO_SESSION_MANAGER),
            "with nothing to publish to, the row says so"
        );

        let present = std::env::current_exe().expect("the test binary is a real file");
        let _available = cfgd_core::test_helpers::EnvVarGuard::set(
            cfgd_core::SYSTEMCTL_BIN_ENV,
            &present.to_string_lossy(),
        );
        assert_eq!(
            session_cell(),
            "session env",
            "a reachable session manager needs no qualifier"
        );
    }

    /// A recorded type is a state-matching token; the Type column names the
    /// surface in the same words a module row's own id spells them, so one
    /// table never calls one thing two names depending on who declared it.
    #[test]
    fn the_type_column_names_the_surface_not_the_recorded_token() {
        let rows = managed_resource_rows(
            &[
                recorded("file", "/home/u/.bashrc"),
                recorded("env", "/home/u/.cfgd.env"),
                recorded("Running script", "echo hi"),
            ],
            &[],
            Some("base"),
        );
        // Owner order, so the surfaces are named in the order the tree
        // renders them: the profile's two rows, then cfgd's own env file.
        let types: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(types, vec!["file", "script", "env"]);
        // The env file is cfgd's own; the managed file and the profile script
        // are the profile's, and each row says which.
        let owners: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
        assert_eq!(owners, vec!["profile:base", "profile:base", "cfgd:env"]);
    }

    /// Every kind the Type column can print, in the singular — `kubectl get`'s
    /// shape, where the column names WHAT a row is and the row beside it
    /// carries how many.
    ///
    /// The population is both producers: every `resource_type`
    /// `action_resource_info` records (plus the legacy `"Running script"` that
    /// `execute_script` stamped), and the three surfaces a module row's own id
    /// spells. A new kind reaching this column without an arm renders whatever
    /// its producer spelled, so it is listed here or it is not covered.
    #[test]
    fn every_kind_the_type_column_prints_is_singular() {
        const RECORDED_KINDS: &[&str] = &[
            "file",
            "package",
            "secret",
            "system",
            "script",
            "module",
            "env",
            "env-rc",
            "env-session",
            "manager",
            "Running script",
        ];
        const MODULE_SURFACES: &[&str] = &["files", "packages", "script"];

        for kind in RECORDED_KINDS.iter().chain(MODULE_SURFACES) {
            let word = display_type(kind);
            assert!(
                !word.ends_with('s') || word.ends_with("ss"),
                "{kind:?} prints as the plural {word:?}"
            );
            assert!(
                !word.contains(' '),
                "{kind:?} prints as a phrase, not a kind: {word:?}"
            );
        }
        // The two spellings of one surface collapse onto one word, which is the
        // whole point: a `files` module row and a `file` profile row are the
        // same kind of thing.
        assert_eq!(display_type("files"), display_type("file"));
        assert_eq!(display_type("packages"), display_type("package"));
        assert_eq!(display_type("Running script"), display_type("script"));
    }

    /// A config dir whose profile resolves to something its DECLARED list does
    /// not say: one module pulled in by a `depends`, and one gated off this
    /// host. It also declares a `spec.backups[]` unit, so the surfaces that
    /// report under this profile include the backup verbs, and subscribes to
    /// one config source, so every header naming what this machine composed
    /// from has a source to name.
    fn setup_env_with_resolved_modules() -> ResolvedModulesEnv {
        let allow_local =
            cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        // A profile the source PUBLISHES and this machine subscribes to,
        // declaring nothing of its own: the layer is what the `Sources` row
        // names, and a layer that changed the resolved module set would make
        // this fixture unable to say which surface disagreed about which fact.
        let remote = cfgd_core::test_helpers::BareGitRepo::builder()
            .commit(
                "team source",
                &[
                    (
                        "cfgd-source.yaml",
                        "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: team\nspec:\n  provides:\n    profiles:\n      - shared\n",
                    ),
                    (
                        "profiles/shared.yaml",
                        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: shared\nspec: {}\n",
                    ),
                ],
            )
            .build();
        std::fs::write(
            &config_path,
            format!(
                "{CONFIG_YAML}  sources:\n    - name: team\n      origin:\n        type: Git\n        url: {}\n        branch: {}\n      subscription:\n        profile: shared\n",
                remote.url(),
                remote.head_branch(),
            ),
        )
        .unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        let backup_source = config_dir.path().join("data").join("notes.txt");
        std::fs::create_dir_all(backup_source.parent().unwrap()).unwrap();
        std::fs::write(&backup_source, "hello backup").unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - editor\n    - off-host\n  backups:\n    - name: docs\n      source: {}\n      retention: 3\n",
                backup_source.display()
            ),
        )
        .unwrap();
        let module = |name: &str, body: &str| {
            let dir = config_dir.path().join("modules").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("module.yaml"),
                format!(
                    "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: {name}\nspec:\n{body}"
                ),
            )
            .unwrap();
        };
        module("core", "  packages: []\n");
        module("editor", "  depends:\n    - core\n  packages: []\n");
        // The tag this host is not, so the gate fires wherever the suite runs.
        let elsewhere = if cfg!(windows) { "linux" } else { "windows" };
        module(
            "off-host",
            &format!("  platforms:\n    - {elsewhere}\n  packages: []\n"),
        );
        ResolvedModulesEnv {
            config_path,
            _config_dir: config_dir,
            state_dir,
            _remote: remote,
            _allow_local: allow_local,
        }
    }

    /// What [`setup_env_with_resolved_modules`] hands back. A struct rather
    /// than a tuple because the source's bare repository and the local-origin
    /// allowance have to outlive the surfaces being driven, and a `_`-prefixed
    /// tuple slot at a call site is one rename away from being dropped early.
    struct ResolvedModulesEnv {
        config_path: std::path::PathBuf,
        _config_dir: tempfile::TempDir,
        state_dir: tempfile::TempDir,
        _remote: cfgd_core::test_helpers::BareGitRepo,
        _allow_local: cfgd_core::test_helpers::EnvVarGuard,
    }

    /// The header block's rows in the ONE ruled order, ASSERTING as it reads
    /// that each sits where the reader is scanning: in the order `Config`,
    /// `Sources`, `Profile`, `Modules`, and all in one key column.
    ///
    /// A surface that emits a row outside its header block renders it at a
    /// different indent — which is what `sync` did with `Modules`, printing it
    /// at column 0 beside a `Profile` two spaces in — and no comparison of the
    /// words alone can see that. A surface that orders the block its own way
    /// is invisible to a per-row comparison for the same reason. The interior
    /// run of each row collapses: the key column is padded to the widest key
    /// of each surface's own row set, a layout fact rather than a
    /// disagreement about what the machine is configured from.
    ///
    /// A row a surface legitimately has no fact for is absent, not empty, so
    /// the returned rows are only the ones it rendered.
    fn rendered_header_rows(out: &str) -> Vec<String> {
        const KEYS: [&str; 4] = ["Config", "Sources", "Profile", "Modules"];
        // A `Sources` SECTION heading and a `Config` block heading wear the
        // same word with no value beside them, so a row is recognised by the
        // key being followed by its value.
        let row_of = |key: &str| {
            out.lines().enumerate().find(|(_, l)| {
                let code = l.trim_start();
                code.strip_prefix(key)
                    .is_some_and(|rest| rest.starts_with(' ') && !rest.trim().is_empty())
            })
        };
        let found: Vec<(usize, usize, String)> = KEYS
            .iter()
            .filter_map(|key| row_of(key))
            .map(|(n, line)| {
                (
                    n,
                    line.len() - line.trim_start().len(),
                    line.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect();
        assert!(!found.is_empty(), "no header row rendered at all:\n{out}");
        for pair in found.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "the header block reads {:?} before {:?}, and the ruled order is \
                 Config, Sources, Profile, Modules:\n{out}",
                pair[1].2,
                pair[0].2
            );
            assert_eq!(
                pair[0].1, pair[1].1,
                "{:?} left the header's key column:\n{out}",
                pair[1].2
            );
        }
        found.into_iter().map(|(_, _, row)| row).collect()
    }

    /// The dashboard opens on the same `Modules` row `cfgd diff` does — two
    /// surfaces, two independent resolutions of one profile.
    ///
    /// The README demo opened on a `cfgd status` naming a profile and nothing
    /// it resolved to, two commands above an apply header that named `nvim` —
    /// one machine, two headers, only one of which said what was on it. The
    /// fixture is what makes the two capable of DISAGREEING: `editor` pulls
    /// `core` in through `depends`, so the resolved set differs from the
    /// declared list in membership and in order, and `off-host` is gated off
    /// this host, so a surface passing no skips would list it as an ordinary
    /// member.
    #[test]
    #[serial_test::serial]
    fn status_header_names_the_profiles_modules_like_the_apply_header() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let env = setup_env_with_resolved_modules();
        let mut cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        cli.cache_dir = Some(env.state_dir.path().to_path_buf());

        let (printer, buf) = test_printers();
        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);
        let dashboard = cfgd_core::test_helpers::captured_text(&buf);

        let (printer, buf) = test_printers();
        crate::cli::diff::cmd_diff(&cli, &printer, None, false).unwrap();
        drop(printer);
        let diff = cfgd_core::test_helpers::captured_text(&buf);

        let expected = "Modules core, editor (off-host skipped: platform not matched \
                        (requires: windows))";
        let expected = if cfg!(windows) {
            expected.replace("windows", "linux")
        } else {
            expected.to_string()
        };
        assert_eq!(
            rendered_header_rows(&dashboard),
            rendered_header_rows(&diff),
            "two surfaces reporting on one profile must name its modules \
             identically:\n{dashboard}\n---\n{diff}"
        );
        assert!(
            rendered_header_rows(&dashboard).contains(&expected),
            "the dashboard names the resolved set and its gate:\n{dashboard}"
        );
        drop(env);
    }

    /// The gated module reaches `-o json` carrying the reason the human
    /// `Modules` row prints in its skipped annotation.
    ///
    /// Without it a module this host resolves to nothing arrived as an
    /// ordinary entry reading `not applied`, and a consumer could not tell it
    /// apart from one that simply has not been applied yet.
    #[test]
    #[serial_test::serial]
    fn status_json_carries_the_skip_reason_its_modules_row_prints() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let env = setup_env_with_resolved_modules();
        let mut cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        cli.cache_dir = Some(env.state_dir.path().to_path_buf());

        let (printer, buf) = test_printers_json();
        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);
        let payload: serde_json::Value =
            serde_json::from_str(&cfgd_core::test_helpers::captured_text(&buf))
                .expect("status emits a payload");

        let modules = payload["modules"].as_array().expect("modules array");
        let entry = |name: &str| {
            modules
                .iter()
                .find(|m| m["name"] == name)
                .unwrap_or_else(|| panic!("no {name} entry in {payload}"))
        };
        let elsewhere = if cfg!(windows) { "linux" } else { "windows" };
        assert_eq!(
            entry("off-host")["platformSkipReason"],
            serde_json::json!(format!("platform not matched (requires: {elsewhere})")),
        );
        assert_eq!(
            entry("editor").get("platformSkipReason"),
            None,
            "a module that applies here carries no skip reason"
        );
        drop(env);
    }

    /// Every surface reporting on a resolved configuration opens on one
    /// identical header block, in the one ruled order.
    ///
    /// The class, not the pair: `status`, `diff`, `sync`, `daemon status`, the
    /// two plan-running verbs and the three backup verbs each reach their own
    /// resolution, and three of them once named the profile's DECLARED list —
    /// so a profile of one module that `depends` on another read `editor` on
    /// three surfaces and `core, editor` on two. `Sources` was further gone
    /// still: a run header was the only surface in the CLI that named the
    /// sources at all, and it named them UNDER the profile they had composed.
    ///
    /// A verb that resolves no profile of its own has nothing to agree about
    /// and is not in the equality class, but the ruled ORDER binds it like
    /// every other surface — so `module create --apply` is read through the
    /// same reader, which asserts it.
    #[test]
    #[serial_test::serial]
    fn every_surface_reporting_a_resolved_profile_names_the_same_header() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let env = setup_env_with_resolved_modules();
        let mut cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        cli.cache_dir = Some(env.state_dir.path().to_path_buf());

        let render = |run: &dyn Fn(&Printer)| {
            let (printer, buf) = test_printers();
            run(&printer);
            drop(printer);
            rendered_header_rows(&cfgd_core::test_helpers::captured_text(&buf))
        };

        // Read COLD, before anything has fetched the subscribed checkout: read
        // paths compose cache-only, so a header derived from the composition
        // would drop its `Sources` row here and the key would be answering
        // "has this machine synced yet" rather than what the config declares.
        let cold = render(&|p| cmd_status(&cli, p, None, false, false, false).unwrap());
        assert_eq!(
            cold.len(),
            4,
            "a cold cache changes nothing about the header: {cold:?}"
        );

        let status = render(&|p| cmd_status(&cli, p, None, false, false, false).unwrap());
        let diff = render(&|p| crate::cli::diff::cmd_diff(&cli, p, None, false).unwrap());
        let sync = render(&|p| crate::cli::sync::cmd_sync(&cli, p).unwrap());
        // The two verbs that build a `Plan`: their header reads its module
        // gating off the plan's own `Skip` actions rather than off the
        // resolution, which is the one branch that can disagree with the five
        // surfaces above without any of them being wrong about the machine.
        let plan = render(&|p| {
            crate::cli::plan::cmd_plan(&cli, p, &header_plan_args()).unwrap();
        });
        let apply = render(&|p| {
            crate::cli::apply::run_apply(&cli, p, &header_apply_args()).unwrap();
        });
        // `spec.backups[]` is profile-declared, so a backup run reports under a
        // resolved profile exactly as an apply does.
        let backup = render(&|p| {
            crate::cli::backup::run_backup_run(&cli, p, Some("docs")).unwrap();
        });
        // The two verbs that put data back report under the same profile, off
        // the same resolution — and a restore is what leaves the safety copy a
        // rollback puts back, so the legs run in that order.
        let restore = render(&|p| {
            crate::cli::backup::run_backup_restore(
                &cli,
                p,
                &crate::cli::backup::RestoreArgs {
                    name: "docs",
                    at: None,
                    to: None,
                    yes: true,
                },
            )
            .unwrap();
        });
        let rollback = render(&|p| {
            crate::cli::backup::run_backup_rollback(&cli, p, "docs", true).unwrap();
        });

        // The daemon's reader is another process: it renders the modules the
        // reconcile tick put on the wire and the sources its own config
        // declares, holding no composition of its own.
        let (probe, _) = test_printers();
        let ctx = crate::cli::RunContext::new(&cli, &probe);
        let (cfg, profile_name, local_resolved) = ctx.config_and_profile().unwrap();
        let declared = cfgd_core::reconciler::ComposedSource::from_declared(&cfg.spec.sources);
        let profile_name = profile_name.to_string();
        let desired = crate::cli::helpers::resolve_desired_state(
            &ctx,
            cfg,
            local_resolved,
            &[],
            false,
            &probe,
            false,
            cfgd_core::composition::ConstraintMode::Report,
        )
        .unwrap();
        let mut response = crate::cli::daemon::placeholder_status();
        response.running = true;
        response.config_path = Some(cli.config.display().to_string());
        response.profile = Some(profile_name);
        response.modules = cfgd_core::output::HeaderModule::of_resolved(&desired.modules);
        let daemon = render(&|p| {
            p.emit(crate::cli::daemon::build_daemon_status_doc(
                Some(&response),
                &declared,
                &[],
                "2026-05-12T14:30:25Z",
            ))
        });

        assert_eq!(
            status.len(),
            4,
            "the fixture declares a source, a profile and its modules, so every \
             row of the block is fillable: {status:?}"
        );
        for (surface, rows) in [
            ("diff", &diff),
            ("sync", &sync),
            ("plan", &plan),
            ("apply", &apply),
            ("daemon status", &daemon),
            ("backup run", &backup),
            ("backup restore", &restore),
            ("backup rollback", &rollback),
        ] {
            assert_eq!(
                &status, rows,
                "`cfgd {surface}` opens on a different header block from `cfgd status`"
            );
        }

        // The runs that resolve NO profile: not in the equality class, since
        // each names its own module set, but the config each of them read
        // declares the same subscriptions — where a run's configuration comes
        // from is a fact about the config, not about a profile having
        // resolved. Read through the same reader, which fails on a block out
        // of the ruled order or out of the key column.
        let sources_row = status
            .iter()
            .find(|row| row.starts_with("Sources "))
            .expect("the fixture declares a source")
            .clone();
        let config_row = format!("Config {}", cfgd_core::to_posix_string(&cli.config));
        let created = render(&|p| {
            crate::cli::module::cmd_module_create(&cli, p, &header_module_create_args()).unwrap();
        });
        let diff_isolate =
            render(&|p| crate::cli::diff::cmd_diff(&cli, p, Some("editor"), false).unwrap());
        let apply_isolate = render(&|p| {
            let mut args = header_apply_args();
            args.module = vec!["editor".to_string()];
            crate::cli::apply::run_apply(&cli, p, &args).unwrap();
        });
        for (surface, rows, module) in [
            ("module create --apply", &created, "scratch"),
            ("diff --module", &diff_isolate, "editor"),
            // The apply's own plan expands `editor`'s `depends`, so it names
            // the set it will really act on rather than the name it was given.
            ("apply --module", &apply_isolate, "core, editor"),
        ] {
            assert_eq!(
                rows,
                &vec![
                    config_row.clone(),
                    sources_row.clone(),
                    format!("Modules {module}"),
                ],
                "`cfgd {surface}` names its config, the subscriptions that config \
                 declares and the module it is about, and no `Profile` row — it \
                 resolves none: {rows:?}"
            );
        }

        drop(env);
    }

    /// An unfiltered `cfgd plan`, for the header pin above.
    fn header_plan_args() -> crate::cli::PlanArgs {
        crate::cli::PlanArgs {
            from: None,
            phase: None,
            skip: vec![],
            only: vec![],
            module: vec![],
            with_profile: false,
            skip_scripts: false,
            context: "apply".to_string(),
        }
    }

    /// A preview `cfgd apply`: the same header over the same plan, without the
    /// pin having to converge a machine to read it.
    fn header_apply_args() -> crate::cli::ApplyArgs {
        crate::cli::ApplyArgs {
            on_conflict: crate::cli::OnConflict::Ask,
            from: None,
            dry_run: true,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: vec![],
            with_profile: false,
            skip_scripts: false,
            context: "apply".to_string(),
            shell: None,
        }
    }

    /// A module with nothing in it, applied: the header is the whole of what
    /// the pin reads.
    fn header_module_create_args() -> crate::cli::ModuleCreateArgs {
        crate::cli::ModuleCreateArgs {
            name: "scratch".to_string(),
            // Answered, because an unanswered one is a prompt and this pin has
            // no terminal to answer it on.
            description: Some("a module with nothing in it".to_string()),
            depends: vec![],
            packages: vec![],
            files: vec![],
            private: false,
            env: vec![],
            aliases: vec![],
            post_apply: vec![],
            sets: vec![],
            apply: true,
            yes: true,
        }
    }

    /// The module health line's units agree with their own counts: a module
    /// with one of each reads `1 package, 1 file, 1 script`, and anything else
    /// — including zero — keeps the plural.
    #[test]
    fn module_status_line_units_agree_with_their_counts() {
        let output = StatusOutput {
            last_apply: None,
            drift: Vec::new(),
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: vec![
                ModuleStatusEntry {
                    name: "tmux".to_string(),
                    packages: 1,
                    files: 1,
                    scripts: 1,
                    status: "installed".to_string(),
                    platform_skip_reason: None,
                    declared: ModuleDeclared::default(),
                },
                ModuleStatusEntry {
                    name: "nvim".to_string(),
                    packages: 3,
                    files: 12,
                    scripts: 7,
                    status: "installed".to_string(),
                    platform_skip_reason: None,
                    declared: ModuleDeclared::default(),
                },
                ModuleStatusEntry {
                    name: "git".to_string(),
                    packages: 0,
                    files: 0,
                    scripts: 0,
                    status: "installed".to_string(),
                    platform_skip_reason: None,
                    declared: ModuleDeclared::default(),
                },
            ],
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: false,
            classification_degraded_code: None,
            classification_degraded_reason: None,
            drift_checked_live: false,
            last_scan_at: None,
        };

        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_fleet_status_doc(
            &output,
            &cfgd_core::output::ConfigHeader {
                config_path: Some(std::path::Path::new("/etc/cfgd/cfgd.yaml")),
                sources: &[],
                profile: Some("default"),
                profile_inherits: &[],
                modules: &[],
            },
            &[],
            "2026-05-12T14:30:25Z",
            &Default::default(),
        ));
        drop(printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);

        assert!(
            out.contains("Synced (1 package, 1 file, 1 script)"),
            "a single package, file and script must read singular: {out}"
        );
        assert!(
            out.contains("Synced (3 packages, 12 files, 7 scripts)"),
            "many must stay plural: {out}"
        );
        assert!(
            out.contains("Synced (0 packages, 0 files, 0 scripts)"),
            "zero keeps the plural: {out}"
        );
    }

    /// When the shown state was last checked rides the Drift VERDICT it
    /// qualifies, and the report hints at `--scan` once that answer is old
    /// enough to be misleading. The threshold is the daemon's default
    /// reconcile interval: past it, the dashboard is showing something a live
    /// daemon would never have let get this stale.
    ///
    /// No separate `Last Scan` row: it sat in the header, pages above the
    /// verdict it qualifies, leaving the reader to carry a timestamp down to
    /// `No drift recorded` and decide for themselves what that verdict was
    /// worth. The `-o json` payload keeps `lastScan` either way.
    ///
    /// A run that DID scan says nothing here — its Drift section already
    /// speaks for how current the display is — which is the branch that keeps
    /// `--scan`'s own output from carrying a hint pointing back at itself.
    #[test]
    fn status_dates_the_drift_verdict_and_hints_when_it_is_stale() {
        fn header(last_scan_at: Option<&str>, checked_live: bool) -> String {
            let output = StatusOutput {
                last_apply: None,
                drift: Vec::new(),
                sources: Vec::new(),
                pending_decisions: Vec::new(),
                modules: Vec::new(),
                managed_resources: Vec::new(),
                warnings: Vec::new(),
                classification_degraded: false,
                classification_degraded_code: None,
                classification_degraded_reason: None,
                drift_checked_live: checked_live,
                last_scan_at: last_scan_at.map(str::to_string),
            };
            let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
            printer.emit(build_fleet_status_doc(
                &output,
                &cfgd_core::output::ConfigHeader {
                    config_path: Some(std::path::Path::new("/etc/cfgd/cfgd.yaml")),
                    sources: &[],
                    profile: Some("default"),
                    profile_inherits: &[],
                    modules: &[],
                },
                &[],
                // Pinned, never the wall clock: the age is a rendered value.
                "2026-05-14T10:05:00Z",
                &Default::default(),
            ));
            drop(printer);
            cfgd_core::test_helpers::captured_text(&buf)
        }

        let hint = "checks the live machine for drift";

        // Exactly at the threshold is not yet stale — `is_stale_since` is
        // "more than", so the boundary belongs to the fresh side and a daemon
        // reconciling on schedule never trips the hint.
        // The date is ON the verdict line, not in a row of its own.
        let verdict = |out: &str| {
            out.lines()
                .find(|l| l.contains("No drift recorded") || l.contains("No drift detected"))
                .unwrap_or_else(|| panic!("no drift verdict rendered: {out}"))
                .to_string()
        };

        let fresh = header(Some("2026-05-14T10:00:00Z"), false);
        assert!(
            !fresh.contains("Last Scan"),
            "the date rides the verdict, it is not a header row: {fresh}"
        );
        assert!(
            verdict(&fresh).contains("scanned 5m ago"),
            "wrong age rendered: {fresh}"
        );
        assert!(!fresh.contains(hint), "a fresh scan must not hint: {fresh}");

        let stale = header(Some("2026-05-14T08:00:00Z"), false);
        assert!(
            verdict(&stale).contains("scanned 2h ago"),
            "wrong age rendered: {stale}"
        );
        assert!(stale.contains(hint), "a stale scan must hint: {stale}");

        let never = header(None, false);
        assert!(
            verdict(&never).contains("never scanned"),
            "an unscanned host says so on the verdict: {never}"
        );
        assert!(never.contains(hint), "an unscanned host must hint: {never}");

        let scanned = header(Some("2026-05-14T08:00:00Z"), true);
        assert!(
            !scanned.contains("scanned ") && !scanned.contains(hint),
            "a run that just scanned must not date or hint at itself: {scanned}"
        );
        assert!(
            !scanned.contains("Last Scan"),
            "no scan row on any branch: {scanned}"
        );
    }

    /// An empty Drift section wears the tick only when a check stands behind
    /// it: this run scanned, or a scan is on record. `never scanned` under a
    /// green `✓` claimed a verdict nothing had produced, while `diff` refuses
    /// exactly that (`Drift undetermined`, no tick) — the two are pinned here
    /// to one role for one fact: neither paints `Ok` over an unverified one.
    #[test]
    fn no_recorded_verdict_claims_a_check_that_never_ran() {
        let tick = cfgd_core::output::Theme::default().icon_ok;
        let fleet = |last_scan_at: Option<&str>, checked_live: bool| {
            let output = StatusOutput {
                last_apply: None,
                drift: Vec::new(),
                sources: Vec::new(),
                pending_decisions: Vec::new(),
                modules: Vec::new(),
                managed_resources: Vec::new(),
                warnings: Vec::new(),
                classification_degraded: false,
                classification_degraded_code: None,
                classification_degraded_reason: None,
                drift_checked_live: checked_live,
                last_scan_at: last_scan_at.map(str::to_string),
            };
            let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
            printer.emit(build_fleet_status_doc(
                &output,
                &cfgd_core::output::ConfigHeader {
                    config_path: Some(std::path::Path::new("/etc/cfgd/cfgd.yaml")),
                    sources: &[],
                    profile: Some("default"),
                    profile_inherits: &[],
                    modules: &[],
                },
                &[],
                "2026-05-14T10:05:00Z",
                &Default::default(),
            ));
            drop(printer);
            cfgd_core::test_helpers::captured_text(&buf)
        };
        let verdict = |out: &str| {
            out.lines()
                .find(|l| l.contains("No drift"))
                .unwrap_or_else(|| panic!("no drift verdict rendered: {out}"))
                .trim()
                .to_string()
        };

        let never = verdict(&fleet(None, false));
        assert!(
            never.contains("never scanned") && !never.starts_with(&tick),
            "no check ran, so no tick: {never}"
        );
        let recorded = verdict(&fleet(Some("2026-05-14T10:00:00Z"), false));
        assert!(
            recorded.starts_with(&tick),
            "a scan on record earns the tick: {recorded}"
        );
        let live = verdict(&fleet(None, true));
        assert!(
            live.starts_with(&tick),
            "a scan this run made earns the tick: {live}"
        );

        // The per-module view holds no scan stamp, so only a live scan can
        // stand behind its empty section.
        let module = |checked_live: bool| {
            let output = ModuleStatus {
                name: "nvim".to_string(),
                packages: 0,
                files: 0,
                env: 0,
                aliases: 0,
                scripts: Vec::new(),
                system: Vec::new(),
                depends: Vec::new(),
                declared: Default::default(),
                status: "installed".to_string(),
                last_applied: None,
                scope: None,
                package_state: Vec::new(),
                deployed_files: Vec::new(),
                drift_checked_live: checked_live,
                drift: Vec::new(),
            };
            let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
            printer.emit(build_module_status_doc(
                &output,
                ModuleStatusView::Compact,
                "2026-05-14T10:05:00Z",
            ));
            drop(printer);
            verdict(&cfgd_core::test_helpers::captured_text(&buf))
        };
        assert!(
            !module(false).starts_with(&tick),
            "an unscanned module wears no tick: {}",
            module(false)
        );
        assert!(
            module(true).starts_with(&tick),
            "a scanned module earns it: {}",
            module(true)
        );

        // The sibling surface, for the same fact: a check that could not run
        // is no verdict either.
        let payload = DiffOutput {
            summary: DiffSummary {
                env_check_failed: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(super::super::diff::build_diff_doc(
            &payload,
            super::super::diff::DiffScope::Machine,
        ));
        drop(printer);
        let diff = cfgd_core::test_helpers::captured_text(&buf);
        let undetermined = diff
            .lines()
            .find(|l| l.contains("Drift undetermined"))
            .unwrap_or_else(|| panic!("diff names the gap: {diff}"))
            .trim();
        assert!(
            !undetermined.starts_with(&tick),
            "diff paints no tick over an unverified fact either: {undetermined}"
        );
    }

    /// The dating survives findings being present. With rows to state there is
    /// no verdict line to hang a detail off, and the fact still qualifies every
    /// row beneath it — a recorded dashboard whose findings carry no date is
    /// exactly the header row this change removed, minus the date.
    #[test]
    fn a_recorded_drift_finding_is_dated_by_the_same_scan_note_the_verdict_carries() {
        let output = StatusOutput {
            last_apply: None,
            drift: vec![cfgd_core::state::DriftEvent {
                id: 1,
                timestamp: "2026-05-14T08:00:00Z".to_string(),
                resource_type: "file".to_string(),
                resource_id: "~/.zshrc".to_string(),
                expected: Some("hash-desired".to_string()),
                actual: Some("hash-actual".to_string()),
                resolved_by: None,
                source: "local".to_string(),
                want: None,
                have: None,
            }],
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: Vec::new(),
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: false,
            classification_degraded_code: None,
            classification_degraded_reason: None,
            drift_checked_live: false,
            last_scan_at: Some("2026-05-14T08:00:00Z".to_string()),
        };

        let out = dashboard(&output);
        assert!(
            out.contains("scanned 2h ago"),
            "findings are dated too, not only the empty verdict: {out}"
        );
        assert!(
            !out.contains("Last Scan"),
            "the date never returns to a header row: {out}"
        );

        // A live scan speaks for its own currency on both branches.
        let live = StatusOutput {
            drift_checked_live: true,
            ..output
        };
        let live_out = dashboard(&live);
        assert!(
            !live_out.contains("scanned "),
            "a run that just scanned does not date its own findings"
        );
        assert!(
            live_out.contains(&super::heal_drift_hint(None)),
            "a live drift verdict offers the machine-wide heal: {live_out}"
        );
        assert!(
            !out.contains(&super::heal_drift_hint(None)),
            "a recorded finding is not healed on the strength of an old scan: {out}"
        );
    }

    /// The whole dashboard for a `StatusOutput`, rendered the way `cmd_status`
    /// renders it. Every clock-reading input is supplied, so a render pins.
    fn dashboard(output: &StatusOutput) -> String {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_fleet_status_doc(
            output,
            &cfgd_core::output::ConfigHeader {
                config_path: Some(std::path::Path::new("/etc/cfgd/cfgd.yaml")),
                sources: &[],
                profile: Some("default"),
                profile_inherits: &[],
                modules: &[],
            },
            &[],
            "2026-05-14T10:05:00Z",
            &Default::default(),
        ));
        drop(printer);
        cfgd_core::test_helpers::captured_text(&buf)
    }

    fn empty_output() -> StatusOutput {
        StatusOutput {
            last_apply: None,
            drift: Vec::new(),
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: Vec::new(),
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: false,
            classification_degraded_code: None,
            classification_degraded_reason: None,
            drift_checked_live: false,
            // Old enough for the scan hint to be earned; the fresh case sets
            // its own stamp.
            last_scan_at: Some("2026-05-14T08:00:00Z".to_string()),
        }
    }

    /// The Summary row is a sentence. It read the stored wire shape verbatim:
    /// `Summary  {"failed":0,"succeeded":22,"total":22}`.
    #[test]
    fn the_summary_row_never_shows_a_stored_wire_shape() {
        let mut output = empty_output();
        output.last_apply = Some(ApplyRecord {
            id: 4,
            timestamp: "2026-05-14T09:00:00Z".to_string(),
            profile: "default".to_string(),
            plan_hash: "deadbeef".to_string(),
            status: ApplyStatus::Success,
            summary: Some(
                cfgd_core::state::ApplySummary::Actions {
                    total: 22,
                    succeeded: 21,
                    skipped: 1,
                    failed: 0,
                    not_attempted: 0,
                    not_run: None,
                    aborted: false,
                }
                .to_column(),
            ),
        });
        let out = dashboard(&output);
        assert!(
            out.contains("21 succeeded, 1 skipped"),
            "the Summary row must read as prose: {out}"
        );
        assert!(
            !out.contains("{\"") && !out.contains("\"succeeded\""),
            "a stored wire shape reached a human surface: {out}"
        );
    }

    /// One screen, one hint for one need. The Drift section stated the absence
    /// AND pointed at `--scan`, and the header pointed at it again; the report
    /// now states the absence plainly and closes with the one hint, naming the
    /// one command that checks the machine, and only while the recorded state
    /// is old enough for it to matter.
    #[test]
    fn the_scan_hint_is_said_once_and_last() {
        let out = dashboard(&empty_output());
        assert!(
            out.contains("No drift recorded"),
            "the drift line states the absence plainly: {out}"
        );
        let hint_lines: Vec<&str> = out.lines().filter(|l| l.contains("cfgd diff")).collect();
        assert_eq!(hint_lines.len(), 1, "one hint, one need: {out}");
        let last = out
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or_default();
        assert!(
            last.contains("cfgd diff"),
            "a next step closes the report: {out}"
        );

        // A host whose recorded state is current has no need to be told, so
        // the report closes on its own content.
        let mut fresh = empty_output();
        fresh.last_scan_at = Some("2026-05-14T10:04:00Z".to_string());
        assert!(
            !dashboard(&fresh).contains("cfgd diff"),
            "a fresh scan needs no hint"
        );
    }

    /// The Modules headline and the Managed Resources table answer the same
    /// question and must give the same number: the headline said `28 packages`
    /// over a table listing 24, and later omitted the module's scripts while
    /// the table two lines below listed seven of them.
    #[test]
    fn the_module_headline_counts_what_the_table_lists() {
        let resources = vec![
            recorded("module", "nvim:packages:ripgrep,fd,bat"),
            recorded("module", "nvim:packages:neovim"),
            recorded("module", "nvim:files:6"),
            recorded("module", "nvim:script"),
        ];
        let declared = ModuleDeclared {
            script_summary: Some("postApply (7 scripts)".to_string()),
            scripts: 7,
            ..ModuleDeclared::default()
        };
        let declared_map =
            std::collections::BTreeMap::from([("nvim".to_string(), declared.clone())]);
        let tallies = recorded_module_tallies(&resources, &declared_map);
        let tally = tallies.get("nvim").copied().unwrap_or_default();

        let mut output = empty_output();
        output.modules = vec![ModuleStatusEntry {
            name: "nvim".to_string(),
            packages: tally.packages,
            files: tally.files,
            scripts: tally.scripts,
            status: "installed".to_string(),
            platform_skip_reason: None,
            declared,
        }];
        output.managed_resources = resources.clone();
        let out = dashboard(&output);

        let rows = managed_resource_rows(&resources, &output.modules, Some("base"));
        let listed: usize = rows
            .iter()
            .filter(|row| row[0] == "package")
            .map(|row| {
                row[2]
                    .rsplit(": ")
                    .next()
                    .map_or(0, |names| names.split(", ").count())
            })
            .sum();
        assert_eq!(tally.packages, listed, "headline vs table: {out}");
        assert!(
            rows.iter()
                .any(|row| row[0] == "script" && row[2] == "postApply (7 scripts)"),
            "the table lists the scripts the headline must name: {rows:?}"
        );
        assert!(
            out.contains("4 packages, 6 files, 7 scripts"),
            "the headline reports the recorded tally: {out}"
        );
    }

    /// Every kind the Managed Resources table can call a module's has a slot in
    /// the headline three lines above it.
    ///
    /// The population is read off `display_type`'s own arms — the fn that folds
    /// a recorded token onto the Type word — so a kind reaching that column
    /// cannot skip this walk: the words it FOLDS are exactly the module-owned
    /// ones (`env` and every cfgd-owned token fall through its `other` arm and
    /// belong to no module). The headline dropped the `script` rows for as long
    /// as its tally was an unnamed pair, which is why the slot is proven by
    /// rendering rather than by counting fields.
    #[test]
    fn every_module_owned_kind_the_table_lists_has_a_slot_in_the_headline() {
        let words = folded_type_column_words();
        assert!(
            words.len() >= 3,
            "the walk no longer reaches `display_type`'s arms: {words:?}"
        );
        for word in &words {
            // The recorded id one row of this surface is stored under, in the
            // shape `action_resource_info` mints, standing for exactly one thing.
            let (id, declared) = match word.as_str() {
                "package" => ("nvim:packages:neovim", ModuleDeclared::default()),
                "file" => ("nvim:files:1", ModuleDeclared::default()),
                "script" => (
                    "nvim:script",
                    ModuleDeclared {
                        script_summary: Some("postApply (1 script)".to_string()),
                        scripts: 1,
                        ..ModuleDeclared::default()
                    },
                ),
                other => panic!(
                    "the Type column prints {other:?}, which this walk cannot record — \
                     give it a recorded id here and a slot in `ModuleTally`"
                ),
            };
            let resources = vec![recorded("module", id)];
            let declared_map =
                std::collections::BTreeMap::from([("nvim".to_string(), declared.clone())]);
            let tally = recorded_module_tallies(&resources, &declared_map)
                .get("nvim")
                .copied()
                .unwrap_or_default();

            let mut output = empty_output();
            output.modules = vec![ModuleStatusEntry {
                name: "nvim".to_string(),
                packages: tally.packages,
                files: tally.files,
                scripts: tally.scripts,
                status: "installed".to_string(),
                platform_skip_reason: None,
                declared,
            }];
            output.managed_resources = resources;
            let out = dashboard(&output);
            // The headline alone: the table's own cell for this row names the
            // same count, and an assertion over the whole report would pass on
            // the row while the line above it stayed silent.
            let headline = out
                .split("Managed Resources")
                .next()
                .and_then(|head| head.lines().find(|l| l.contains("module:nvim")))
                .unwrap_or_default();
            assert!(
                headline.contains(&format!("1 {word}")),
                "a module recording only {word} rows renders a headline that never names them: {out}"
            );
        }
    }

    /// The Type words `display_type` FOLDS a recorded token onto — its match
    /// arms read off this file, so the walk above sees a kind added there.
    fn folded_type_column_words() -> Vec<String> {
        let source = include_str!("status.rs");
        let start = source
            .find("fn display_type(")
            .expect("the Type column's mapping fn");
        let body = &source[start..];
        let end = body.find("\n}\n").expect("the fn's closing brace");
        let mut words: Vec<String> = string_literals(&body[..end])
            .into_iter()
            .map(|token| display_type(&token))
            .collect();
        words.sort();
        words.dedup();
        words
    }

    /// The double-quoted literals in `body`, which carry no escapes here.
    fn string_literals(body: &str) -> Vec<String> {
        body.split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    /// A packages row names the manager that installs it. One row rendered
    /// bare beside `apt:`, `cargo:`, `npm:` and `pipx:` siblings, because the
    /// declaration map held one manager per NAME and a package declared twice
    /// (natively and under npm) collapsed onto whichever won.
    #[test]
    fn every_packages_row_spells_its_manager() {
        for manager in cfgd_core::config::ALL_MANAGER_NAMES {
            let declared = ModuleDeclared {
                package_managers: declared_managers(&[("thing", manager)]),
                ..ModuleDeclared::default()
            };
            let rows = managed_resource_rows(
                &[recorded("module", "nvim:packages:thing")],
                &[nvim_entry(declared)],
                Some("base"),
            );
            assert_eq!(
                rows[0][2],
                format!("{manager}: thing"),
                "a {manager} row lost its prefix"
            );
        }

        // The name declared under TWO managers still names the manager of the
        // row it is in: the recorded row is per manager, so every name in one
        // row shares an installer.
        let both = ModuleDeclared {
            package_managers: declared_managers(&[("neovim", "apt"), ("neovim", "npm")]),
            ..ModuleDeclared::default()
        };
        let rows = managed_resource_rows(
            &[recorded("module", "nvim:packages:neovim")],
            &[nvim_entry(both)],
            Some("base"),
        );
        assert_eq!(
            rows[0][2], "neovim",
            "two managers claim it, so no row may claim one of them"
        );
    }

    /// A degraded classification must be visible IN the `-o json` payload:
    /// the human warning is suppressed under structured output, so without
    /// these fields a broken classification is indistinguishable from a clean
    /// machine with nothing pending.
    #[test]
    fn status_json_degraded_classification_is_structural() {
        let output = StatusOutput {
            last_apply: None,
            drift: Vec::new(),
            sources: Vec::new(),
            pending_decisions: Vec::new(),
            modules: Vec::new(),
            managed_resources: Vec::new(),
            warnings: Vec::new(),
            classification_degraded: true,
            classification_degraded_code: Some(
                crate::cli::output_types::ClassificationDegradedCode::SourceUnreadable,
            ),
            classification_degraded_reason: Some(
                "source 'acme': cached config is unreadable".to_string(),
            ),
            drift_checked_live: false,
            last_scan_at: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["classificationDegraded"], serde_json::json!(true));
        assert_eq!(
            json["classificationDegradedCode"],
            serde_json::json!("sourceUnreadable"),
            "the code is the closed, camelCase machine token"
        );
        assert_eq!(
            json["classificationDegradedReason"],
            serde_json::json!("source 'acme': cached config is unreadable")
        );
    }

    // Minimal config + default profile YAML used by every test that exercises
    // the load_config_and_profile path. The active profile must materialize as
    // a profile file under `profiles/` for resolve_profile to succeed.
    const CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                               kind: Config\n\
                               metadata:\n  name: t\n\
                               spec:\n  profile: default\n";

    const PROFILE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                kind: Profile\n\
                                metadata:\n  name: default\n\
                                spec: {}\n";

    /// Profile that references `test-mod`; used by tests that exercise the
    /// per-module rendering and structured output paths.
    const PROFILE_WITH_MODULE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                            kind: Profile\n\
                                            metadata:\n  name: default\n\
                                            spec:\n  modules:\n    - test-mod\n";

    const MODULE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                               kind: Module\n\
                               metadata:\n  name: test-mod\n\
                               spec:\n  packages:\n    - name: ripgrep\n";

    fn test_cli_for(config_path: std::path::PathBuf, state_dir: &std::path::Path) -> Cli {
        Cli {
            config: config_path,
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            no_hints: false,
            theme: None,
            jsonpath: None,
            yes: false,
            state_dir: Some(state_dir.to_path_buf()),
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    fn test_printers() -> (Printer, std::sync::Arc<std::sync::Mutex<String>>) {
        Printer::for_test_at(Verbosity::Normal)
    }

    fn test_printers_json() -> (Printer, std::sync::Arc<std::sync::Mutex<String>>) {
        Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json)
    }

    /// Isolated config-dir + state-dir pair with a minimal valid `cfgd.yaml`
    /// and matching `profiles/default.yaml`.
    fn setup_env() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_YAML).unwrap();
        std::fs::create_dir_all(config_dir.path().join("modules")).unwrap();
        (config_dir, state_dir, config_path)
    }

    /// Same as `setup_env` but the default profile references `test-mod` and
    /// the corresponding `modules/test-mod/module.yaml` is materialized.
    fn setup_env_with_module() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), MODULE_YAML).unwrap();
        (config_dir, state_dir, config_path)
    }

    /// Isolated config-dir + state-dir pair whose active profile `base`
    /// inherits `core`, which inherits `shared` — the real resolution path
    /// `cmd_status` drives, not a synthesized `ResolvedProfile`.
    fn setup_env_with_inheriting_profile()
    -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        const CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                   kind: Config\n\
                                   metadata:\n  name: t\n\
                                   spec:\n  profile: base\n";
        const SHARED_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                   kind: Profile\n\
                                   metadata:\n  name: shared\n\
                                   spec:\n  inherits: []\n";
        const CORE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                 kind: Profile\n\
                                 metadata:\n  name: core\n\
                                 spec:\n  inherits:\n    - shared\n";
        const BASE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\n\
                                 kind: Profile\n\
                                 metadata:\n  name: base\n\
                                 spec:\n  inherits:\n    - core\n";

        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("shared.yaml"), SHARED_YAML).unwrap();
        std::fs::write(profiles_dir.join("core.yaml"), CORE_YAML).unwrap();
        std::fs::write(profiles_dir.join("base.yaml"), BASE_YAML).unwrap();
        std::fs::create_dir_all(config_dir.path().join("modules")).unwrap();
        (config_dir, state_dir, config_path)
    }

    // --- cmd_status (aggregate) -------------------------------------------

    /// End-to-end proof that a real active profile's resolved inheritance
    /// chain reaches the rendered header: `base` inherits `core` inherits
    /// `shared`, driven through `cmd_status`'s own resolution, not a
    /// synthesized `ResolvedProfile`. This is the pin that fails if any
    /// `ConfigHeader` call site threads an empty chain instead of
    /// `resolved.inherits_chain()`, or if the chain's order inverts.
    #[test]
    fn cmd_status_renders_the_resolved_inheritance_chain() {
        let (_cfg_dir, state_dir, config_path) = setup_env_with_inheriting_profile();
        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("(inherits: core → shared)"),
            "expected the Profile row annotated with its nearest-parent-first \
             chain, got: {output}"
        );
    }

    /// `--show-values` names items, and only the itemized view has rows to
    /// name them on — so it selects that view without `-o wide` being asked
    /// for, and renders the declared value beside the name.
    #[test]
    fn show_values_selects_the_itemized_view_without_wide() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (config_dir, state_dir, config_path) = setup_env_with_module();
        std::fs::write(
            config_dir
                .path()
                .join("modules")
                .join("test-mod")
                .join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  env:\n    - name: EDITOR\n      value: nvim\n",
        )
        .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status(&cli, &printer, Some("test-mod"), false, false, true).unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("\nShell\n")
                && out.contains("\n  Env\n")
                && out.contains(r#"EDITOR="nvim""#),
            "--show-values must itemize env under Shell and show the declared \
             value: {out}"
        );
    }

    /// `-o wide` reaches the same view through the global output flag, with no
    /// values shown.
    #[test]
    fn wide_output_selects_the_itemized_view() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_config_dir, state_dir, config_path) = setup_env_with_module();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = super::OutputFormatArg(cfgd_core::output::OutputFormat::Wide);
        let (printer, cap) =
            Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Wide);
        cmd_status(&cli, &printer, Some("test-mod"), false, false, false).unwrap();
        drop(printer);

        let out = cap.human();
        assert!(
            out.contains("\nInstalled Packages\n") && out.contains("ripgrep"),
            "-o wide must itemize the declared packages: {out}"
        );
        assert!(
            !out.contains("Packages      1"),
            "the itemized view replaces the counts rather than adding to them: {out}"
        );
    }

    #[test]
    fn cmd_status_missing_config_returns_err() {
        let state_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_for(dir.path().join("nope.yaml"), state_dir.path());
        let (printer, _) = test_printers();

        let err = cmd_status(&cli, &printer, None, false, false, false).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("not found") || msg.contains("nope.yaml"),
            "expected config-not-found error, got: {err}"
        );
    }

    #[test]
    fn cmd_status_empty_state_renders_no_applies_and_no_drift() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Status"),
            "should render Status heading, got: {output}"
        );
        assert!(
            output.contains("No applies recorded yet"),
            "empty applies state should render info line, got: {output}"
        );
        assert!(
            output.contains("No drift recorded"),
            "an empty recorded dashboard says what it read, got: {output}"
        );
    }

    #[test]
    fn cmd_status_with_apply_record_prints_last_apply_block() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .record_apply(
                "default",
                "deadbeef",
                ApplyStatus::Success,
                Some("test apply summary"),
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Last Apply"),
            "should render Last Apply section, got: {output}"
        );
        assert!(
            output.contains("default"),
            "should print profile, got: {output}"
        );
        assert!(
            output.contains("Success"),
            "should print success status, got: {output}"
        );
        assert!(
            output.contains("test apply summary"),
            "should include summary text, got: {output}"
        );
    }

    #[test]
    fn cmd_status_drift_present_renders_warning_line() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .record_drift(
                "file",
                "/etc/hosts",
                Some("desired-hash"),
                Some("actual-hash"),
                "local",
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            !output.contains("No drift detected"),
            "drift recorded — should NOT print all-clear line, got: {output}"
        );
        assert!(
            output.contains("file") && output.contains("/etc/hosts"),
            "drift event should appear in output, got: {output}"
        );
        assert!(
            output.contains("desired-hash") && output.contains("actual-hash"),
            "drift line should include want/have values, got: {output}"
        );
    }

    #[test]
    fn cmd_status_drift_non_local_source_includes_source_tag() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .record_drift(
                "package",
                "ripgrep",
                Some("1.0"),
                Some("0.9"),
                "team-config",
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        // The label is appended only when source != "local", and it carries the
        // owner token so the attribution reads the same here as it does over a
        // `cfgd sync` group.
        assert!(
            output.contains("source:team-config"),
            "non-local drift should carry the source owner token, got: {output}"
        );
    }

    #[test]
    fn cmd_status_managed_resources_renders_table() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .upsert_managed_resource("file", "/etc/managed.conf", "local", Some("hashval"), None)
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Managed Resources"),
            "should print Managed Resources section, got: {output}"
        );
        assert!(
            output.contains("/etc/managed.conf"),
            "managed resource row should be present, got: {output}"
        );
    }

    // onChange scripts persist under resource_type
    // "Running script" (execute_script's own return value), distinct from
    // the main pre/post-apply phase scripts' "script" type
    // (apply_script_action's return value). Both must condense for human
    // display; the stored/JSON id must stay the raw multi-line body.
    #[test]
    fn cmd_status_running_script_managed_resource_condenses_for_human_display() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let raw_body = " echo one\necho two\necho three";
        store
            .upsert_managed_resource("Running script", raw_body, "local", None, None)
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            !output.contains("echo two"),
            "human table cell must not leak the raw multi-line body: {output}"
        );
        assert!(
            output.contains("echo one"),
            "condensed label should reference the first line: {output}"
        );
    }

    #[test]
    fn cmd_status_running_script_json_preserves_raw_resource_id() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let raw_body = " echo one\necho two\necho three";
        store
            .upsert_managed_resource("Running script", raw_body, "local", None, None)
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let resources = parsed["managedResources"].as_array().unwrap();
        assert_eq!(
            resources[0]["resourceId"], raw_body,
            "JSON payload must preserve the raw multi-line resource_id byte-identical, got: {output}"
        );
    }

    #[test]
    fn cmd_status_exit_code_false_with_drift_returns_ok() {
        // Guard: when --exit-code is not set, drift presence must NOT trigger
        // process::exit. Only the non-exiting half is testable in-process; the
        // drift-present branch would terminate the test runner via process::exit.
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .record_drift("file", "/etc/x", Some("a"), Some("b"), "local")
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, _) = test_printers();

        let res = cmd_status(&cli, &printer, None, false, false, false);
        assert!(res.is_ok(), "exit_code=false must return Ok, got: {res:?}");
    }

    #[test]
    fn cmd_status_exit_code_true_no_drift_returns_ok() {
        // Complement to the test above: with `exit_code=true` but a clean host,
        // the live-scan gate finds no drift, so the function must not call
        // `process::exit` and must return Ok.
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, _) = test_printers();

        let res = cmd_status(&cli, &printer, None, true, true, false);
        assert!(
            res.is_ok(),
            "exit_code=true with no drift must return Ok, got: {res:?}"
        );
    }

    /// A scan the store refuses to record must leave `lastScanAt` naming the
    /// stamp the row still holds, never the one this run tried to write.
    ///
    /// The store half of that contract is pinned in cfgd-core; the harm lives
    /// here, on the payload: a stamp no row holds reports the machine as
    /// scanned more recently than anything can prove, and the next run that
    /// reads the row sends the dashboard backwards.
    #[test]
    fn cmd_status_scan_keeps_the_recorded_stamp_when_the_write_is_refused() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let frozen = "2000-01-01T00:00:00Z";
        {
            let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
            cfgd_core::test_helpers::freeze_last_scan_at(&store, frozen).unwrap();
        }

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false, true, false).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(
            parsed["driftCheckedLive"], true,
            "`--scan` ran the live scan, so the payload must say so: {parsed}"
        );
        assert_eq!(
            parsed["lastScanAt"], frozen,
            "a refused write must leave the stored stamp standing: {parsed}"
        );
    }

    /// WARN regression (re-review of the QP13 fix round): `cfgd status
    /// --scan`'s live-drift display must show a drifted env-var/alias's real
    /// declared line, not the opaque `current`/`missing or changed` markers
    /// `verify_env_items` persists. `drift_event_from` (`live_drift.rs`)
    /// shapes every live-scan finding into the exact `StatusOutput.drift`
    /// vec this test reads back from `-o json`, and `render_drift_section`
    /// renders the same `DriftEvent.expected` string to the human terminal —
    /// so recomputing at that one shaping point fixes both surfaces. This is
    /// the sibling of `cmd_diff_reports_no_env_drift_when_bootstrap_path_dirs_are_recorded`'s
    /// fix, on `status`'s parallel live-scan path rather than `diff`'s.
    #[test]
    fn cmd_status_scan_shows_the_declared_env_value_not_the_opaque_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("modules")).unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // Header only, no `EDITOR` line — the per-item check reports the
        // declared var as drifted (opaque "missing or changed" before this
        // fix; the real declared line after it).
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            "# managed by cfgd \u{2014} do not edit\n",
        )
        .unwrap();
        // The declared line's dialect is platform-dependent (bash `export`
        // vs PowerShell `$env:`), so the expected needle is derived from
        // `env_item_declared_line` — production's own per-item renderer for
        // the running platform — rather than a hardcoded POSIX literal.
        let declared_env = vec![cfgd_core::config::EnvVar {
            name: "EDITOR".to_string(),
            value: "vim".to_string(),
            platforms: vec![],
        }];
        // The owners the profile-layer merge records for this profile: the
        // generated line names its layer, so a needle rendered with no owner
        // is a line the file never holds.
        let declared_owners = {
            let mut o = cfgd_core::config::EntryOwners::default();
            o.claim("profile:default", &declared_env, &[]);
            o
        };
        let declared_line = cfgd_core::reconciler::MergedEnvItems::new(
            &declared_env,
            &[],
            &declared_owners,
            &[],
            &[],
        )
        .declared_line("env-var", "EDITOR")
        .expect("EDITOR renders a declared line");

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let mut cli = test_cli_for(config_path, &state_dir);
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false, true, false).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        let drift = parsed["drift"].as_array().expect("drift array");
        let editor_row = drift
            .iter()
            .find(|d| d["resourceType"] == "env-var" && d["resourceId"] == "EDITOR")
            .unwrap_or_else(|| panic!("expected an EDITOR env-var drift row: {parsed}"));
        assert_eq!(
            editor_row["expected"],
            serde_json::json!(declared_line),
            "the -o json payload must carry the declared line, not the opaque \
             marker: {editor_row}"
        );
        assert_ne!(
            editor_row["expected"],
            serde_json::json!("current"),
            "must not regress to the opaque marker: {editor_row}"
        );

        // The human render shares the same `DriftEvent`, so the fix must show
        // up there too — assert its content directly rather than trusting the
        // JSON assertion above to stand in for it.
        let (human_printer, human_buf) = test_printers();
        cmd_status(&cli, &human_printer, None, false, true, false).unwrap();
        drop(human_printer);
        let human = cfgd_core::test_helpers::captured_text(&human_buf);
        let editor_line = human
            .lines()
            .find(|l| l.contains("env: EDITOR"))
            .unwrap_or_else(|| panic!("expected an EDITOR env drift line, got: {human}"));
        assert!(
            editor_line.contains(&declared_line),
            "the human render must show the declared line, got: {editor_line}"
        );
        assert!(
            !editor_line.contains("want: current"),
            "the EDITOR row must not show the opaque marker, got: {editor_line}"
        );
        assert!(
            editor_line.contains(&cfgd_core::output::drift_detail(
                &declared_line,
                cfgd_core::Absence::Missing.as_str()
            )),
            "both operands must be real: the declared line against the absence \
             the file reports, got: {editor_line}"
        );
    }

    /// Plain `cfgd status` renders RECORDED rows and `--scan` renders live
    /// ones, and both used to word the same drifted env var differently: the
    /// scan recomputed the declared line while the dashboard printed the
    /// stored `want: current, have: missing or changed`, which names no value
    /// at all. Seeds a recorded row carrying exactly the opaque markers
    /// production persists, then pins that both modes render the same line.
    #[test]
    fn plain_status_words_a_recorded_env_row_the_same_way_a_scan_does() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("modules")).unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            "# managed by cfgd \u{2014} do not edit\n",
        )
        .unwrap();

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        {
            let state = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
            state
                .record_drift(
                    "env-var",
                    "EDITOR",
                    Some("current"),
                    Some("missing or changed"),
                    cfgd_core::config::LOCAL_LAYER,
                )
                .unwrap();
        }

        let declared_env = vec![cfgd_core::config::EnvVar {
            name: "EDITOR".to_string(),
            value: "vim".to_string(),
            platforms: vec![],
        }];
        // The owners the profile-layer merge records for this profile: the
        // generated line names its layer, so a needle rendered with no owner
        // is a line the file never holds.
        let declared_owners = {
            let mut o = cfgd_core::config::EntryOwners::default();
            o.claim("profile:default", &declared_env, &[]);
            o
        };
        let declared_line = cfgd_core::reconciler::MergedEnvItems::new(
            &declared_env,
            &[],
            &declared_owners,
            &[],
            &[],
        )
        .declared_line("env-var", "EDITOR")
        .expect("EDITOR renders a declared line");
        let expected_detail =
            cfgd_core::output::drift_detail(&declared_line, cfgd_core::Absence::Missing.as_str());

        let mut cli = test_cli_for(config_path, &state_dir);
        for scan in [false, true] {
            let (printer, buf) = test_printers();
            cmd_status(&cli, &printer, None, false, scan, false).unwrap();
            drop(printer);
            let human = cfgd_core::test_helpers::captured_text(&buf);
            let editor_line = human
                .lines()
                .find(|l| l.contains("env: EDITOR"))
                .unwrap_or_else(|| panic!("expected an EDITOR env drift line, got: {human}"));
            assert!(
                editor_line.contains(&expected_detail),
                "scan={scan} must render the same real operands, got: {editor_line}"
            );
        }

        // The recompute is a DISPLAY truth. The payload's `expected`/`actual`
        // stay the bytes the row was stored with — a keyed record describes
        // its own row — and the recompute rides the additive pair beside them.
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();
        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);
        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        let row = parsed["drift"]
            .as_array()
            .and_then(|d| {
                d.iter()
                    .find(|d| d["resourceType"] == "env-var" && d["resourceId"] == "EDITOR")
            })
            .unwrap_or_else(|| panic!("expected a recorded EDITOR row: {parsed}"));
        assert_eq!(
            (&row["expected"], &row["actual"]),
            (
                &serde_json::json!("current"),
                &serde_json::json!("missing or changed")
            ),
            "the stored operands describe the stored row and must not move: {row}"
        );
        assert_eq!(
            (&row["want"], &row["have"]),
            (
                &serde_json::json!(declared_line),
                &serde_json::json!(cfgd_core::Absence::Missing.as_str())
            ),
            "the recompute rides its own additive pair: {row}"
        );
    }

    /// A recorded env row the machine has since converged must leave no trace.
    ///
    /// The recompute reads the machine, so a row whose declared line IS the
    /// line the file holds has healed since it was recorded — rendering it
    /// puts `want: X, have: X` under a warning glyph, a finding that refutes
    /// itself. Dropped from the display on BOTH modes: plain `status` reads
    /// the recorded row and `--scan` renders it beside the live findings, so
    /// a fix on one surface alone leaves the other still self-refuting.
    #[test]
    fn a_recorded_env_row_the_machine_has_since_converged_is_not_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("modules")).unwrap();

        let declared_env = vec![cfgd_core::config::EnvVar {
            name: "EDITOR".to_string(),
            value: "vim".to_string(),
            platforms: vec![],
        }];
        // The owners the profile-layer merge records for this profile: the
        // generated line names its layer, so a needle rendered with no owner
        // is a line the file never holds.
        let declared_owners = {
            let mut o = cfgd_core::config::EntryOwners::default();
            o.claim("profile:default", &declared_env, &[]);
            o
        };
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let declared_line = cfgd_core::reconciler::MergedEnvItems::new(
            &declared_env,
            &[],
            &declared_owners,
            &[],
            &[],
        )
        .declared_line("env-var", "EDITOR")
        .expect("EDITOR renders a declared line");
        // The machine HOLDS the declared line: whatever the recorded row says,
        // this entry is converged right now.
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            format!("# managed by cfgd \u{2014} do not edit\n{declared_line}\n"),
        )
        .unwrap();

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        {
            let state = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
            state
                .record_drift(
                    "env-var",
                    "EDITOR",
                    Some("current"),
                    Some("missing or changed"),
                    cfgd_core::config::LOCAL_LAYER,
                )
                .unwrap();
        }

        let cli = test_cli_for(config_path.clone(), &state_dir);
        for scan in [false, true] {
            let (printer, buf) = test_printers();
            cmd_status(&cli, &printer, None, false, scan, false).unwrap();
            drop(printer);
            let human = cfgd_core::test_helpers::captured_text(&buf);
            assert!(
                !human.contains("env: EDITOR"),
                "scan={scan} must not report a converged entry, got: {human}"
            );
            assert!(
                !human.contains(&cfgd_core::output::drift_detail(
                    &declared_line,
                    &declared_line
                )),
                "scan={scan} must never render a row that refutes itself, got: {human}"
            );
        }

        // The drop is a payload fact, not a display trim: the vector the
        // recompute filters IS `drift[]`, so a machine consumer must read the
        // same converged verdict the human row shows.
        for scan in [false, true] {
            let mut cli = test_cli_for(config_path.clone(), &state_dir);
            cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
            let (printer, buf) = test_printers_json();
            cmd_status(&cli, &printer, None, false, scan, false).unwrap();
            drop(printer);
            let captured = cfgd_core::test_helpers::captured_text(&buf);
            let parsed: serde_json::Value = serde_json::from_str(captured.trim())
                .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
            let drift = parsed["drift"].as_array().expect("drift array");
            assert!(
                !drift
                    .iter()
                    .any(|e| e["resourceType"] == "env-var" && e["resourceId"] == "EDITOR"),
                "scan={scan}: a healed row must leave drift[] with the human row, got: {parsed}"
            );
        }
    }

    /// A recorded row and a live scan reporting the SAME entry render once.
    ///
    /// The env recompute keeps a recorded row that is still drifting, and a
    /// `--scan` of the same machine then finds the same entry and words it
    /// identically — without the replacement the report rendered one drift as
    /// two rows and `drift[]` carried it twice. The scan is a FULL-machine
    /// check, so a recorded row it did not re-find (here a package this
    /// profile never declared, in the live check's own `<manager>:<name>`
    /// grammar — a bare name would be a daemon spelling the scan keeps)
    /// resolves as healed and leaves both the display and the record.
    #[test]
    fn a_recorded_row_the_scan_also_reports_renders_once() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Interactive\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("modules")).unwrap();

        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        // The managed env file exists but holds no EDITOR line: the entry is
        // genuinely drifting, so the recompute keeps the recorded row and the
        // live scan reports the same key.
        std::fs::write(
            cfgd_core::reconciler::primary_env_file(tmp_home.path()),
            "# managed by cfgd \u{2014} do not edit\n",
        )
        .unwrap();

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        {
            let state = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
            state
                .record_drift(
                    "env-var",
                    "EDITOR",
                    Some("current"),
                    Some("missing or changed"),
                    cfgd_core::config::LOCAL_LAYER,
                )
                .unwrap();
            state
                .record_drift(
                    "package",
                    "brew:ghost-tool",
                    Some("installed"),
                    Some("not installed"),
                    cfgd_core::config::LOCAL_LAYER,
                )
                .unwrap();
        }

        let cli = test_cli_for(config_path.clone(), &state_dir);
        let (printer, buf) = test_printers();
        cmd_status(&cli, &printer, None, false, true, false).unwrap();
        drop(printer);
        let human = cfgd_core::test_helpers::captured_text(&buf);
        assert_eq!(
            human.matches("env: EDITOR").count(),
            1,
            "one drift must render as one row, got: {human}"
        );

        let mut cli = test_cli_for(config_path, &state_dir);
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();
        cmd_status(&cli, &printer, None, false, true, false).unwrap();
        drop(printer);
        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        let drift = parsed["drift"].as_array().expect("drift array");
        assert_eq!(
            drift
                .iter()
                .filter(|e| e["resourceType"] == "env-var" && e["resourceId"] == "EDITOR")
                .count(),
            1,
            "drift[] must carry one entry for one drift, got: {parsed}"
        );
        assert!(
            !drift
                .iter()
                .any(|e| e["resourceType"] == "package" && e["resourceId"] == "brew:ghost-tool"),
            "a recorded row the full scan did not re-find has healed and must not render, got: {parsed}"
        );
        let store = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
        assert!(
            !store
                .unresolved_drift()
                .unwrap()
                .iter()
                .any(|e| e.resource_id == "brew:ghost-tool"),
            "the full scan must resolve the recorded row it did not re-find"
        );
    }

    #[test]
    fn cmd_status_json_output_emits_expected_shape() {
        let (_cfg_dir, state_dir, config_path) = setup_env();
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .record_apply("default", "abc123", ApplyStatus::Success, Some("ok"))
            .unwrap();
        store
            .record_drift("file", "/etc/foo", Some("want"), Some("have"), "local")
            .unwrap();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert!(
            parsed["lastApply"].is_object(),
            "lastApply should be an object, got: {parsed}"
        );
        assert_eq!(parsed["lastApply"]["profile"], "default");
        let drift = parsed["drift"].as_array().expect("drift array");
        assert_eq!(drift.len(), 1, "expected 1 drift entry, got: {parsed}");
        assert_eq!(drift[0]["resourceType"], "file");
        assert_eq!(drift[0]["resourceId"], "/etc/foo");
        // Empty arrays should still be present (not omitted).
        assert!(parsed["sources"].is_array());
        assert!(parsed["pendingDecisions"].is_array());
        assert!(parsed["modules"].is_array());
        assert!(parsed["managedResources"].is_array());
    }

    #[test]
    fn cmd_status_module_filter_routes_to_per_module_path() {
        // When `module_filter` is Some, cmd_status delegates to
        // cmd_status_module — the aggregate "Status" heading must NOT appear.
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status(&cli, &printer, Some("test-mod"), false, false, false).unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        // Per-module heading is "Status: <name>" — must be present.
        assert!(
            output.contains("Status: test-mod"),
            "should route to per-module heading, got: {output}"
        );
        // Aggregate-only sections (no apply record was made → 'No applies'
        // would have appeared in the main path) must NOT appear.
        assert!(
            !output.contains("No applies recorded yet"),
            "should not fall through to aggregate path, got: {output}"
        );
    }

    // --- cmd_status_module ------------------------------------------------

    #[test]
    fn cmd_status_module_unknown_module_table_prints_not_found() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "ghost",
            false,
            false,
            ModuleStatusView::Compact,
        )
        .unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Status: ghost"),
            "should print module heading, got: {output}"
        );
        assert!(
            output.contains("not found"),
            "unknown module should print not-found info, got: {output}"
        );
    }

    #[test]
    fn cmd_status_module_unknown_module_json_emits_not_found_shape() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "ghost",
            false,
            false,
            ModuleStatusView::Compact,
        )
        .unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(parsed["name"], "ghost");
        assert_eq!(parsed["status"], "not found");
        assert_eq!(parsed["packages"], 0);
        assert_eq!(parsed["files"], 0);
        assert!(parsed["lastApplied"].is_null());
    }

    #[test]
    fn cmd_status_module_known_module_with_state_renders_details() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        // Pre-populate module state.
        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .upsert_module_state(
                "test-mod",
                None,
                "pkg-hash-xyz",
                "files-hash-abc",
                None,
                "installed",
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            false,
            ModuleStatusView::Compact,
        )
        .unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Status: test-mod"),
            "should print module heading, got: {output}"
        );
        assert!(
            output.contains("Packages") && output.contains('1'),
            "module declares 1 package, got: {output}"
        );
        assert!(
            output.contains("Synced"),
            "should print state-store status, got: {output}"
        );
    }

    fn declared(name: &str, platforms: &[&str]) -> cfgd_core::config::ModulePackageEntry {
        cfgd_core::config::ModulePackageEntry {
            name: name.to_string(),
            platforms: platforms.iter().map(|p| (*p).to_string()).collect(),
            ..Default::default()
        }
    }

    /// One name declared twice under two managers is two rows with two
    /// verdicts. Keyed by name alone, the second resolution overwrote the
    /// first and both rows rendered the same manager.
    #[test]
    fn two_declarations_of_one_name_each_keep_their_own_manager() {
        let mut scanned = std::collections::HashMap::new();
        scanned.insert(
            "docker".to_string(),
            std::collections::VecDeque::from(vec![
                ("brew".to_string(), ModulePackagePresence::Installed),
                ("brew-cask".to_string(), ModulePackagePresence::NotInstalled),
            ]),
        );

        let rows = join_package_state(
            &[declared("docker", &[]), declared("docker", &[])],
            &mut scanned,
            Platform::current(),
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].manager.as_deref(), Some("brew"));
        assert_eq!(rows[0].state, ModulePackagePresence::Installed);
        assert_eq!(rows[1].manager.as_deref(), Some("brew-cask"));
        assert_eq!(rows[1].state, ModulePackagePresence::NotInstalled);
    }

    /// A gated entry resolved to nothing, so it must not consume the verdict
    /// its same-named sibling earned.
    #[test]
    fn a_gated_declaration_does_not_consume_its_siblings_verdict() {
        let mut scanned = std::collections::HashMap::new();
        scanned.insert(
            "docker".to_string(),
            std::collections::VecDeque::from(vec![(
                "brew".to_string(),
                ModulePackagePresence::Installed,
            )]),
        );

        let rows = join_package_state(
            &[declared("docker", &["plan9"]), declared("docker", &[])],
            &mut scanned,
            Platform::current(),
        );

        assert_eq!(rows[0].state, ModulePackagePresence::PlatformSkipped);
        assert_eq!(rows[0].manager, None);
        assert_eq!(rows[1].manager.as_deref(), Some("brew"));
        assert_eq!(rows[1].state, ModulePackagePresence::Installed);
    }

    /// A package the module's own `platforms` gate rules out is not "not
    /// scanned" — nobody was ever going to look. `cfgd module show` says
    /// `skipped (platform filter)` for the same package, and two surfaces
    /// answering one question differently is the drift this pins.
    #[test]
    fn a_platform_gated_package_reads_skipped_not_unscanned() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        // `plan9` is no OS, distro or arch cfgd targets, so the gate closes on
        // every host the suite runs on.
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: ripgrep\n    - name: plan9-only\n      platforms:\n        - plan9\n",
        )
        .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            false,
            ModuleStatusView::Inventory { show_values: false },
        )
        .unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        let gated = output
            .lines()
            .find(|l| l.contains("plan9-only"))
            .unwrap_or_else(|| panic!("gated package must render a row, got:\n{output}"));
        assert!(
            gated.contains(PLATFORM_SKIPPED),
            "gated package must read the platform-filter wording, got: {gated}"
        );
        let ungated = output
            .lines()
            .find(|l| l.contains("ripgrep"))
            .unwrap_or_else(|| panic!("ungated package must render a row, got:\n{output}"));
        assert!(
            ungated.contains(NOT_SCANNED),
            "an ungated package with no scan still reads not scanned, got: {ungated}"
        );
    }

    /// The whole shape of the bug: a module whose declared packages the machine
    /// already holds plans nothing, so `Reconciler::apply` — the only writer of
    /// `module_state` — never runs. The run must still record the module, or
    /// `cfgd status` and `cfgd module list` both call a fully converged module
    /// `NotApplied` forever.
    #[test]
    fn a_converged_module_apply_still_records_the_module_as_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        // `fakemgr` reports itself present and reports `ripgrep` installed —
        // `echo` runs on every host cfgd targets, so the fixture describes the
        // same machine everywhere and no real package manager is reached.
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n  packages:\n    custom:\n      - name: fakemgr\n        check: echo ok\n        listInstalled: echo ripgrep\n        install: echo install\n        uninstall: echo uninstall\n",
        )
        .unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: ripgrep\n      prefer:\n        - fakemgr\n",
        )
        .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (apply_printer, apply_buf) = test_printers();
        let args = crate::cli::ApplyArgs {
            on_conflict: crate::cli::OnConflict::Ask,
            from: None,
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: vec![],
            with_profile: false,
            skip_scripts: false,
            context: "apply".to_string(),
            shell: None,
        };
        crate::cli::apply::cmd_apply(&cli, &apply_printer, &args).unwrap();
        drop(apply_printer);
        let applied = cfgd_core::test_helpers::captured_text(&apply_buf);

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let record = store
            .module_state_by_name("test-mod")
            .unwrap()
            .unwrap_or_else(|| {
                panic!("a converged apply must still record module_state, apply said:\n{applied}")
            });
        assert_eq!(record.status, "installed");
        assert!(
            !record.packages_hash.is_empty(),
            "the recorded packages_hash must describe the declared set, got: {record:?}"
        );

        let (printer, buf) = test_printers();
        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            false,
            ModuleStatusView::Compact,
        )
        .unwrap();
        drop(printer);
        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Synced") && !output.contains("NotApplied"),
            "a converged module must not report itself unapplied, got: {output}"
        );
    }

    /// The inverse of the converged-apply case above: a `--skip` that empties
    /// an otherwise non-empty plan must
    /// not record the module as installed. `fakemgr` here reports the package
    /// absent, so the plan holds a real install action before filtering; the
    /// skip token removes it entirely, leaving a machine that is NOT
    /// converged with nothing left to apply. Recording `module_state` from
    /// that emptied plan would claim a package the machine never received.
    #[test]
    fn a_filter_emptied_plan_does_not_record_the_module_as_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        // `fakemgr` reports the package absent, so the reconciler plans a real
        // install before any filter runs.
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n  packages:\n    custom:\n      - name: fakemgr\n        check: echo ok\n        listInstalled: echo none\n        install: echo install\n        uninstall: echo uninstall\n",
        )
        .unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: ripgrep\n      prefer:\n        - fakemgr\n",
        )
        .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (apply_printer, apply_buf) = test_printers();
        let args = crate::cli::ApplyArgs {
            on_conflict: crate::cli::OnConflict::Ask,
            from: None,
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec!["module:test-mod".to_string()],
            only: vec![],
            module: vec![],
            with_profile: false,
            skip_scripts: false,
            context: "apply".to_string(),
            shell: None,
        };
        crate::cli::apply::cmd_apply(&cli, &apply_printer, &args).unwrap();
        drop(apply_printer);
        let applied = cfgd_core::test_helpers::captured_text(&apply_buf);

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        assert!(
            store.module_state_by_name("test-mod").unwrap().is_none(),
            "a --skip that empties the plan must not mint a converged \
             module_state row for a module the machine never applied, \
             apply said:\n{applied}"
        );
    }

    #[test]
    fn cmd_status_module_without_state_record_reads_not_applied() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            false,
            ModuleStatusView::Compact,
        )
        .unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("NotApplied"),
            "no state-store record should produce 'NotApplied', got: {output}"
        );
    }

    #[test]
    fn cmd_status_module_renders_deployed_files_section() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        // Materialize an existing deployed file so the path-exists branch runs
        // (and a separate missing-file path so the error-line branch runs).
        let real_file = tmp_home.path().join("real.conf");
        std::fs::write(&real_file, b"x").unwrap();

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let apply_id = store
            .record_apply("default", "h", ApplyStatus::Success, None)
            .unwrap();
        store
            .upsert_module_file(
                "test-mod",
                &cfgd_core::to_posix_fs_key(&real_file),
                "hash-exists",
                "copy",
                apply_id,
            )
            .unwrap();
        store
            .upsert_module_file(
                "test-mod",
                "/nonexistent/missing.conf",
                "hash-missing",
                "copy",
                apply_id,
            )
            .unwrap();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();

        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            false,
            ModuleStatusView::Inventory { show_values: false },
        )
        .unwrap();
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("Deployed Files"),
            "deployed files section should be present, got: {output}"
        );
        // The row folds the test home to `~/`, like every display slot.
        assert!(
            output.contains(&cfgd_core::fold_home_in_text(&cfgd_core::to_posix_string(
                &real_file
            ))),
            "existing file should appear, got: {output}"
        );
        assert!(
            output.contains("/nonexistent/missing.conf") && output.contains("— missing"),
            "missing file should be flagged, got: {output}"
        );
        // No scan ran, so the present file's CONTENT is unchecked and the row
        // must say that rather than claim health `Path::exists` cannot back.
        let shown = cfgd_core::fold_home_in_text(&cfgd_core::to_posix_string(&real_file));
        let present_row = output
            .lines()
            .find(|l| l.contains(&shown))
            .unwrap_or_else(|| panic!("no row for the present file: {output}"));
        assert!(
            present_row.contains(NOT_SCANNED) && !present_row.contains('✓'),
            "an unscanned present file must not read converged: {present_row}"
        );
    }

    #[test]
    fn cmd_status_module_known_module_json_shape() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        store
            .upsert_module_state("test-mod", None, "pkgh", "fileh", None, "installed")
            .unwrap();

        let mut cli = test_cli_for(config_path, state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            false,
            ModuleStatusView::Compact,
        )
        .unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(parsed["name"], "test-mod");
        assert_eq!(parsed["packages"], 1);
        assert_eq!(parsed["files"], 0);
        assert_eq!(parsed["status"], "installed");
        assert!(
            parsed["lastApplied"].is_string(),
            "lastApplied should be the installed_at timestamp, got: {parsed}"
        );
        assert!(parsed["depends"].is_array());
    }

    /// A module whose one declared file already matches its deployed copy, so
    /// a live scan of it finds nothing and the only thing left to observe is
    /// what the payload SAYS about having scanned.
    ///
    /// Held as a struct rather than returned loose because every field is a
    /// live guard: dropping a `TempDir` deletes the tree the run is reading,
    /// and dropping the home guard hands the run the real `$HOME`.
    struct ConvergedModuleEnv {
        config_path: std::path::PathBuf,
        state_dir: tempfile::TempDir,
        target: std::path::PathBuf,
        _config_dir: tempfile::TempDir,
        _target_dir: tempfile::TempDir,
        _home: cfgd_core::TestHomeGuard,
    }

    fn converged_module_env() -> ConvergedModuleEnv {
        module_env_with("same content\n", "[]")
    }

    /// `converged_module_env` with the two knobs the state-rendering tests
    /// turn: what the deployed target actually holds (content identical to the
    /// module's source converges, anything else is content drift), and the
    /// module's declared `packages:` block.
    fn module_env_with(target_content: &str, packages_yaml: &str) -> ConvergedModuleEnv {
        let tmp_home = tempfile::tempdir().unwrap();
        let home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("converged.conf");
        std::fs::write(&target, target_content).unwrap();

        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("conf"), "same content\n").unwrap();
        let module_yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: {}\n  files:\n    - source: conf\n      target: {}\n",
            packages_yaml,
            cfgd_core::to_posix_string(&target)
        );
        std::fs::write(mod_dir.join("module.yaml"), module_yaml).unwrap();

        ConvergedModuleEnv {
            config_path,
            state_dir,
            target,
            _config_dir: config_dir,
            _target_dir: target_dir,
            _home: home,
        }
    }

    /// Record `target` as a file this module deployed, so the Deployed Files
    /// section has a row to state a verdict about.
    fn record_deployed(env: &ConvergedModuleEnv) {
        let store = open_state_store(Some(env.state_dir.path()), cfgd_core::Scope::User).unwrap();
        let apply_id = store
            .record_apply("default", "h", ApplyStatus::Success, None)
            .unwrap();
        store
            .upsert_module_file(
                "test-mod",
                &cfgd_core::to_posix_fs_key(&env.target),
                "hash-deployed",
                "copy",
                apply_id,
            )
            .unwrap();
    }

    /// Everything the report says under its `Deployed Files` heading.
    fn deployed_files_section(output: &str) -> &str {
        output
            .split_once("Deployed Files")
            .unwrap_or_else(|| panic!("no Deployed Files section: {output}"))
            .1
    }

    /// A file whose content drifted carries that verdict on its own Deployed
    /// Files row. It is present on disk, so the bare `Path::exists` check this
    /// row used to be rendered it converged — beside, in the compact view, its
    /// own drift finding.
    #[test]
    fn cmd_status_module_drifted_file_is_never_ok_under_deployed_files() {
        let env = module_env_with("tampered\n", "[]");
        record_deployed(&env);

        let cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            true,
            ModuleStatusView::Inventory { show_values: false },
        )
        .unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        let deployed = deployed_files_section(&out);
        let path = cfgd_core::to_posix_string(&env.target);
        let row = deployed
            .lines()
            .find(|l| l.contains(&path))
            .unwrap_or_else(|| panic!("no deployed row for {path}: {out}"));
        assert!(
            row.contains("content differs"),
            "the row must carry the cause the scan found, not a bare presence check: {row}"
        );
        assert!(
            !row.contains('✓'),
            "a drifted file must not render converged: {row}"
        );
    }

    /// The same module with nothing tampered: the row reads converged, so the
    /// drift marking above is a verdict rather than a constant.
    #[test]
    fn cmd_status_module_converged_file_reads_deployed_after_a_scan() {
        let env = converged_module_env();
        record_deployed(&env);

        let cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            true,
            ModuleStatusView::Inventory { show_values: false },
        )
        .unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        let deployed = deployed_files_section(&out);
        let path = cfgd_core::to_posix_string(&env.target);
        let row = deployed
            .lines()
            .find(|l| l.contains(&path))
            .unwrap_or_else(|| panic!("no deployed row for {path}: {out}"));
        assert!(
            row.contains('✓') && !row.contains("content differs"),
            "a scanned, converged file must read converged: {row}"
        );
    }

    /// P1: the packages phase has a STATE presentation, not just a declared
    /// count. A `script` package is the deterministic arm — no manager can be
    /// asked about it on any host — so the row names the manager that would
    /// have answered and says plainly that nothing did.
    #[test]
    fn cmd_status_module_scan_renders_package_state_per_declared_package() {
        let env = module_env_with(
            "same content\n",
            "\n    - name: rustup\n      prefer:\n        - script\n      script: \"true\"",
        );
        let cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            true,
            ModuleStatusView::Inventory { show_values: false },
        )
        .unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("\nInstalled Packages\n"),
            "the packages phase must have a section of its own: {out}"
        );
        let row = out
            .lines()
            .find(|l| l.contains("rustup"))
            .unwrap_or_else(|| panic!("no package row: {out}"));
        assert!(
            row.contains(NOT_SCANNED) && row.contains("(script)"),
            "the row must name the manager and the verdict it gave: {row}"
        );
    }

    /// Without `--scan` the section still stands — a declared package with no
    /// state is still a package the apply installed — and every row says
    /// nothing asked, rather than borrowing the ✓ a scan would have earned.
    #[test]
    fn cmd_status_module_without_scan_lists_declared_packages_as_unscanned() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_cfg_dir, state_dir, config_path) = setup_env_with_module();

        let cli = test_cli_for(config_path, state_dir.path());
        let (printer, buf) = test_printers();
        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            false,
            ModuleStatusView::Inventory { show_values: false },
        )
        .unwrap();
        drop(printer);

        let out = cfgd_core::test_helpers::captured_text(&buf);
        let row = out
            .lines()
            .find(|l| l.contains("ripgrep"))
            .unwrap_or_else(|| panic!("the declared package must appear: {out}"));
        assert!(
            row.contains(NOT_SCANNED) && !row.contains('✓'),
            "an unscanned package must not read installed: {row}"
        );
    }

    /// `--scan` without `-e` scans, and the payload must say so.
    ///
    /// The two flags are separate now, and every other module test passes them
    /// together — which is exactly the pairing under which a payload reporting
    /// the WRONG one of the two still reads correct. A consumer differencing
    /// an empty `drift` array has only this flag to tell "checked, and the
    /// machine is clean" from "never checked".
    #[test]
    fn cmd_status_module_scan_without_exit_code_reports_the_scan_it_ran() {
        let env = converged_module_env();
        let mut cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            true,
            ModuleStatusView::Compact,
        )
        .unwrap();
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(
            parsed["driftCheckedLive"], true,
            "`--scan` ran the live scan, so the payload must not report otherwise: {parsed}"
        );
        assert_eq!(
            parsed["drift"],
            serde_json::json!([]),
            "a converged module's live scan must find nothing, got: {parsed}"
        );
    }

    // The drift-catching, exit(5) branch is proven by the real subprocess in
    // `tests/cli_integration.rs::status_module_exit_code_catches_module_file_drift`
    // — `process::exit` cannot be exercised in-process. This test proves the
    // complementary path: a converged module's live scan finds nothing, so
    // `--exit-code` must return Ok rather than calling `process::exit`.
    #[test]
    fn cmd_status_module_exit_code_true_no_drift_returns_ok() {
        let env = converged_module_env();
        let mut cli = test_cli_for(env.config_path.clone(), env.state_dir.path());
        cli.output = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
        let (printer, buf) = test_printers_json();

        let res = cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            true,
            true,
            ModuleStatusView::Compact,
        );
        assert!(
            res.is_ok(),
            "exit_code=true with a converged module must return Ok, got: {res:?}"
        );
        drop(printer);

        let captured = cfgd_core::test_helpers::captured_text(&buf);
        let parsed: serde_json::Value = serde_json::from_str(captured.trim())
            .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {captured}"));
        assert_eq!(
            parsed["driftCheckedLive"], true,
            "exit_code=true must have actually run the live scan, got: {parsed}"
        );
        assert_eq!(
            parsed["drift"],
            serde_json::json!([]),
            "a converged module's live scan must find nothing, got: {parsed}"
        );
    }

    /// The Last Apply block names what the run was scoped to. A profile-scoped
    /// run reads `Profile`; an isolated one reads `Scope`, because
    /// `module:nvim` is not a profile and labelling it as one names a profile
    /// nothing has. A store written by an older cfgd still holds the
    /// `unknown` placeholder, which renders as no row at all.
    #[test]
    fn a_recorded_scope_is_labelled_by_what_it_names() {
        assert_eq!(
            super::recorded_scope_row("module:nvim"),
            Some(("Scope", "module:nvim"))
        );
        assert_eq!(
            super::recorded_scope_row("module:nvim, module:zsh"),
            Some(("Scope", "module:nvim, module:zsh"))
        );
        assert_eq!(super::recorded_scope_row("base"), Some(("Profile", "base")));
        assert_eq!(super::recorded_scope_row("unknown"), None);
        assert_eq!(super::recorded_scope_row(""), None);
    }

    fn module_status_with_scope(scope: Option<&str>) -> ModuleStatus {
        ModuleStatus {
            name: "nvim".into(),
            packages: 0,
            files: 0,
            env: 0,
            aliases: 0,
            scripts: Vec::new(),
            declared: cfgd_core::modules::ModuleSurfaces::default(),
            system: Vec::new(),
            depends: Vec::new(),
            status: cfgd_core::state::MODULE_STATUS_INSTALLED.to_string(),
            last_applied: Some("2026-05-14T10:00:00Z".into()),
            scope: scope.map(str::to_string),
            package_state: Vec::new(),
            deployed_files: Vec::new(),
            drift: Vec::new(),
            drift_checked_live: false,
        }
    }

    /// Two hours after the fixture's `last_applied`, so the rendered age is a
    /// fixed `2h ago` rather than moving with the suite's clock.
    const MODULE_STATUS_NOW: &str = "2026-05-14T12:00:00Z";

    fn module_status_render(scope: Option<&str>) -> String {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_module_status_doc(
            &module_status_with_scope(scope),
            ModuleStatusView::Compact,
            MODULE_STATUS_NOW,
        ));
        drop(printer);
        cfgd_core::test_helpers::captured_text(&buf)
    }

    /// A module's report names the scope of the run that last applied it, but
    /// only when that run was isolated to modules: a profile-wide apply is a
    /// fact about the machine, which `cfgd status` already states, and
    /// repeating it here would answer a question nobody asked about this
    /// module.
    #[test]
    fn a_module_report_names_an_isolated_runs_scope_and_no_other() {
        let isolated = module_status_render(Some("module:nvim"));
        assert!(
            isolated.contains("Scope") && isolated.contains("module:nvim"),
            "an isolated run's scope is named: {isolated}"
        );

        for recorded in [Some("base"), Some("unknown"), Some(""), None] {
            let out = module_status_render(recorded);
            assert!(
                !out.contains("Scope"),
                "no Scope row for a recorded {recorded:?}: {out}"
            );
        }
    }

    /// The `Scope` row carries owner TOKENS, not a string: the renderer paints
    /// each one through `OwnerLabel`'s three slots, the same coat the apply
    /// tree's group headings and the Managed Resources Owner column wear, so
    /// one owner is spelled one way whichever surface names it. An isolated run
    /// over several modules recorded a `, `-joined list, and every member of it
    /// is its own token.
    #[test]
    fn a_scope_row_carries_the_owner_tokens_it_names() {
        let one = super::scope_row("module:nvim");
        assert_eq!(one.value, "module:nvim", "the plain value is the token");
        assert_eq!(
            one.owners.iter().map(OwnerLabel::plain).collect::<Vec<_>>(),
            vec!["module:nvim".to_string()],
            "the row carries the owner the token names"
        );

        let many = super::scope_row("module:nvim, module:zsh");
        assert_eq!(
            many.owners
                .iter()
                .map(OwnerLabel::plain)
                .collect::<Vec<_>>(),
            vec!["module:nvim".to_string(), "module:zsh".to_string()],
            "every member of a multi-module scope is its own token"
        );
        assert_eq!(many.value, "module:nvim, module:zsh");

        // Nothing a run recorded is dropped for being unreadable: a token no
        // owner kind claims keeps the recorded string rather than losing the row.
        let unreadable = super::scope_row("whatever");
        assert!(unreadable.owners.is_empty());
        assert_eq!(unreadable.value, "whatever");
    }

    /// The two rows a reader scans for a module's standing lead its report:
    /// `Status`, then `Scope`, and only then the recorded `Last Applied` and
    /// the counts under it.
    #[test]
    fn the_status_and_scope_rows_lead_a_module_report() {
        let out = module_status_render(Some("module:nvim"));
        let row = |key: &str| {
            out.lines()
                .position(|l| l.trim_start().starts_with(key))
                .unwrap_or_else(|| panic!("the {key} row renders: {out}"))
        };
        // `Status: nvim` is the heading, so the Status ROW is the one indented
        // under it — the first line whose trimmed text opens on the key alone.
        let status = out
            .lines()
            .position(|l| l.trim_start().starts_with("Status "))
            .unwrap_or_else(|| panic!("the Status row renders: {out}"));
        assert!(
            status < row("Scope") && row("Scope") < row("Last Applied"),
            "Status, then Scope, then Last Applied: {out}"
        );
    }

    /// The payload gains a field only when there is a scope to state, so every
    /// reader of a profile-wide module report sees the object it always saw
    /// rather than a new `null` key.
    #[test]
    fn a_scopeless_module_payload_carries_no_scope_key() {
        let absent = serde_json::to_value(module_status_with_scope(None))
            .expect("a module status serializes");
        assert!(
            absent.get("scope").is_none(),
            "no scope key without a scope: {absent}"
        );

        let present = serde_json::to_value(module_status_with_scope(Some("module:nvim")))
            .expect("a module status serializes");
        assert_eq!(
            present.get("scope").and_then(serde_json::Value::as_str),
            Some("module:nvim"),
            "an isolated run's scope reaches the payload: {present}"
        );
    }

    /// Config dir + state dir whose profile manages ONE file, applied for
    /// real, so the machine holds a target cfgd itself wrote — the fixture
    /// every record-fed drift pin below starts from.
    fn applied_managed_file_env(
        home: &std::path::Path,
    ) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let files_dir = config_dir.path().join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("managed.txt"), "declared content\n").unwrap();
        let target = home.join("deployed.txt");
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: files/managed.txt\n        target: {}\n        strategy: Copy\n",
                target.display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(config_dir.path().join("modules")).unwrap();

        let cli = test_cli_for(config_path.clone(), state_dir.path());
        let args = crate::cli::ApplyArgs {
            on_conflict: crate::cli::OnConflict::Ask,
            from: None,
            dry_run: false,
            phase: None,
            yes: true,
            skip: vec![],
            only: vec![],
            module: vec![],
            with_profile: false,
            skip_scripts: false,
            context: "apply".to_string(),
            shell: None,
        };
        let printer = cfgd_core::test_helpers::test_printer();
        crate::cli::apply::cmd_apply(&cli, &printer, &args).unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "declared content\n",
            "the fixture's apply must deploy the managed file"
        );
        (config_dir, state_dir, config_path)
    }

    /// A drift a live check finds is a fact the record keeps: `cfgd diff`
    /// records every finding as a `drift_events` row, so the next plain
    /// `cfgd status` — the recorded dashboard, no scan — lists it instead of
    /// claiming a machine the last check saw drifting is clean.
    #[test]
    #[serial_test::serial]
    fn a_drift_a_live_check_finds_is_visible_to_the_next_recorded_status() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_config_dir, state_dir, config_path) = applied_managed_file_env(tmp_home.path());
        let target = tmp_home.path().join("deployed.txt");

        std::fs::write(&target, "edited out of band\n").unwrap();

        let cli = test_cli_for(config_path.clone(), state_dir.path());
        let printer = cfgd_core::test_helpers::test_printer();
        crate::cli::diff::cmd_diff(&cli, &printer, None, false).unwrap();

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let rows = store.unresolved_drift().unwrap();
        assert!(
            rows.iter()
                .any(|e| e.resource_type == "file" && e.resource_id.contains("deployed.txt")),
            "`cfgd diff` must record the file drift it found, got: {rows:?}"
        );

        let (printer, buf) = test_printers();
        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("deployed.txt"),
            "the recorded status view must list the drifted file, got: {out}"
        );
        assert!(
            !out.contains("No drift recorded"),
            "a machine the last check saw drifting is not clean, got: {out}"
        );
    }

    /// The resolve half of the same contract: a full live check that no
    /// longer finds a recorded drift proves it healed, so the row resolves
    /// and the next recorded `status` reads clean again.
    #[test]
    #[serial_test::serial]
    fn a_healed_drift_the_next_live_check_clears_from_the_recorded_status() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let (_config_dir, state_dir, config_path) = applied_managed_file_env(tmp_home.path());
        let target = tmp_home.path().join("deployed.txt");

        std::fs::write(&target, "edited out of band\n").unwrap();
        let cli = test_cli_for(config_path.clone(), state_dir.path());
        let printer = cfgd_core::test_helpers::test_printer();
        crate::cli::diff::cmd_diff(&cli, &printer, None, false).unwrap();
        {
            let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
            assert!(
                !store.unresolved_drift().unwrap().is_empty(),
                "the fixture's first diff must record the drift"
            );
        }

        // Heal by hand and check again: the second diff re-finds nothing,
        // which is the evidence the recorded row needs to resolve.
        std::fs::write(&target, "declared content\n").unwrap();
        crate::cli::diff::cmd_diff(&cli, &printer, None, false).unwrap();

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        let rows = store.unresolved_drift().unwrap();
        assert!(
            rows.is_empty(),
            "a full check that re-found nothing must resolve the recorded rows, got: {rows:?}"
        );

        let (printer, buf) = test_printers();
        cmd_status(&cli, &printer, None, false, false, false).unwrap();
        drop(printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            out.contains("No drift recorded"),
            "a healed machine's recorded status must read clean, got: {out}"
        );
        assert!(
            !out.contains("edited out of band"),
            "no healed finding may linger on the recorded view, got: {out}"
        );
    }

    /// A module-scoped scan is evidence about ONE module, not the machine:
    /// it must not write the machine-wide `last_scan` stamp, must not
    /// resolve another module's (or the machine's) recorded rows, and must
    /// still record and resolve rows inside its own scope.
    #[test]
    #[serial_test::serial]
    fn a_module_scoped_scan_stamps_nothing_and_touches_only_its_own_rows() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("cfgd.yaml");
        std::fs::write(&config_path, CONFIG_YAML).unwrap();
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), PROFILE_WITH_MODULE_YAML).unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("conf"), "module content\n").unwrap();
        let module_target = tmp_home.path().join("mod-file.txt");
        std::fs::write(
            mod_dir.join("module.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  files:\n    - source: conf\n      target: {}\n",
                module_target.display()
            ),
        )
        .unwrap();

        // Rows OUTSIDE the scan's scope: another module's file, and a
        // machine-wide file. A scoped scan may vouch for neither.
        {
            let store =
                cfgd_core::state::StateStore::open(&state_dir.path().join("state.db")).unwrap();
            store
                .record_drift(
                    "module",
                    "other-mod/etc/other.conf",
                    None,
                    Some("x"),
                    "local",
                )
                .unwrap();
            store
                .record_drift("file", "/etc/hosts", None, Some("x"), "local")
                .unwrap();
        }

        let cli = test_cli_for(config_path, state_dir.path());
        let printer = cfgd_core::test_helpers::test_printer();
        // The module target is missing, so the scoped scan FINDS drift.
        cmd_status_module(
            &RunContext::new(&cli, &printer),
            "test-mod",
            false,
            true,
            ModuleStatusView::Compact,
        )
        .unwrap();

        let store = open_state_store(Some(state_dir.path()), cfgd_core::Scope::User).unwrap();
        assert_eq!(
            store.last_scan_at().unwrap(),
            None,
            "one module's scan must not date the machine-wide dashboard"
        );
        let rows = store.unresolved_drift().unwrap();
        assert!(
            rows.iter()
                .any(|e| e.resource_type == "module" && e.resource_id.starts_with("test-mod/")),
            "the scoped scan must record its own module's finding, got: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|e| e.resource_id == "other-mod/etc/other.conf"),
            "another module's recorded row must stand, got: {rows:?}"
        );
        assert!(
            rows.iter().any(|e| e.resource_id == "/etc/hosts"),
            "a machine-wide recorded row must stand, got: {rows:?}"
        );

        // Heal the module file and scan again through the OTHER scoped
        // surface (`diff --module`): its own row resolves, the out-of-scope
        // rows still stand, the stamp is still unwritten.
        std::fs::write(&module_target, "module content\n").unwrap();
        crate::cli::diff::cmd_diff(&cli, &printer, Some("test-mod"), false).unwrap();

        let rows = store.unresolved_drift().unwrap();
        assert!(
            !rows
                .iter()
                .any(|e| e.resource_type == "module" && e.resource_id.starts_with("test-mod/")),
            "a scoped check that re-found nothing in its scope must resolve its own rows, got: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|e| e.resource_id == "other-mod/etc/other.conf")
                && rows.iter().any(|e| e.resource_id == "/etc/hosts"),
            "out-of-scope rows must survive a scoped check, got: {rows:?}"
        );
        assert_eq!(
            store.last_scan_at().unwrap(),
            None,
            "a scoped check must never write the machine-wide stamp"
        );
    }
}
