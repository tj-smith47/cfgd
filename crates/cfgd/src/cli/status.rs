use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::LOCAL_LAYER;
use cfgd_core::output::{
    Doc, KvPair, OwnerLabel, Printer, Role, SectionBuilder, condense_script_label, renderer::Table,
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
    pub packages: usize,
    pub files: usize,
    pub status: String,
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
    /// Resolved package name to the manager that installs it.
    pub package_managers: std::collections::BTreeMap<String, String>,
    /// `3 preApply, 6 postApply`, from [`cfgd_core::modules::ModuleSurfaces`] —
    /// the same tally, and the same rendering, `cfgd status <module>` reports.
    pub script_summary: Option<String>,
}

impl ModuleDeclared {
    fn of(module: &cfgd_core::modules::ResolvedModule) -> Self {
        Self {
            file_root: common_target_root(&module.files),
            package_managers: module
                .packages
                .iter()
                .map(|p| (p.resolved_name.clone(), p.manager.clone()))
                .collect(),
            script_summary: cfgd_core::modules::ModuleSurfaces::of_resolved(module)
                .script_summary(),
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
    pub env: usize,
    pub aliases: usize,
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
#[derive(Serialize)]
struct ModuleStatusPayload<'a> {
    #[serde(flatten)]
    module: &'a ModuleStatus,
    state: &'static str,
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
fn drift_section<T>(
    doc: Doc,
    drift: &[T],
    checked_live: bool,
    row: impl Fn(SectionBuilder, &T) -> SectionBuilder,
) -> Doc {
    doc.section("Drift", |s| {
        if drift.is_empty() {
            // Only the live scan may claim a detection. The recorded dashboard
            // has asked nothing of the machine, and "No drift detected" over a
            // host whose last apply left a declared package uninstalled is an
            // assurance no query backs.
            if checked_live {
                s.status(Role::Ok, "No drift detected")
            } else {
                s.status_with(Role::Ok, "No drift recorded", |sf| {
                    sf.detail("`cfgd diff` checks the live machine")
                })
            }
        } else {
            drift.iter().fold(s, &row)
        }
    })
}

/// Render the fleet-wide "Drift" section: one row per recorded event, named by
/// the resource type and id the event was stored under.
fn render_drift_section(
    doc: Doc,
    drift: &[cfgd_core::state::DriftEvent],
    checked_live: bool,
) -> Doc {
    let drop_env_file_row = cfgd_core::output::env_file_row_is_redundant(
        drift.iter().map(|e| e.resource_type.as_str()),
    );
    let rows: Vec<&cfgd_core::state::DriftEvent> = drift
        .iter()
        .filter(|e| !(drop_env_file_row && e.resource_type == "env"))
        .collect();
    drift_section(doc, &rows, checked_live, |s, event| {
        // A "script" / "Running script" resource_id is the raw run_str body
        // (preserved byte-identical for UPSERT matching against prior drift
        // rows) — condense only here, at the point it enters a status subject,
        // so a multi-line inline script never lands raw. Two type strings exist
        // because two producers persist script actions: `apply_script_action`
        // (main pre/post-apply phase scripts, format.rs's
        // `format_action_description`) stamps "script"; `execute_script`
        // (onChange / module-onChange scripts, reconciler/scripts.rs) stamps
        // "Running script: {body}" — both must condense here.
        let display_id =
            if event.resource_type == "script" || event.resource_type == "Running script" {
                condense_script_label(&event.resource_id)
            } else {
                event.resource_id.clone()
            };
        let subject = cfgd_core::output::drift_item_subject(&event.resource_type, &display_id);
        let (expected, actual) = cfgd_core::output::drift_operands(
            &event.resource_type,
            event.expected.as_deref().unwrap_or("?"),
            event.actual.as_deref().unwrap_or("?"),
        );
        if event.source != LOCAL_LAYER {
            // Source attribution renders in `secondary` (pink/magenta) at
            // end-of-subject; the StatusBuilder API guarantees the label lands
            // last so the inner SGR reset is never followed by outer-role-styled
            // text. The token is the vocabulary `cfgd sync` and `cfgd source *`
            // head their groups with, so a reader carries one spelling across
            // the three surfaces that name a source.
            let label_text = OwnerLabel::new("source", &event.source).plain();
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
    drift_section(doc, &ordered, checked_live, |s, d| {
        let subject = format!(
            "{}:{} {}",
            OwnerLabel::new("module", &d.owner).plain(),
            d.surface,
            d.item
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
        ("Profile", value)
    })
}

/// The recorded-state header's staleness threshold: a daemon's default
/// reconcile interval. Past this age, the recorded drift a plain `cfgd status`
/// shows could easily be older than a live daemon would ever let it get, so
/// the header hints at `--scan` instead of leaving the reader to guess.
const SCAN_STALENESS_SECS: i64 = cfgd_core::daemon::DEFAULT_RECONCILE_SECS as i64;

/// Build the fleet-wide `cfgd status` Doc. Caller supplies the precomputed
/// payload and the configured `SourceSpec` list so the renderer can show
/// "not yet fetched" rows for sources without state records.
pub fn build_fleet_status_doc(
    output: &StatusOutput,
    configured_sources: &[String],
    config_path: &Path,
    profile_name: &str,
    now: &str,
) -> Doc {
    let mut doc = Doc::new()
        .heading("Status")
        .kv("Config", config_path.display_posix());
    if let Some(profile) = derivable_profile(profile_name) {
        doc = doc.kv("Profile", profile);
    }

    // Only the recorded-state dashboard needs a staleness signal: a `--scan`/
    // `--exit-code` run just checked the machine itself, so its Drift section
    // already speaks for how current the display is.
    if !output.drift_checked_live {
        match &output.last_scan_at {
            Some(ts) => {
                let age = cfgd_core::humanize_age_since(ts, now).unwrap_or_else(|| ts.clone());
                doc = doc.kv("Last Scan", &age);
                if cfgd_core::is_stale_since(ts, now, SCAN_STALENESS_SECS) {
                    doc = doc.hint("Run `cfgd status --scan` for a live check");
                }
            }
            None => {
                doc = doc
                    .kv("Last Scan", "never")
                    .hint("Run `cfgd status --scan` for a live check");
            }
        }
    }

    match &output.last_apply {
        Some(last) => {
            doc = doc.section("Last Apply", |s| {
                let mut s = s.kv("Time", &last.timestamp);
                if let Some((key, value)) = recorded_scope_row(&last.profile) {
                    s = s.kv(key, value);
                }
                s = s.kv("Result", last.status.human_str());
                if let Some(summary) = &last.summary {
                    s = s.kv("Summary", summary);
                }
                s
            });
        }
        None => {
            doc = doc.status(Role::Info, "No applies recorded yet");
        }
    }

    doc = render_drift_section(doc, &output.drift, output.drift_checked_live);

    if !configured_sources.is_empty() {
        doc = doc.section("Config Sources", |s| {
            if output.sources.is_empty() {
                configured_sources
                    .iter()
                    .fold(s, |s, name| s.kv(name, "not yet fetched"))
            } else {
                let mut t = Table::new(["Source", "Status", "Version", "Last Fetched"]);
                for rec in &output.sources {
                    let (status, role) = cfgd_core::state::source_status_display(&rec.status);
                    t = t.row_styled([
                        (rec.name.clone(), None),
                        (status.to_string(), Some(role)),
                        (
                            rec.source_version.clone().unwrap_or_else(|| "-".into()),
                            None,
                        ),
                        (
                            rec.last_fetched.clone().unwrap_or_else(|| "never".into()),
                            None,
                        ),
                    ]);
                }
                s.table(t)
            }
        });
    }

    doc = doc.section_if_nonempty(
        "Pending Decisions",
        &output.pending_decisions,
        super::build_pending_decisions_table_section,
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
            // Fixed units, so they agree with their own count: one package is
            // `1 pkg`, not `1 pkgs`.
            let summary = format!(
                "{} pkg{}, {} file{}",
                m.packages,
                if m.packages == 1 { "" } else { "s" },
                m.files,
                if m.files == 1 { "" } else { "s" }
            );
            // The dashboard reads RECORDED state only, so no row here can
            // claim `Drifted` — this surface's Drift section is what reports
            // that, and `cfgd status --module --scan` is what derives it.
            let (state_word, role) = cfgd_core::state::module_status_display(&m.status, false);
            // Subject is the owner token, exactly as the tree that applied the
            // module heads its group; the counts and the state are what the
            // line reports about it.
            s.status_with(role, OwnerLabel::new("module", &m.name).plain(), |f| {
                f.detail(format!("{summary}, {state_word}"))
            })
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
            for row in managed_resource_rows(items, &output.modules) {
                t = t.row(row);
            }
            s.table(t)
        },
    );

    doc.with_data(output)
}

/// The owner column's word for what cfgd manages on the profile's own behalf
/// rather than for a module.
const OWNER_SELF: &str = "cfgd";

/// Stand-in for a resource column with nothing left to say — the same `-` the
/// Config Sources table renders for a version nobody has fetched.
const NO_DETAIL: &str = "-";

/// The Managed Resources rows, as `[Type, Owner, Resource, Source]`.
///
/// A recorded row is a state-matching key rather than a report: a `module`
/// row's id carries the owner and the surface inside it, and a `package` row
/// is ONE package where a reader wants the list a manager installed. Both are
/// split out here, so the table can say whose each resource is and can render
/// one row per manager rather than one per package.
fn managed_resource_rows(
    items: &[cfgd_core::state::ManagedResource],
    modules: &[ModuleStatusEntry],
) -> Vec<[String; 4]> {
    let mut rows: Vec<[String; 4]> = Vec::new();
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
            } else {
                r.resource_id.clone()
            };
            rows.push([
                display_type(&r.resource_type),
                OWNER_SELF.to_string(),
                resource,
                r.source.clone(),
            ]);
            continue;
        };
        let owner = OwnerLabel::new("module", module).plain();
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
        rows.push([surface.to_string(), owner, resource, r.source.clone()]);
    }

    for ((manager, source), mut packages) in own_packages {
        packages.sort_unstable();
        rows.push([
            "packages".to_string(),
            OWNER_SELF.to_string(),
            format!("{manager}: {}", packages.join(", ")),
            source.to_string(),
        ]);
    }
    // The recorded order is the state store's (type, id); grouping and
    // splitting break it, so the table sorts what it renders instead of
    // letting the shape of the rows decide the order.
    rows.sort();
    rows
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
    let count = recorded_count
        .parse::<usize>()
        .ok()
        .map(|n| cfgd_core::pluralize(n, "file"));
    let root = declared.and_then(|d| d.file_root.clone());
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
    let mut names: Vec<&str> = recorded.split(',').filter(|n| !n.is_empty()).collect();
    if names.is_empty() {
        return NO_DETAIL.to_string();
    }
    names.sort_unstable();
    let list = names.join(", ");
    let managers: std::collections::BTreeSet<&str> = names
        .iter()
        .filter_map(|n| declared?.package_managers.get(*n).map(String::as_str))
        .collect();
    match managers.len() {
        1 => format!("{}: {list}", managers.iter().next().unwrap_or(&"")),
        _ => list,
    }
}

/// The word the Type column reads for a recorded `resource_type`.
///
/// The recorded types are a state-matching vocabulary and stay exactly as they
/// are; the column names the SURFACE, in the same words a module row's own id
/// spells them (`files`, `packages`), so one table does not call one thing two
/// names depending on who declared it.
fn display_type(resource_type: &str) -> String {
    match resource_type {
        "file" => "files".to_string(),
        "package" => "packages".to_string(),
        "Running script" => "script".to_string(),
        other => other.to_string(),
    }
}

/// Build the per-module `cfgd status <module>` Doc.
///
/// Every row's subject is the thing's identity and its detail is what the
/// machine holds — the same grammar the fleet doc's module rows read in, so
/// one report never states a fact the other contradicts.
pub fn build_module_status_doc(output: &ModuleStatus, view: ModuleStatusView) -> Doc {
    // One aligned block: the Status row needs a role-tinted value, which only
    // `kv_rows` can carry, and `kv_rows` does not coalesce with a preceding
    // `kv` block — so every row of the header is built here.
    let (state_word, role) = output.state_display();
    let mut rows = vec![KvPair::role_valued("Status", state_word, role)];
    if let Some(last) = &output.last_applied {
        rows.push(KvPair::new("Last applied", last));
    }
    // Only an isolated run's scope: `recorded_scope_row` answers `Profile` for
    // a profile-wide apply, which belongs to `cfgd status` rather than to one
    // module's report.
    if let Some(("Scope", scope)) = output.scope.as_deref().and_then(recorded_scope_row) {
        rows.push(KvPair::new("Scope", scope));
    }
    // The counts are what the compact view has INSTEAD of the inventories: a
    // report that showed both would state every fact twice.
    if view == ModuleStatusView::Compact {
        rows.push(KvPair::new("Packages", output.packages.to_string()));
        rows.push(KvPair::new("Files", output.files.to_string()));
        if output.env > 0 {
            rows.push(KvPair::new("Env", output.env.to_string()));
        }
        if output.aliases > 0 {
            rows.push(KvPair::new("Aliases", output.aliases.to_string()));
        }
        if let Some(scripts) = output.declared.script_summary() {
            rows.push(KvPair::new("Scripts", scripts));
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
            render_module_drift_section(doc, &output.drift, output.drift_checked_live)
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
        files.iter().fold(s, |s, file| match file.state {
            // A converged row says the path and stops: "deployed" under a
            // heading that already says Deployed Files is a word per row that
            // adds nothing.
            ModuleFilePresence::Deployed => s.status(Role::Ok, &file.path),
            ModuleFilePresence::Drifted => {
                let cause = file_causes
                    .get(&super::live_drift::module_file_resource_id(
                        &output.name,
                        &file.path,
                    ))
                    .cloned()
                    .unwrap_or_else(|| file.state.label().to_string());
                s.status_with(file.state.role(), &file.path, |f| f.detail(cause))
            }
            _ => s.status_with(file.state.role(), &file.path, |f| {
                f.detail(file.state.label())
            }),
        })
    });

    doc = doc.section_if_nonempty("Env", &output.declared.env, |s, env| {
        let mut sorted: Vec<&cfgd_core::config::EnvVar> = env.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        sorted.into_iter().fold(s, |s, ev| {
            let subject = if show_values {
                format!("{}={}", ev.name, ev.value)
            } else {
                ev.name.clone()
            };
            s.status(Role::Ok, subject)
        })
    });

    doc = doc.section_if_nonempty("Aliases", &output.declared.aliases, |s, aliases| {
        let mut sorted: Vec<&cfgd_core::config::ShellAlias> = aliases.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        sorted.into_iter().fold(s, |s, alias| {
            let subject = if show_values {
                format!("{}={}", alias.name, alias.command)
            } else {
                alias.name.clone()
            };
            s.status(Role::Ok, subject)
        })
    });

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
    let mut resolved = desired.resolved;
    let resolved_modules = desired.modules;

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
            &resolved,
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
    let module_entries: Vec<ModuleStatusEntry> = resolved_modules
        .iter()
        .map(|module| {
            let status = state_map
                .get(&module.name)
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "not applied".into());
            ModuleStatusEntry {
                name: module.name.clone(),
                packages: module.packages.len(),
                files: module.files.len(),
                status,
                declared: ModuleDeclared::of(module),
            }
        })
        .collect();

    let configured_source_names: Vec<String> =
        cfg.spec.sources.iter().map(|s| s.name.clone()).collect();

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
    // var two different ways — one naming no value at all. Same display-only
    // recompute `drift_event_from` runs for a live finding, and the same
    // reason it is safe: nothing here is written back.
    for event in &mut output.drift {
        if let Some((expected, actual)) = cfgd_core::reconciler::env_item_display_values(
            &event.resource_type,
            &event.resource_id,
            &resolved.merged.env,
            &resolved.merged.aliases,
            &resolved_modules,
        ) {
            event.expected = Some(expected);
            event.actual = Some(actual);
        }
    }

    // Plain `status` (no --scan/--exit-code) keeps the fast RECORDED-drift
    // dashboard by deliberate design. `--scan` (and `--exit-code`, which
    // implies it), however, must reflect REALITY: a host with no daemon and no
    // prior scan has zero recorded events even when a managed file was just
    // edited out-of-band. So when scanning, run the LIVE, read-only scan
    // (never recording drift rows — the same checks `diff`/`verify` run, though
    // it DOES stamp the last-scan timestamp this header reads back next time)
    // BEFORE emitting, fold its findings into the displayed Drift section, then
    // exit 5 if `--exit-code` asked for it and any drift was found. This keeps
    // the human verdict and the exit code in agreement instead of printing "No
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
        for r in &drift {
            output.drift.push(super::live_drift::drift_event_from(
                r,
                &resolved.merged.env,
                &resolved.merged.aliases,
                &resolved_modules,
            ));
        }
        drift
    } else {
        Vec::new()
    };

    printer.emit(build_fleet_status_doc(
        &output,
        &configured_source_names,
        &cli.config,
        profile_name,
        &cfgd_core::utc_now_iso8601(),
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
        let resolved_modules = modules::resolve_modules(
            &[mod_name.to_string()],
            config_dir,
            &cache_base,
            &[],
            platform,
            &mgr_map,
            printer,
        )?;
        let resolved = empty_resolved_profile(&[mod_name.to_string()], &ctx.active_profile_name());
        let fm = CfgdFileManager::new(config_dir, &resolved)?;
        // One spinner across this module's live scan, narrated per pass.
        printer.narrate(
            format!("Scanning module:{mod_name} files"),
            |sp| -> anyhow::Result<()> {
                let file_results = super::live_drift::module_file_verify_results(
                    &fm,
                    config_dir,
                    &resolved,
                    &resolved_modules,
                )?;
                for r in file_results.into_iter().filter(|r| !r.matches) {
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
                        event: super::live_drift::drift_event_from(
                            &r,
                            &resolved.merged.env,
                            &resolved.merged.aliases,
                            &resolved_modules,
                        ),
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
                        // no manager gave.
                        let presence = if pkg.manager == "script"
                            || !mgr_map.contains_key(pkg.manager.as_str())
                        {
                            ModulePackagePresence::NotScanned
                        } else if let Some(pd) =
                            super::diff::package_missing_drift(pkg, &mgr_map, &pkg_cx)
                        {
                            drift.push(ModuleDrift {
                                event: super::live_drift::drift_event_from(
                                    &cfgd_core::reconciler::VerifyResult {
                                        resource_type: "package".to_string(),
                                        resource_id: super::diff::package_resource_id(
                                            &pd.manager,
                                            &pd.packages,
                                        ),
                                        matches: false,
                                        expected: "installed".to_string(),
                                        actual: cfgd_core::Absence::Missing.to_string(),
                                    },
                                    &resolved.merged.env,
                                    &resolved.merged.aliases,
                                    &resolved_modules,
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

    printer.emit(build_module_status_doc(&output, view));

    if exit_code && !output.drift.is_empty() {
        cfgd_core::exit::ExitCode::DriftDetected.exit();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn nvim_entry(declared: ModuleDeclared) -> ModuleStatusEntry {
        ModuleStatusEntry {
            name: "nvim".to_string(),
            packages: 3,
            files: 6,
            status: "installed".to_string(),
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
    /// manager, names alphabetical whatever order they were recorded in.
    #[test]
    fn profile_packages_collapse_into_one_row_per_manager() {
        let rows = managed_resource_rows(
            &[
                recorded("package", "brew/ripgrep"),
                recorded("package", "apt/git"),
                recorded("package", "brew/bat"),
            ],
            &[],
        );
        assert_eq!(
            rows,
            vec![
                [
                    "packages".to_string(),
                    "cfgd".to_string(),
                    "apt: git".to_string(),
                    "local".to_string()
                ],
                [
                    "packages".to_string(),
                    "cfgd".to_string(),
                    "brew: bat, ripgrep".to_string(),
                    "local".to_string()
                ],
            ]
        );
    }

    /// Two sources delivering one manager's packages are two facts. Merging
    /// them would attribute both to whichever source sorted first.
    #[test]
    fn one_manager_delivered_by_two_sources_stays_two_rows() {
        let mut remote = recorded("package", "brew/fd");
        remote.source = "acme".to_string();
        let rows = managed_resource_rows(&[recorded("package", "brew/bat"), remote], &[]);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0][2], "brew: bat");
        assert_eq!(rows[0][3], "local");
        assert_eq!(rows[1][2], "brew: fd");
        assert_eq!(rows[1][3], "acme");
    }

    /// A module row's id carries the owner and the surface; the detail the
    /// reader wants (where files land, which manager installs, how many hooks)
    /// lives in the resolution beside it.
    #[test]
    fn a_module_row_names_its_owner_and_reads_its_detail_from_the_resolution() {
        let declared = ModuleDeclared {
            file_root: Some("/home/u/.config/nvim".to_string()),
            package_managers: [("git", "apt"), ("gcc", "apt")]
                .into_iter()
                .map(|(p, m)| (p.to_string(), m.to_string()))
                .collect(),
            script_summary: Some("3 preApply, 6 postApply".to_string()),
        };
        let rows = managed_resource_rows(
            &[
                recorded("module", "nvim:files:6"),
                recorded("module", "nvim:packages:git,gcc"),
                recorded("module", "nvim:script"),
            ],
            &[nvim_entry(declared)],
        );
        let resources: Vec<&str> = rows.iter().map(|r| r[2].as_str()).collect();
        assert!(rows.iter().all(|r| r[1] == "module:nvim"), "{rows:?}");
        assert_eq!(
            resources,
            vec![
                "/home/u/.config/nvim (6 files)",
                "apt: gcc, git",
                "3 preApply, 6 postApply",
            ]
        );
    }

    /// The manager prefix is recovered from the resolution, so a row whose
    /// names do not all agree on one manager names none — better silent than
    /// claiming a manager that installs only part of its own list.
    #[test]
    fn a_package_row_names_a_manager_only_when_every_name_agrees() {
        let split = ModuleDeclared {
            package_managers: [("git", "apt"), ("neovim", "brew")]
                .into_iter()
                .map(|(p, m)| (p.to_string(), m.to_string()))
                .collect(),
            ..ModuleDeclared::default()
        };
        let rows = managed_resource_rows(
            &[recorded("module", "nvim:packages:neovim,git")],
            &[nvim_entry(split)],
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
        );
        let resources: Vec<&str> = rows.iter().map(|r| r[2].as_str()).collect();
        assert_eq!(resources, vec!["4 files", "zsh", "-"]);
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
        );
        let types: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(types, vec!["env", "files", "script"]);
        assert!(rows.iter().all(|r| r[1] == "cfgd"), "{rows:?}");
    }

    /// The module health line's units agree with their own counts: a module
    /// with one of each reads `1 pkg, 1 file`, and anything else — including
    /// zero — keeps the plural.
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
                    status: "installed".to_string(),
                    declared: ModuleDeclared::default(),
                },
                ModuleStatusEntry {
                    name: "nvim".to_string(),
                    packages: 3,
                    files: 12,
                    status: "installed".to_string(),
                    declared: ModuleDeclared::default(),
                },
                ModuleStatusEntry {
                    name: "git".to_string(),
                    packages: 0,
                    files: 0,
                    status: "installed".to_string(),
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
            &[],
            std::path::Path::new("/etc/cfgd/cfgd.yaml"),
            "default",
            "2026-05-12T14:30:25Z",
        ));
        drop(printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);

        assert!(
            out.contains("1 pkg, 1 file,"),
            "a single package and file must read singular: {out}"
        );
        assert!(
            out.contains("3 pkgs, 12 files,"),
            "many must stay plural: {out}"
        );
        assert!(
            out.contains("0 pkgs, 0 files,"),
            "zero keeps the plural: {out}"
        );
    }

    /// The recorded-state header says when the shown state was last checked
    /// against the machine, and hints at `--scan` once that answer is old
    /// enough to be misleading. The threshold is the daemon's default
    /// reconcile interval: past it, the dashboard is showing something a live
    /// daemon would never have let get this stale.
    ///
    /// A run that DID scan says nothing here — its Drift section already
    /// speaks for how current the display is — which is the branch that keeps
    /// `--scan`'s own output from carrying a hint pointing back at itself.
    #[test]
    fn status_header_dates_the_recorded_state_and_hints_when_it_is_stale() {
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
                &[],
                std::path::Path::new("/etc/cfgd/cfgd.yaml"),
                "default",
                // Pinned, never the wall clock: the age is a rendered value.
                "2026-05-14T10:05:00Z",
            ));
            drop(printer);
            cfgd_core::test_helpers::captured_text(&buf)
        }

        let hint = "cfgd status --scan";

        // Exactly at the threshold is not yet stale — `is_stale_since` is
        // "more than", so the boundary belongs to the fresh side and a daemon
        // reconciling on schedule never trips the hint.
        let fresh = header(Some("2026-05-14T10:00:00Z"), false);
        assert!(fresh.contains("Last Scan"), "no age row: {fresh}");
        assert!(fresh.contains("5m ago"), "wrong age rendered: {fresh}");
        assert!(!fresh.contains(hint), "a fresh scan must not hint: {fresh}");

        let stale = header(Some("2026-05-14T08:00:00Z"), false);
        assert!(stale.contains("2h ago"), "wrong age rendered: {stale}");
        assert!(stale.contains(hint), "a stale scan must hint: {stale}");

        let never = header(None, false);
        assert!(never.contains("never"), "no never row: {never}");
        assert!(never.contains(hint), "an unscanned host must hint: {never}");

        let scanned = header(Some("2026-05-14T08:00:00Z"), true);
        assert!(
            !scanned.contains("Last Scan") && !scanned.contains(hint),
            "a run that just scanned must not date or hint at itself: {scanned}"
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
            jsonpath: None,
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

    // --- cmd_status (aggregate) -------------------------------------------

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
            out.contains("\nEnv\n") && out.contains("EDITOR=nvim"),
            "--show-values must itemize env and show the declared value: {out}"
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
            tmp_home
                .path()
                .join(crate::cli::helpers::tests::primary_env_file_name()),
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
        }];
        let declared_line = cfgd_core::reconciler::env_item_declared_line(
            "env-var",
            "EDITOR",
            &declared_env,
            &[],
            &[],
        )
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
            tmp_home
                .path()
                .join(crate::cli::helpers::tests::primary_env_file_name()),
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
        }];
        let declared_line = cfgd_core::reconciler::env_item_declared_line(
            "env-var",
            "EDITOR",
            &declared_env,
            &[],
            &[],
        )
        .expect("EDITOR renders a declared line");
        let expected_detail =
            cfgd_core::output::drift_detail(&declared_line, cfgd_core::Absence::Missing.as_str());

        let cli = test_cli_for(config_path, &state_dir);
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
                real_file.to_str().unwrap(),
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
        assert!(
            output.contains(real_file.to_str().unwrap()),
            "existing file should appear, got: {output}"
        );
        assert!(
            output.contains("/nonexistent/missing.conf") && output.contains("— missing"),
            "missing file should be flagged, got: {output}"
        );
        // No scan ran, so the present file's CONTENT is unchecked and the row
        // must say that rather than claim health `Path::exists` cannot back.
        let present_row = output
            .lines()
            .find(|l| l.contains(real_file.to_str().unwrap()))
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

    fn module_status_render(scope: Option<&str>) -> String {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(build_module_status_doc(
            &module_status_with_scope(scope),
            ModuleStatusView::Compact,
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
}
