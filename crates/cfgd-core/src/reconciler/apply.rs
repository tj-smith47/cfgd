use crate::AbortFlag;
use crate::PathDisplayExt;
use crate::config::{LOCAL_LAYER, ResolvedProfile, ScriptShell};
use crate::errors::{CfgdError, ConfigError, PackageError, Result};
use crate::modules::ResolvedModule;
use crate::output::{Printer, Role, SectionGuard};
use crate::state::ApplyStatus;
use crate::to_posix_string;

use super::env_engine::ManagerPathDir;
use super::format::{
    action_display_subject_within, condense_action_desc_for_display, deploy_file_children,
    format_action_description, parse_package_description, parse_resource_from_description,
};
use super::restore::action_target_path;
use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, ScriptReport, ScriptSubject, build_module_script_env,
    build_script_env, effective_continue_on_error, execute_script, script_default_workdir,
};
use super::sidecar::SidecarOutcome;
use super::types::{
    Action, ActionResult, ApplyResult, ENV_RESOURCE_TYPE, MANAGER_RESOURCE_TYPE, ManagerAction,
    ModuleAction, ModuleActionKind, Owner, OwnerKind, PhaseFilter, PhaseName, Plan,
    ReconcileContext, ScriptAction, ScriptPhase, SystemAction, is_module_skip,
};
use crate::providers::{
    ActionNote, FileAction, NoteSink, PackageAction, ProviderRegistry, SecretAction,
};

/// One action's line in the execution tree, resolved where the outcome is known
/// and written either immediately (a streaming phase) or at phase close
/// (`Packages`, whose dispatch order is not its reading order).
pub(super) struct ActionOutcome {
    /// The action's display subject, resolved once at record time so the
    /// streaming writer and the deferred one cannot disagree about it.
    pub(super) subject: String,
    pub(super) role: Role,
    pub(super) detail: Option<String>,
    /// The detail was derived from the plan or from the action's own no-op
    /// status rather than from what happened, so it renders muted. A collapsed
    /// error never does — it is the thing the reader has to act on.
    pub(super) detail_muted: bool,
    pub(super) duration: Option<std::time::Duration>,
    notes: Vec<ActionNote>,
    /// The child output a concurrent lane captured instead of streaming, laid
    /// out beneath this line when the phase's tree is written. Always empty in
    /// a sequential phase, where the output window already showed it live.
    body: Vec<String>,
    /// Every file a `DeployFiles` action writes, target then resolved method —
    /// [`super::format::deploy_file_children`]'s output, carried here so
    /// [`emit_action_line`] renders the same list [`super::render_plan_tree`]
    /// previewed. Empty for every other action kind.
    children: Vec<(String, String)>,
}

#[cfg(test)]
impl ActionOutcome {
    /// The outcome of an action that succeeded, for a test driving the display
    /// path rather than an apply.
    pub(super) fn for_test(subject: &str, duration: std::time::Duration) -> Self {
        Self {
            subject: subject.to_string(),
            role: Role::Ok,
            detail: None,
            detail_muted: false,
            duration: Some(duration),
            notes: Vec::new(),
            body: Vec::new(),
            children: Vec::new(),
        }
    }

    /// [`Self::for_test`] with its child rows populated — for a test proving
    /// the plan and apply trees enumerate a `DeployFiles` action's files
    /// identically.
    pub(super) fn for_test_with_children(
        subject: &str,
        duration: std::time::Duration,
        children: Vec<(String, String)>,
    ) -> Self {
        Self {
            children,
            ..Self::for_test(subject, duration)
        }
    }

    /// The outcome `settle_action` records for an action that did nothing: the
    /// role its own text implies, and a detail it derived rather than observed.
    pub(super) fn for_test_settled(subject: &str, role: Role, detail: &str) -> Self {
        Self {
            subject: subject.to_string(),
            role,
            detail: Some(detail.to_string()),
            detail_muted: true,
            duration: None,
            notes: Vec::new(),
            body: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// What an env-file write put in the file, for the detail beside its own line:
/// `write ~/.cfgd.env — 3 vars, 3 aliases`.
///
/// `None` for every other action, and for a write that renders neither (the
/// neutralizing rewrite of a surface whose declarations all went away) — a
/// detail reading `0 vars, 0 aliases` states the same thing the empty file
/// already does.
fn env_write_summary(action: &Action) -> Option<String> {
    let Action::Env(super::types::EnvAction::WriteEnvFile { vars, aliases, .. }) = action else {
        return None;
    };
    let mut parts = Vec::new();
    if *vars > 0 {
        parts.push(crate::pluralize(*vars, "var"));
    }
    if *aliases > 0 {
        // Not `pluralize`: its plural is a bare `+s`, and this noun's is not.
        let noun = if *aliases == 1 { "alias" } else { "aliases" };
        parts.push(format!("{aliases} {noun}"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// What a module file deploy leaves alone, for the detail beside its own
/// line: `deploy init.lua — 5 already deployed` for a subset.
///
/// A subset is stated against the DECLARED set, so "one file changed" and
/// "nothing changed" (no action at all) can never render alike. A full deploy
/// carries no count at all: the subject already states how many it writes
/// (`deploy 6 files`), so a detail of `6 already deployed` would restate it.
///
/// The COMPLEMENT, never a ratio: the detail names only what the subject
/// cannot. A second number over a set the subject already spells out invites
/// pairing the wrong two. What the subject cannot say is how many declared
/// entries were already in place, so that is the number.
fn deploy_files_summary(action: &Action) -> Option<String> {
    let Action::Module(ModuleAction {
        kind:
            ModuleActionKind::DeployFiles {
                files,
                declared_total,
            },
        ..
    }) = action
    else {
        return None;
    };
    let written = files.len();
    (written < *declared_total).then(|| format!("{} already deployed", declared_total - written))
}

/// What a package install found already on the machine, for the detail beside
/// its own line: `apt install ripgrep, fd-find — 1 already installed`.
///
/// The subject stays the PLANNED set in every tree ([`action_display_subject`]
/// is one string across the preview bullet, the alignment column and the
/// executed row) and so does the recorded description, which is a wire
/// contract. What the executed row alone can differ on is the COUNT, and it
/// only learns it at execute time: the `Prerequisites` phase installs packages,
/// so an install re-reads the machine and drops every entry that is already
/// there. `installed` is that re-read's answer, carried out of the executor on
/// [`ActionRun`]; `None` is a preview, which has no answer yet.
///
/// No plain count on a full install: a package subject names every entry it
/// installs, so a trailing `— 6 packages` could only ever restate the row.
/// And the shortfall is the COMPLEMENT, never a ratio, for the reason the
/// deploy arm states: `— 7 of 9 packages` puts two numbers over one set on
/// one row, and the available reading was that the two the reader could not
/// pair were the ones that landed. The shortfall on this arm is never a
/// failure (a failed install fails the action; `installed_now` drops exactly
/// what an earlier phase already put on the machine), so the un-said number
/// is what was already there.
///
/// [`action_display_subject`]: super::format::action_display_subject
fn installed_packages_summary(
    action: &Action,
    installed: Option<usize>,
    delivered: usize,
) -> Option<String> {
    let planned = planned_package_count(action)?;
    let landed = installed.filter(|landed| *landed < planned)?;
    // `already installed` is the vocabulary for state this run did not
    // create. An entry the run's own `Prerequisites` phase put on the machine
    // (`provision npm via brew` IS a `brew install node`) reads as delivered
    // by the run, or the row says cfgd declared one tool twice and wasted
    // half the install twelve lines under the provision that landed it.
    let delivered = delivered.min(planned - landed);
    let already = planned - landed - delivered;
    let mut parts = Vec::new();
    if already > 0 {
        parts.push(format!("{already} already installed"));
    }
    if delivered > 0 {
        parts.push(format!("{delivered} provisioned by this run"));
    }
    Some(parts.join(", "))
}

/// How many entries an install NAMES, for the two shapes whose executed set
/// can be narrower than their planned one.
fn planned_package_count(action: &Action) -> Option<usize> {
    match action {
        Action::Package(PackageAction::Install { packages, .. }) => Some(packages.len()),
        Action::Module(ModuleAction {
            kind: ModuleActionKind::InstallPackages { resolved },
            ..
        }) => Some(resolved.len()),
        _ => None,
    }
}

/// What a provision actually had to install, for the detail beside its own
/// line: `provision cargo, npm via apt — 1 of 2 managers`.
///
/// A provision node promises an AVAILABLE manager, not a second run of an
/// installer that is minutes of work and idempotent for nobody — so an earlier
/// node, or the `Prerequisites` phase, may have already delivered one of the
/// managers this node names. The subject stays the planned set in both trees
/// (it is one string across the preview bullet, the alignment column and the
/// executed row), so the count is the only seam that can say the run landed
/// fewer, and it is the executor's own re-read carried out on
/// [`ActionRun::installed`] exactly as the package arm's is.
///
/// No count when the node landed everything it named: its subject already
/// names every manager it provisions, so a trailing `— 2 managers` could only
/// restate the row. The same rule the two arms above state for their own
/// subjects.
///
/// And what it DELIVERED: the version each landed manager's own binary
/// reports (`PackageManager::tool_version`, re-read by the executor after its
/// verification), the one fact about a provision its subject cannot already
/// hold. A node naming one manager states the version bare (`— 4.6.3`); a
/// batch names each (`— npm 11.4.2, pipx 1.7.1`); a shortfall keeps its count
/// and parenthesises what did land (`— 1 of 2 managers (npm 11.4.2)`). A
/// manager that answers no version leaves the slot as it was, so a preview
/// and a mock render exactly as before.
fn provisioned_managers_summary(
    action: &Action,
    installed: Option<usize>,
    versions: &[(String, String)],
) -> Option<String> {
    let Action::Manager(node @ ManagerAction::Provision { .. }) = action else {
        return None;
    };
    let members = node.provisioned_managers();
    let planned = members.len();
    let shortfall = installed.filter(|landed| *landed < planned).map(|landed| {
        format!(
            "{landed} of {planned} {}",
            crate::plural_noun(planned, "manager")
        )
    });
    let delivered = (!versions.is_empty()).then(|| {
        if planned == 1 && versions.len() == 1 {
            versions[0].1.clone()
        } else {
            versions
                .iter()
                .map(|(manager, version)| format!("{manager} {version}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    });
    match (shortfall, delivered) {
        (Some(count), Some(delivered)) => Some(format!("{count} ({delivered})")),
        (Some(count), None) => Some(count),
        (None, delivered) => delivered,
    }
}

/// The fact an action PRODUCES, worded for the detail slot of its own row —
/// the ONE producer both trees read, so the plan's bullet and the apply's
/// status line state the same count one beat apart, and `-o json`'s plan
/// payload carries it as `detail` rather than folded into `description`.
///
/// `installed`, `delivered` and `versions` are the facts a preview cannot
/// supply: how many of the entries an install NAMED it still had to land, how
/// many of the rest this run's own provisions put there
/// (`Reconciler::delivered_by_this_run`), and what version each manager a
/// provision landed reports. A plan passes `None`, `0` and `&[]` and gets the
/// same detail it always did.
///
/// `None` for an action that produces nothing worth stating.
pub fn action_produced_detail(
    action: &Action,
    installed: Option<usize>,
    delivered: usize,
    versions: &[(String, String)],
) -> Option<String> {
    env_write_summary(action)
        .or_else(|| deploy_files_summary(action))
        .or_else(|| installed_packages_summary(action, installed, delivered))
        .or_else(|| provisioned_managers_summary(action, installed, versions))
}

/// The widest detail [`action_produced_detail`] can settle `action`'s row
/// with, for a report pricing its column BEFORE the run: every shortfall the
/// executor may re-read, worded through the same producer. A preview's own
/// `None` priced `— 2 already installed` at nothing, so the moment the wait
/// term stopped dominating the allowance the produced term was the binding
/// one and under-priced. Versions are not priced: a manager answers one only
/// after it is here, and a provision row carries no other detail to compete
/// with.
pub fn widest_produced_detail(action: &Action) -> Option<String> {
    let planned = planned_package_count(action).unwrap_or(0);
    // A provision prices its own shortfall through `installed` alone.
    (0..=planned)
        .filter_map(|delivered| action_produced_detail(action, Some(0), delivered, &[]))
        .max_by_key(|detail| crate::output::measure_width(detail))
}

/// A planned action that is a no-op by construction. Its subject already states
/// why nothing happened, so the tree renders it at the role that text implies
/// and attaches no `unchanged` detail.
///
/// An unknown system key keeps `Role::Warn` — it is almost always a typo, and
/// `format_plan_items` branches on the same flag, so the warning-versus-neutral
/// distinction the deleted bespoke lines carried survives as the action's role.
fn declared_noop_role(action: &Action) -> Option<Role> {
    match action {
        Action::System(SystemAction::Skip { unknown: true, .. }) => Some(Role::Warn),
        Action::System(SystemAction::Skip { .. })
        | Action::File(FileAction::Skip { .. })
        | Action::Package(PackageAction::Skip { .. })
        | Action::Secret(SecretAction::Skip { .. })
        | Action::Module(ModuleAction {
            kind: ModuleActionKind::Skip { .. },
            ..
        }) => Some(Role::Skipped),
        _ => None,
    }
}

/// What a FAILED action's row shows in the elapsed slot: whether it ran.
///
/// The row slot measures what RAN, not what succeeded — the same rule the
/// success arm states for itself, where a threshold would make the suffix's
/// absence ambiguous between "fast" and "not measured". A failed `apt-get
/// install rustc` fetched and unpacked a whole dependency closure; untimed, it
/// left the run's `(N.Ns wall)` total exceeding the sum of its visible rows
/// with nothing on screen to account for the difference.
///
/// Two failure shapes genuinely ran nothing and stay untimed. A `Refuse` node
/// IS the refusal — it runs no command by construction — and a dependent the
/// coordinator swept was never dispatched at all. Both are asked HERE rather
/// than inferred from a near-zero `elapsed`, which the duration floor would
/// render as `(<0.1s)` and so state as a measurement.
struct FailureDisplay {
    detail: String,
    continue_on_err: bool,
    ran: bool,
}

pub(super) fn failed_action_ran(action: &Action, error: &CfgdError) -> bool {
    !matches!(action, Action::Manager(ManagerAction::Refuse { .. }))
        && !matches!(
            error,
            CfgdError::Package(PackageError::DependencyFailed { .. })
        )
}

/// The role a SUCCESSFUL action's line settles at.
///
/// The ONE derivation, read by the line the tree paints and by the `skipped`
/// flag its [`ActionResult`] carries: a run whose footer counts an outcome the
/// glyph above it contradicts is the defect this shares its answer to prevent.
fn settled_success_role(action: &Action, changed: bool) -> Role {
    match (declared_noop_role(action), changed) {
        (Some(role), _) => role,
        (None, true) => Role::Ok,
        (None, false) => Role::Skipped,
    }
}

/// `execute_script` emits a script action's one status line itself, so the tree
/// must not add a second for the two action shapes that reach it.
fn action_reports_its_own_status(action: &Action) -> bool {
    matches!(action, Action::Script(_))
        || matches!(
            action,
            Action::Module(ModuleAction {
                kind: ModuleActionKind::RunScript { .. },
                ..
            })
        )
}

/// The per-phase display inputs every settled action line is built from.
struct PhaseLedger<'p> {
    phase_name: PhaseName,
    /// The subject every action renders under, in both trees: the same string
    /// the preview bullet printed and `align_width` measured.
    subjects: &'p std::collections::HashMap<usize, String>,
}

/// What applying one action produced.
///
/// A struct rather than a tuple because a script action's captured output is a
/// third thing an action can produce, and a third anonymous slot names none of
/// them.
pub(super) struct ActionRun {
    /// The description the journal, the result row and the line all carry.
    pub description: String,
    pub changed: bool,
    /// A script action's captured output.
    pub script_output: Option<String>,
    /// How many of the entries the action NAMED it actually put on the
    /// machine, for an action whose executed set is narrower than its planned
    /// one and only knows by how much once it has run. `None` for every action
    /// that installs nothing and for a preview, which has not run yet.
    pub installed: Option<usize>,
    /// How many of the entries it did NOT install were put there by THIS
    /// run's own provisions (`Reconciler::delivered_by_this_run`), so the row
    /// can tell `already installed` from `provisioned by this run`.
    pub delivered: usize,
    /// The version each manager a provision LANDED reports, `(manager,
    /// version)` in provision order — the executor's re-read after its
    /// verification, and empty for every other action and for a member that
    /// was already here or answers no version.
    pub versions: Vec<(String, String)>,
}

impl ActionRun {
    pub(super) fn new(description: String, changed: bool) -> Self {
        Self {
            description,
            changed,
            script_output: None,
            installed: None,
            delivered: 0,
            versions: Vec::new(),
        }
    }

    /// The same run, carrying what the provision delivered.
    pub(super) fn delivering(self, versions: Vec<(String, String)>) -> Self {
        Self { versions, ..self }
    }

    /// The same run, carrying the count the executor re-read off the machine,
    /// and how many of the rest this run itself delivered.
    pub(super) fn installed(self, installed: usize, delivered: usize) -> Self {
        Self {
            installed: Some(installed),
            delivered,
            ..self
        }
    }
}

/// The detail an action row carries about the copies it took, joined when one
/// action displaced several targets (a module's deploy loop).
///
/// Read from a buffer the DISPATCHER owns rather than off the action's `Ok`
/// value, because a sidecar is taken BEFORE the write it protects: an action
/// that copied a target aside and then failed still displaced nothing, and the
/// copy it left on disk has to be named on its row or it is named nowhere.
/// Two things one row has to say, joined in the order they happened; either
/// half alone when it is the only one.
fn join_detail(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(a), Some(b)) => Some(format!("{a}, {b}")),
        (a, b) => a.or(b),
    }
}

fn sidecar_detail(sidecars: &[SidecarOutcome]) -> Option<String> {
    (!sidecars.is_empty()).then(|| {
        sidecars
            .iter()
            .map(SidecarOutcome::detail)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// One finished action, as its collection point hands it over.
struct SettleInput<'p, 'r> {
    action: &'p Action,
    journal_id: Option<i64>,
    result: Result<ActionRun>,
    /// Every target this action copied aside before displacing it. Owned by the
    /// dispatcher, so a FAILED action still reports the copy it took.
    sidecars: Vec<SidecarOutcome>,
    elapsed: std::time::Duration,
    notes: Vec<ActionNote>,
    body: Vec<String>,
    finished: usize,
    ledger: &'p PhaseLedger<'p>,
    results: &'r mut Vec<ActionResult>,
}

/// What settling produced: the description the journal and the result row
/// carry, whether the run must stop here, and the line the tree will render.
struct Settled {
    desc: String,
    should_abort: bool,
    /// `None` for the two script shapes, whose one line `execute_script`
    /// already emitted; their notes are in `notes` instead.
    outcome: Option<ActionOutcome>,
    notes: Vec<ActionNote>,
}

/// The run's monotonic count of finishes, ticked wherever an action is
/// collected — which is always the coordinator thread, sequential phase and
/// concurrent one alike.
///
/// Separate from the plan-position counter because lanes make the two orders
/// differ, and the wall clock cannot substitute: `utc_now_iso8601` truncates to
/// whole seconds, so two lanes finishing inside one second are unordered by
/// `completed_at`.
#[derive(Default)]
struct Completions(usize);

impl Completions {
    fn next(&mut self) -> usize {
        let index = self.0;
        self.0 += 1;
        index
    }
}

pub(super) fn emit_action_line(
    printer: &Printer,
    section: &SectionGuard<'_>,
    outcome: &ActionOutcome,
) {
    {
        let mut builder = section.action_status(outcome.role, &outcome.subject);
        if crate::output::renderer::action_detail_is_muted(outcome.role, outcome.detail_muted) {
            builder = builder.detail_muted_opt(outcome.detail.as_deref());
        } else {
            builder = builder.detail_opt(outcome.detail.as_deref());
        }
        if let Some(d) = outcome.duration {
            builder = builder.duration(d);
        }
        drop(builder);
    }
    for (target, method) in &outcome.children {
        section.child_row(target.clone(), method.clone());
    }
    // Notes no longer attach here: every note the action produced rides in
    // `outcome.notes` to the run-wide `caveats` collector instead, and
    // renders once as the closing `Caveats` section (`render_caveats`) —
    // see `collect_caveats`'s call sites in `Reconciler::apply`.
    //
    // Held back rather than streamed: two lanes streaming into one log
    // interleave line by line, so a concurrent phase captures its children's
    // output and lays each action's out under the line it belongs to. Empty
    // whenever a live window already showed it, and under `Verbosity::Quiet`.
    if !outcome.body.is_empty() {
        crate::output::OutputWindow::dump_below(printer, section.depth, &outcome.body);
    }
}

/// Append a settled action's notes to the caveat group for the owner that
/// produced them, merging into an existing group rather than opening a
/// second `Caveats` heading for the same `kind:name` token — a module
/// installing through more than one package manager gets one `module:<name>`
/// group carrying every manager's notes, in the order they were collected.
pub(super) fn collect_caveats(
    caveats: &mut Vec<(Owner, Vec<ActionNote>)>,
    owner: &Owner,
    subject: &str,
    notes: Vec<ActionNote>,
) {
    if notes.is_empty() {
        return;
    }
    // The section groups by OWNER, so the action's own line — the only thing
    // naming what a manager spoke about — is gone by the time a caveat renders.
    // Re-tagged here, the one place the note and the action that produced it
    // are both in scope.
    let notes: Vec<ActionNote> = notes
        .into_iter()
        .map(|note| note.attributed_to(subject))
        .collect();
    match caveats.iter_mut().find(|(existing, _)| existing == owner) {
        Some((_, group)) => group.extend(notes),
        None => caveats.push((owner.clone(), notes)),
    }
}

/// Render the run's closing `Caveats` section: every note collected during
/// the run, grouped under the `kind:name` owner that produced it — the same
/// token the phase tree uses. Silent (opens nothing) when every group is
/// empty, so a run that produced no caveats prints nothing extra.
///
/// Both note slots deduplicate by MESSAGE across the whole section, the first
/// occurrence keeping it; a group left holding nothing but repeats opens no
/// heading. A render fold only — the `-o json` payload keeps every note under
/// its own owner.
///
/// Groups render in the order given — deciding THAT order (informational
/// groups first, `cfgd:env`'s re-source reminder last, since it is the one
/// thing the reader must still do) is the caller's job, and
/// `cli::plan_ops::print_caveats` is the one assembler for a real `cfgd
/// apply`; a per-configurator snapshot bridge is the other caller, with a
/// single group of its own.
///
/// Within a group, `Role::Warn` notes render before every other role — a
/// stable partition, so two `Warn`s (or two non-`Warn`s) keep the relative
/// order they were collected in. Settle order among concurrent lanes is not
/// deterministic (a fast manager can finish well before a slower one
/// dispatched first), so a caveat's ROLE, not its arrival time, decides
/// precedence: the reader's attention goes to what needs it before what is
/// merely informational, and the render is reproducible for VHS/acceptance
/// pinning regardless of which lane happened to settle first.
pub fn render_caveats(printer: &Printer, groups: &[(Owner, Vec<ActionNote>)]) {
    if groups.iter().all(|(_, notes)| notes.is_empty()) {
        return;
    }
    // A next step is what the reader does after reading everything the run had
    // to say about itself, so it belongs at the report's FOOT — not indented
    // inside one owner's caveat group, where it reads as a remark about that
    // owner rather than as the run's closing instruction.
    let mut next_steps: Vec<String> = Vec::new();
    // Both note slots deduplicate by MESSAGE, across the whole report. A caveat
    // states a fact about the MACHINE — brew put its completions in one
    // directory, once — and a run that provisions a manager in
    // `Prerequisites` and uses it again in `Packages` files that one fact
    // under two owners, so the section printed it twice with nothing but the
    // owner heading to distinguish the copies. Attributing a machine-level
    // fact to an owner is what produces the duplicate; the first occurrence
    // keeps it, so the note stays under the owner that produced it earliest
    // and the phase order still reads top to bottom. A render fold only: the
    // `-o json` payload keeps every note under its own owner.
    //
    // The MESSAGE, never the composed body: `collect_caveats` re-tags every
    // note with the SUBJECT of the action that produced it, so two copies of
    // one machine fact carry `[brew install gum]` and `[provision brew via
    // curl]` and no two tagged notes on a real run ever compare equal. Keyed
    // on the body the fold could only ever fire in a test that bypassed the
    // attribution — which is exactly what it did, while the hero printed
    // `Bash completion has been installed to` twice.
    let mut reported: Vec<String> = Vec::new();
    {
        let mut section = None;
        for (owner, notes) in groups {
            for note in notes.iter().filter(|n| n.hint) {
                if !next_steps.iter().any(|s| s == &note.message) {
                    next_steps.push(note.message.clone());
                }
            }
            let mut reports: Vec<&ActionNote> = notes
                .iter()
                .filter(|n| !n.hint)
                .filter(|n| {
                    if reported.contains(&n.message) {
                        return false;
                    }
                    reported.push(n.message.clone());
                    true
                })
                .collect();
            // Every report this group held was a repeat, so it opens no
            // heading: an owner label over nothing reads as a group whose
            // contents went missing.
            if reports.is_empty() {
                continue;
            }
            let section = section.get_or_insert_with(|| printer.section_caveats());
            let group = section.section_owner(&owner.label());
            reports.sort_by_key(|note| note.role != Role::Warn);
            for note in reports {
                group.status_simple(note.role, note.body());
            }
        }
    }
    for step in next_steps {
        printer.hint(step);
    }
}

/// Write the outcomes a concurrent dispatch held back as the phase's tree: the
/// same group / `live_column` / status walk a streaming phase runs live, with
/// the groups in `Owner::sort_key` order and each group's actions in plan
/// order. Every outcome it renders is taken out of `recorded`, so what remains
/// is exactly what nothing rendered.
///
/// A group whose actions an abort reached before any of them were considered
/// for dispatch produces nothing — the shortfall is the rollup's to name, not
/// an empty heading's, and that holds for every action an abort stopped
/// whether or not it was downstream of one that failed. An action left
/// outstanding for a reason that is NOT an abort is different: `dispatch_lanes`
/// hands every one of them to `collect` as a synthetic failure (stalled, or a
/// lane that ended without reporting) before it returns, so they ARE in
/// `recorded` and render here like any other failed action — never dispatching
/// is itself the failure being reported, not a reason to say nothing.
///
/// `preopened` is the group whose label the caller already committed — the
/// single-owner case, where the label belongs above the live region rather
/// than above the tree written after it. Its outcomes render into that guard
/// instead of opening a second one under the same name.
fn emit_phase_tree(
    printer: &Printer,
    section: &SectionGuard<'_>,
    phase: &super::types::Phase,
    width: usize,
    recorded: &mut std::collections::HashMap<usize, ActionOutcome>,
    preopened: Option<(&Owner, &SectionGuard<'_>)>,
) {
    for group in phase.groups() {
        let already_open =
            preopened.and_then(|(owner, guard)| (owner == &group.owner).then_some(guard));
        let mut group_section: Option<SectionGuard<'_>> = None;
        for action in &group.actions {
            let Some(outcome) = recorded.remove(&action_key(action)) else {
                continue;
            };
            // Opened HERE, beneath the content it introduces, and that is the
            // right place for it whenever the phase runs several groups at
            // once: scrollback is append-only, so a label committed before the
            // dispatch would be separated from its own tree by every other
            // group's, and each live window is headed by its owner's name
            // already — nothing paints unlabelled while the lanes run.
            let target = match already_open {
                Some(guard) => guard,
                None => group_section.get_or_insert_with(|| {
                    let opened = section.section_owner(&group.owner.label());
                    opened.live_column(width);
                    opened
                }),
            };
            emit_action_line(printer, target, &outcome);
        }
    }
}

/// Identity of one planned action, for correlating the plan-order walk with the
/// dispatch-order walk over the same `OwnerGroup::actions` storage.
///
/// Value equality is wrong here: two groups can plan byte-identical actions
/// (the same package declared by two modules), and those are distinct rows in
/// the journal.
fn action_key(action: &Action) -> usize {
    action as *const Action as usize
}

/// Whether an owner's actions in `phase` run through the concurrent dispatcher
/// rather than sequentially.
///
/// The ONE partition, so the phase's two halves cannot both claim an action or
/// both disown it: all of `Packages`, and only the `cfgd:managers` group of
/// `Prerequisites` — its other two groups write the env file and refresh the
/// live session, which are one file and one session and contend with each
/// other rather than with a manager's binary.
fn dispatched_in_lanes(phase: &PhaseName, owner: &Owner) -> bool {
    match phase {
        PhaseName::Packages => true,
        PhaseName::Prerequisites => owner.is_managers(),
        _ => false,
    }
}

/// The ONE fold from a module's parts to one recorded digest, shared by every
/// per-module hash cfgd stores: declaration order says nothing about the
/// machine, so the parts sort before they are joined and two spellings of one
/// module cannot record two different digests.
pub(super) fn hash_sorted_parts(mut parts: Vec<String>) -> String {
    parts.sort();
    crate::sha256_hex(parts.join("|").as_bytes())
}

/// Whether `action` (owned by `owner`, residing in `phase_name`) should execute
/// under `filter`.
///
/// `--phase modules` is an OWNER filter: module work applies in the phase whose
/// kind it is, so every module-owned action matches it wherever it landed.
///
/// `--phase post-scripts` / `--phase pre-scripts` are intentionally inclusive
/// across plan phases: a module lifecycle script is
/// `Action::Module(RunScript { phase: PostApply | ... })`, which a naive
/// `phase.name == filter` test would drop, making
/// `cfgd apply --module nvim --phase post-scripts` a no-op even when failed
/// module scripts need re-attempting. Other filters keep strict
/// phase-equality semantics.
///
/// `PhaseFilter::Selector(name, selector)` (the `<phase>.<selector>` grammar,
/// e.g. `prerequisites.managers`) is stricter still: it never inherits the
/// post/pre-scripts cross-phase leak above, because a selector already names
/// something narrower than a whole phase.
pub fn action_matches_phase_filter(
    phase_name: &PhaseName,
    owner: &Owner,
    action: &Action,
    filter: &PhaseFilter,
) -> bool {
    let filter_phase = match filter {
        PhaseFilter::ModuleOwners => return owner.kind == OwnerKind::Module,
        PhaseFilter::Phase(name) => name,
        PhaseFilter::Selector(name, selector) => {
            return phase_name == name && selector_matches(owner, action, selector);
        }
    };
    if phase_name == filter_phase {
        return true;
    }
    match filter_phase {
        PhaseName::PostScripts => is_post_apply_script(action),
        PhaseName::PreScripts => is_pre_apply_script(action),
        _ => false,
    }
}

/// The `<selector>` half of a dotted phase filter: either one of the closed
/// cfgd owner-group names (`managers`/`env`/`session`) or a manager name.
///
/// A manager selector matches on [`ManagerAction::filter_subject`] directly
/// rather than through `Owner`, because every [`ManagerAction`] shares the
/// single `cfgd:managers` owner — the manager identity lives on the action,
/// not the owner. Sub-managers are already collapsed onto their family at
/// plan time (`managers.rs`), so `prerequisites.brew` matching `brew-cask`'s
/// plan node costs nothing extra here. `filter_subject` (not
/// [`ManagerAction::manager`]) keys a prerequisite node on its TOOL rather
/// than its installer, so this matcher agrees with `cfgd`'s own
/// `action_path`/`pattern_matches_action` on which node
/// `prerequisites.curl` reaches.
fn selector_matches(owner: &Owner, action: &Action, selector: &str) -> bool {
    if super::types::CFGD_GROUP_ORDER.contains(&selector) {
        return owner.kind == OwnerKind::Cfgd && owner.name == selector;
    }
    matches!(action, Action::Manager(node) if node.selector_names(selector))
}

/// Suffix `apply_env_action` appends to a description when the surface was
/// already correct and nothing was written.
///
/// Reading it back out of a description is not a general description sniff: the
/// suffix is confined to env-action descriptions, and every reader below gets
/// its string straight from `apply_env_action`, so the shape is the producer's
/// own data. A converged surface also has to be stripped of it before the
/// description becomes a `managed_resources` id, or one resource owns two rows
/// that alternate by run and neither matches the id the planner derives.
pub(super) const ENV_SKIPPED_SUFFIX: &str = ":skipped";

/// Suffix `apply_env_action` appends to `LIVE_SESSION_RESOURCE_ID` when the
/// refresh could not reach any session manager, rather than converging with
/// nothing to do — see `SessionRefresh::unavailable`. A sibling of
/// [`ENV_SKIPPED_SUFFIX`] rather than a variant of it: both mean "nothing was
/// written", but only this one has a distinct display detail
/// (`"no session manager"` instead of `"unchanged"`), and every consumer that
/// strips one for the persisted id strips the other identically.
pub(super) const ENV_NO_SESSION_MANAGER_SUFFIX: &str = ":no-session-manager";

/// How many of `results` the plan withheld before the run: the rows carrying
/// a [`ActionResult::not_attempted`] reason, which every stored total and every
/// `failed = len - …` subtraction has to leave out.
fn not_attempted_count(results: &[ActionResult]) -> usize {
    results.iter().filter(|r| r.not_attempted.is_some()).count()
}

fn env_result_key(description: &str) -> &str {
    description
        .strip_suffix(ENV_SKIPPED_SUFFIX)
        .or_else(|| description.strip_suffix(ENV_NO_SESSION_MANAGER_SUFFIX))
        .unwrap_or(description)
}

/// What a system arm's result description says it DID, split off the composed
/// id it says it did it to: `system:sysctl.vm.swappiness (60 → 10)`, and
/// `system:sysctl (skipped)` for a configurator this host has nothing to apply
/// through. [`None`] for every other action's description.
///
/// The description itself keeps the decoration — it is the wire contract — but
/// the persisted id may not carry it, for the same reason
/// [`ENV_SKIPPED_SUFFIX`] is stripped before one: a drift row is keyed on
/// [`super::system_resource_key`]'s output alone, so an id carrying the
/// transition matches no row any producer wrote and every value the setting
/// ever held leaves its own `managed_resources` row behind. The prefix test is
/// what keeps this off a file description, where a legal path may hold ` (`.
fn system_result_parts(description: &str) -> Option<(&str, &str)> {
    if !description.starts_with("system:") {
        return None;
    }
    let (key, did) = description.strip_suffix(')')?.split_once(" (")?;
    Some((key, did))
}

/// The decoration a system arm appends for a configurator it applied nothing
/// through.
const SYSTEM_SKIPPED_DETAIL: &str = "skipped";

/// Whether an env-action description carries either "nothing was written"
/// suffix — the general form `.contains(ENV_SKIPPED_SUFFIX)` calls used before
/// this suffix existed, now covering both.
fn env_result_unchanged(description: &str) -> bool {
    description.contains(ENV_SKIPPED_SUFFIX) || description.contains(ENV_NO_SESSION_MANAGER_SUFFIX)
}

/// Whether the post-phase env regeneration must re-run over `path_dirs_now`,
/// the recorded PATH directories as they stand once every phase has run.
///
/// Order-insensitive on purpose: `path_dirs_now` reads back
/// `ORDER BY manager` (alphabetical, `state::bootstrapped_managers`) while
/// `path_dirs_at_plan` appends a newly-provisioned manager's declared dirs in
/// plan declaration order, so the same SET of directories can legally list in
/// a different order on each side without either side being wrong — a run
/// that provisions a manager alphabetically ahead of one already recorded
/// must not be told PATH "changed" just because grouping by manager name
/// reordered it.
pub(super) fn path_dirs_changed(
    path_dirs_now: &[ManagerPathDir],
    path_dirs_at_plan: &[ManagerPathDir],
) -> bool {
    // Sorted on the rendered pair: the generated line names the manager beside
    // the directory, so a directory that changed hands is a content change the
    // regeneration has to pick up.
    let key = |d: &ManagerPathDir| (d.dir.clone(), d.manager.clone());
    let mut now_sorted: Vec<_> = path_dirs_now.iter().map(key).collect();
    now_sorted.sort();
    let mut at_plan_sorted: Vec<_> = path_dirs_at_plan.iter().map(key).collect();
    at_plan_sorted.sort();
    now_sorted != at_plan_sorted
}

/// Fold a late env regeneration into the result the Env phase already recorded
/// for the same surface.
///
/// The regeneration rewrites files the Env phase may have written earlier in the
/// same apply. Appending a second result would report one `~/.cfgd.env` twice and
/// push `results.len()` past the planned-action count every caller compares it
/// against. A prior *failure* is left standing as its own row: a failed attempt
/// and a later successful one are distinct events and collapsing them would hide
/// the error.
pub(super) fn merge_env_result(
    results: &mut Vec<ActionResult>,
    action: &Action,
    registry: &ProviderRegistry,
    description: String,
    changed: bool,
) {
    let key = env_result_key(&description);
    if let Some(prev) = results
        .iter_mut()
        .find(|r| r.success && env_result_key(&r.description) == key)
    {
        prev.changed = prev.changed || changed;
        // The row the Env phase settled said "unchanged" and wore a skip dash;
        // a regeneration that has now written the file makes that verdict
        // stale, and a stale skip is a success missing from the tally.
        prev.skipped = prev.skipped && !prev.changed;
        prev.description = if prev.changed {
            key.to_string()
        } else {
            description
        };
        return;
    }
    // The regeneration is not in the plan, but it replays a real action, so the
    // row it heals comes from the same producer the tick records through — a
    // key derived from the description instead would be a third spelling, and
    // the session surface's own row is not the one its description parses to.
    let drift_rows = super::action_drift_rows(action, registry)
        .into_iter()
        .map(|row| row.key())
        .collect();
    results.push(ActionResult {
        // These are env actions no matter which late input triggered them, and a
        // caller filtering results by phase must find them where every other
        // `env:write:`/`env:inject:` result sits.
        phase: PhaseName::Prerequisites.as_str().to_string(),
        description,
        success: true,
        error: None,
        changed,
        // The same verdict the Env phase's own row settles on: a write that
        // changed nothing is a no-op, whichever late input triggered it.
        skipped: !changed,
        not_attempted: None,
        installed: None,
        versions: Default::default(),
        drift_rows,
    });
}

fn is_post_apply_script(action: &Action) -> bool {
    matches!(
        action,
        Action::Script(ScriptAction::Run {
            phase: ScriptPhase::PostApply | ScriptPhase::PostReconcile,
            ..
        }) | Action::Module(ModuleAction {
            kind: ModuleActionKind::RunScript {
                phase: ScriptPhase::PostApply | ScriptPhase::PostReconcile,
                ..
            },
            ..
        })
    )
}

fn is_pre_apply_script(action: &Action) -> bool {
    matches!(
        action,
        Action::Script(ScriptAction::Run {
            phase: ScriptPhase::PreApply | ScriptPhase::PreReconcile,
            ..
        }) | Action::Module(ModuleAction {
            kind: ModuleActionKind::RunScript {
                phase: ScriptPhase::PreApply | ScriptPhase::PreReconcile,
                ..
            },
            ..
        })
    )
}

impl<'a> super::Reconciler<'a> {
    /// Update module state in state.db after a successful apply.
    fn update_module_state(
        &self,
        modules: &[ResolvedModule],
        apply_id: Option<i64>,
        results: &[ActionResult],
    ) -> Result<()> {
        for module in modules {
            // Check if any module action for this module failed
            let module_prefix = format!("module:{}:", module.name);
            let any_failed = results
                .iter()
                .any(|r| r.description.starts_with(&module_prefix) && !r.success);
            let status = if any_failed {
                crate::state::MODULE_STATUS_ERROR
            } else {
                crate::state::MODULE_STATUS_INSTALLED
            };

            let packages_hash = hash_sorted_parts(
                module
                    .packages
                    .iter()
                    .map(|p| {
                        format!(
                            "{}:{}:{}",
                            p.manager,
                            p.resolved_name,
                            p.version.as_deref().unwrap_or("")
                        )
                    })
                    .collect(),
            );

            let files_hash = hash_sorted_parts(
                module
                    .files
                    .iter()
                    .map(|f| format!("{}:{}", f.source.display(), f.target.display()))
                    .collect(),
            );

            // Collect git source info
            let git_sources: Vec<serde_json::Value> = module
                .files
                .iter()
                .filter(|f| f.is_git_source)
                .map(|f| {
                    serde_json::json!({
                        "source": f.source.display().to_string(),
                        "target": f.target.display().to_string(),
                    })
                })
                .collect();
            let git_sources_json = if git_sources.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&git_sources).unwrap_or_default())
            };

            self.state.upsert_module_state(
                &module.name,
                apply_id,
                &packages_hash,
                &files_hash,
                git_sources_json.as_deref(),
                status,
            )?;
        }
        Ok(())
    }

    /// Record the module bookkeeping for a run that had NOTHING to execute.
    ///
    /// A run whose plan is empty never reaches [`Self::apply`], so the only
    /// writer of `module_state` never fires — and since a module's packages are
    /// elided from the plan once the manager already holds them, an empty plan
    /// is exactly what a converged packages-only module produces. Without this
    /// the module reads `NotApplied` in both `cfgd status` and `cfgd module
    /// list` forever, on a machine where it is fully converged, and its
    /// `packages_hash` keeps describing a declared set that has since changed.
    ///
    /// Only correct for a run that planned NOTHING AT ALL: every module in
    /// `modules` is recorded `installed`, which is a claim about the whole
    /// module, not about the subset a phase filter admitted.
    pub fn record_converged_modules(&self, modules: &[ResolvedModule]) -> Result<()> {
        self.update_module_state(modules, None, &[])
    }

    /// Apply a plan, executing each phase in order.
    /// Failed actions are logged and skipped — they don't abort the entire apply.
    ///
    /// `shell_override` forces every inline lifecycle script to run under the
    /// supplied interpreter, ignoring entries' `shell:` field. Set by
    /// `cfgd apply --shell <shell>` for debugging. File/shebang scripts are
    /// unaffected.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        plan: &Plan,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        phase_filter: Option<&PhaseFilter>,
        module_actions: &[ResolvedModule],
        context: ReconcileContext,
        skip_scripts: bool,
        shell_override: Option<ScriptShell>,
        abort: &AbortFlag,
    ) -> Result<ApplyResult> {
        // Record apply up front as "in-progress" so the journal can reference it
        let plan_hash = crate::state::plan_hash(&plan.to_hash_string());
        // What this run was SCOPED to, which is not always a profile: a
        // `--module` run resolves none, and the caller says so with
        // `module:<name>`. An empty string is the honest record of a scope
        // nothing could name — every surface reading this column omits its row
        // rather than showing a placeholder, and a placeholder stored here is
        // one no reader can tell from a profile genuinely called that.
        let scope = self.recorded_scope.clone().unwrap_or_else(|| {
            resolved
                .layers
                .last()
                .map(|l| l.profile_name.clone())
                .unwrap_or_default()
        });
        let apply_id =
            self.state
                .record_apply(&scope, &plan_hash, ApplyStatus::InProgress, None)?;

        // Filter-aware count of the actions this run intends to execute, using
        // the SAME predicate as the loop below — so an aborted run reports
        // "{applied} of {planned_total}" against only the in-scope actions, not
        // the whole plan. The attemptability half is asked through
        // `attempted_count`, never respelled here: a second copy is how the
        // header once promised a row the tree did not draw.
        let planned_total: usize = plan
            .phases
            .iter()
            .map(|phase| match phase_filter {
                Some(filter) => super::attempted_count(
                    phase
                        .owned_actions()
                        .filter(|(owner, a)| {
                            action_matches_phase_filter(&phase.name, owner, a, filter)
                        })
                        .map(|(_, a)| a),
                ),
                None => phase.action_count(),
            })
            .sum();

        let mut results = Vec::new();
        // Running base of the plan-position counter: `action_index` is dense
        // from 0 across the whole run, over the actions that survive
        // `phase_filter`.
        let mut plan_index_base: usize = 0;
        let mut completions = Completions::default();
        let mut secret_env_collector: Vec<(String, String)> = Vec::new();
        // The PATH directories the Env phase's planned content was built from.
        // `plan()` folds a to-be-provisioned manager's OWN declared dirs into
        // the Env phase's write (`managers::fold_provision_path_dirs`), so
        // this baseline must fold the SAME way against the SAME
        // Prerequisites-phase Provision actions — otherwise it is a pre-run
        // snapshot missing every manager this run is about to bootstrap, and
        // the comparison below flags ordinary, successful provisioning as
        // drift.
        let path_dirs_at_plan = super::managers::fold_provision_path_dirs(
            self.registry,
            plan.phases
                .iter()
                .find(|phase| phase.name == PhaseName::Prerequisites)
                .into_iter()
                .flat_map(|phase| phase.actions()),
            super::env::recorded_manager_path_dirs(self.state, &resolved.merged, module_actions),
        );
        // Set when a signal requested cooperative cancellation. Stopping happens BEFORE
        // the next action — the previous one already completed atomically, so no
        // file is left torn.
        let mut aborted_code: Option<u8> = None;
        // Post-install notes a manager produced during ONE action, drained
        // after that action's status line so they render attached to it.
        let notes = NoteSink::default();
        // Provider narration collected across the whole run, grouped by owner,
        // and rendered once as the closing `Caveats` section instead of inline
        // under each action (see `collect_caveats` / `render_caveats`).
        let mut caveats: Vec<(Owner, Vec<ActionNote>)> = Vec::new();
        // Library code reached from inside a phase or owner section renders at
        // that section's depth for the whole run: package-manager output
        // windows, script windows and every status they collapse into.
        let _inherit = printer.depth_inheritance();

        // One column for the tree, measured over every action any phase of it
        // will print. A width taken inside the loop moves the trailing column
        // between one phase and the next, which reads as a wobble down a page
        // whose whole point is that the column can be scanned. The run
        // skeleton claims a report-wide column over this and its
        // pseudo-phases; this is the value when a caller drives the reconciler
        // without one.
        let budget = printer.subject_budget();
        let width = super::run::report_align_width(plan, phase_filter, budget, printer.arrow());

        'phases: for phase in &plan.phases {
            // Plan positions of the actions in this phase that survive
            // `phase_filter`, in `Phase::actions()` order. `action_index` is
            // documented as "where this action sits in the plan", and Rule P
            // dispatches `Packages` out of plan order, so the value is read from
            // this map rather than counted at dispatch. Address identity is the
            // key because both walks borrow the same `OwnerGroup::actions`
            // storage, so a reference from either one names the same slot.
            let mut plan_positions: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            for (owner, action) in phase.owned_actions() {
                if let Some(filter) = phase_filter
                    && !action_matches_phase_filter(&phase.name, owner, action, filter)
                {
                    continue;
                }
                // A module skipped whole is the header's `Modules`-row clause,
                // not work: the tree draws no row for it and the plan promised
                // none, so dispatching it would settle an outcome and a
                // `skipped` tally against a row nobody saw.
                if is_module_skip(action) {
                    continue;
                }
                let next = plan_positions.len();
                plan_positions.insert(action_key(action), next);
            }

            if plan_positions.is_empty() {
                continue;
            }

            let total = plan_positions.len();
            // Rule P's three-tier partition (`0 → B → 1`), which is plain plan
            // order outside `Packages`. Membership in `plan_positions` is the
            // `phase_filter` test: the map was built from the same predicate
            // over the same actions, so an item missing from it is exactly one
            // the filter excluded.
            let dispatch: Vec<(&Owner, &Action, usize)> = phase
                .dispatch_order()
                .filter_map(|(owner, action)| {
                    plan_positions
                        .get(&action_key(action))
                        .map(|pos| (owner, action, *pos))
                })
                .collect();

            // The subject every action renders under, in both trees: the same
            // string the preview bullet printed and `align_width` measured.
            let subjects: std::collections::HashMap<usize, String> = phase
                .groups()
                .iter()
                .flat_map(|group| group.actions.iter())
                .map(|action| {
                    (
                        action_key(action),
                        action_display_subject_within(action, budget, printer.arrow()).to_string(),
                    )
                })
                .collect();
            let ledger = PhaseLedger {
                phase_name: phase.name.clone(),
                subjects: &subjects,
            };
            // The concurrent actions of this phase — all of `Packages`, and the
            // `cfgd:managers` group of `Prerequisites`, whose nodes are a DAG
            // over the same family lanes. The rest of the phase runs
            // sequentially AFTER them: `cfgd:env` publishes where the binaries
            // the managers group just created live, so producer precedes
            // consumer inside the phase as well as across it.
            let (lane_dispatch, serial_dispatch): (Vec<_>, Vec<_>) = dispatch
                .into_iter()
                .partition(|(owner, _, _)| dispatched_in_lanes(&phase.name, owner));
            // The lane half's tree is written the moment the lanes drain, and
            // the serial half streams after it, so the phase reads in
            // `Owner::sort_key` order only while every lane group sorts above
            // every serial one. `Packages` hands everything to a lane and
            // `Prerequisites` leads with `cfgd:managers`; a third partition
            // that did not would print its groups out of order.
            debug_assert!(
                lane_dispatch.iter().all(|(lane_owner, _, _)| {
                    serial_dispatch
                        .iter()
                        .all(|(serial_owner, _, _)| lane_owner.renders_above(serial_owner))
                }),
                "a serially dispatched group sorts above a lane group in {}",
                phase.name.as_str()
            );

            // The lane half's dispatch order is not its reading order — Rule P
            // dispatches `Packages` `0 -> B -> 1` while its groups read in
            // `Owner::sort_key` order, and the `cfgd:managers` nodes finish in
            // whatever order their lanes do — so it holds its outcomes and
            // writes them as a tree the moment the lanes drain. The serial
            // half streams, because there the two walks are the same one.
            let mut recorded: std::collections::HashMap<usize, ActionOutcome> =
                std::collections::HashMap::new();
            // Platform-gated skips are the header's `Modules`-row annotation,
            // so the phase holding them opens no block at all.
            let phase_section = (phase.name != PhaseName::Modules)
                .then(|| printer.section_phase(&phase.name.section_label()));
            // The flat dispatch stream converted back to the nested render
            // shape: a new group guard opens whenever the owner changes, and
            // the previous one closes first. Outside `Packages` an owner's
            // actions are contiguous in the stream by construction, so this is
            // one guard per group, in group order.
            let mut owner_section: Option<SectionGuard<'_>> = None;
            let mut owner_open: Option<&Owner> = None;
            // Why the action loop stopped, resolved after the phase's tree is
            // written — a deferred phase must still render what it completed.
            let mut abort_stop: Option<u8> = None;
            let mut pre_script_stop: Option<String> = None;

            // The one owner every lane action of this phase belongs to, when
            // there is one. `Prerequisites` always has one (`cfgd:managers`);
            // `Packages` has one per module plus the profile's.
            let mut lane_owners = lane_dispatch.iter().map(|(owner, _, _)| *owner);
            let sole_lane_owner = lane_owners
                .next()
                .filter(|first| lane_owners.all(|owner| owner == *first));
            // A single-owner lane phase commits that label BEFORE its lanes
            // paint anything. The live region draws below the last committed
            // line, so a label held back until the tree is written lands under
            // the very windows and wait bars it introduces.
            let lane_group = phase_section
                .as_ref()
                .zip(sole_lane_owner)
                .map(|(section, owner)| {
                    let group = section.section_owner(&owner.label());
                    group.live_column(width);
                    group.commit_header();
                    group
                });

            if !lane_dispatch.is_empty() {
                // The concurrent dispatcher owns these actions: it opens each
                // action's journal row at its dispatch point, runs the work in
                // a per-manager lane, and hands every finish back HERE, on this
                // thread, in completion order. Where each finish LANDS is the
                // tree's: on a terminal every action has a row from the moment
                // the dispatcher first has something to say about it, and the
                // finish settles that row in place. Off one there are no rows,
                // and the outcomes are held for `emit_phase_tree` below.
                // Taken before the dispatch opens, since `settle` below keeps
                // writing to the list while these lanes run.
                let unprovisioned = self.unprovisioned.borrow().clone();
                let provisioned = self.provisioned.borrow().clone();
                let provisioned_packages = self.provisioned_packages.borrow().clone();
                let run = super::lanes::LaneRun {
                    printer,
                    apply_id,
                    phase: &phase.name,
                    config_dir,
                    resolved,
                    module_actions,
                    context,
                    shell_override,
                    abort,
                    plan_index_base,
                    action_depth: phase_section.as_ref().map_or(0, |s| s.depth + 1),
                    unprovisioned: &unprovisioned,
                    provisioned: &provisioned,
                    provisioned_packages: &provisioned_packages,
                };
                let mut tree = super::live_tree::PhaseTree::new(
                    printer,
                    phase_section.as_ref(),
                    sole_lane_owner.zip(lane_group.as_ref()),
                    run.action_depth,
                    width,
                );
                // Asked once, of the tree that will answer for it: the two
                // halves of this decision must never disagree, or an outcome
                // is rendered twice or not at all.
                let settles_in_place = tree.is_live();
                let mut settle =
                    |owner: &Owner, action: &Action, collected: super::lanes::LaneCollected| {
                        let finished = completions.next();
                        let settled = self.settle_action(SettleInput {
                            action,
                            journal_id: collected.journal_id,
                            result: collected.result,
                            // A lane runs package and manager actions, neither
                            // of which adopts a file.
                            sidecars: Vec::new(),
                            elapsed: collected.elapsed,
                            notes: collected.notes,
                            body: collected.body,
                            finished,
                            ledger: &ledger,
                            results: &mut results,
                        });
                        match settled.outcome {
                            Some(outcome) if settles_in_place => {
                                collect_caveats(
                                    &mut caveats,
                                    owner,
                                    &outcome.subject,
                                    outcome.notes.clone(),
                                );
                                Some(outcome)
                            }
                            Some(outcome) => {
                                collect_caveats(
                                    &mut caveats,
                                    owner,
                                    &outcome.subject,
                                    outcome.notes.clone(),
                                );
                                recorded.insert(action_key(action), outcome);
                                None
                            }
                            // An action that reported its own status carries its
                            // notes beside the outcome rather than inside it, and a
                            // held-back tree has no line open to attach them under.
                            // Unreachable: only the two script shapes self-report,
                            // and neither is ever dispatched into a lane.
                            None => {
                                debug_assert!(
                                    settled.notes.is_empty(),
                                    "a self-reporting action reached a lane carrying notes"
                                );
                                collect_caveats(&mut caveats, owner, &settled.desc, settled.notes);
                                None
                            }
                        }
                    };
                abort_stop = self.dispatch_lanes(
                    &lane_dispatch,
                    &run,
                    &mut super::lanes::LaneCollector::new(&mut settle, &mut tree),
                );
                // Committed HERE, not at phase close: whatever the phase does
                // next renders below the live region, so the region has to be
                // down first. `Prerequisites` is the phase that needs it — its
                // `cfgd:env` and `cfgd:session` groups run in the serial half
                // below and stream their own lines, which would land ABOVE the
                // managers group they follow if this waited.
                tree.finish();
                // Empty on a terminal — the tree settled every line as it
                // happened — and the whole phase off one.
                if let Some(section) = phase_section.as_ref() {
                    emit_phase_tree(
                        printer,
                        section,
                        phase,
                        width,
                        &mut recorded,
                        sole_lane_owner.zip(lane_group.as_ref()),
                    );
                }
            }
            // Closed before the serial half opens a group of its own: the
            // renderer's section stack unwinds in order, and a lane group left
            // open would nest the phase's remaining groups inside it.
            drop(lane_group);

            // Whatever the phase did not hand to a lane, in plan order. An
            // abort during the lane half stops here: the phase's remaining work
            // is exactly the work a cancelled run must not begin.
            if abort_stop.is_none() {
                for (owner, action, plan_index) in serial_dispatch.into_iter() {
                    let action_index = plan_index_base + plan_index;
                    // Cooperative cancellation: a signal flips the abort flag, and
                    // the loop stops before beginning the next atomic action.
                    if let Some(code) = abort.aborted() {
                        abort_stop = Some(code);
                        break;
                    }
                    if let Some(section) = phase_section.as_ref()
                        && owner_open != Some(owner)
                    {
                        // Explicit close before the next open: assigning over the
                        // binding would build the new guard first and unwind the
                        // renderer's section stack out of order.
                        drop(owner_section.take());
                        let group = section.section_owner(&owner.label());
                        group.live_column(width);
                        // The label lands before the action does: an action
                        // that opens an output window or a spinner paints it
                        // into the live region, which draws below whatever has
                        // been committed — so a label still deferred to this
                        // action's status line would be written after the
                        // output of the action it introduces.
                        group.commit_header();
                        owner_section = Some(group);
                        owner_open = Some(owner);
                    }

                    let desc_for_journal = format_action_description(action);
                    let (action_type, resource_id) =
                        parse_resource_from_description(&desc_for_journal);

                    // Capture file state before overwrite (for backup). A target
                    // that does not yet exist (a CREATE) gets an absent marker so
                    // rollback removes it rather than restoring a later apply's
                    // post-apply snapshot.
                    if let Some(backup) = action_target_path(action) {
                        let path = &backup.path;
                        // Backup key, not display: every writer of
                        // `file_backups.file_path` folds with `to_posix_fs_key` so a
                        // rollback lookup finds the row a Windows apply wrote — and
                        // so the row a rollback reopens still names the file that
                        // was backed up.
                        let path_str = crate::to_posix_fs_key(path);
                        let captured = if backup.follow_symlink {
                            crate::capture_file_resolved_state(path)
                        } else {
                            crate::capture_file_state(path)
                        };
                        match captured {
                            Ok(Some(file_state)) => {
                                if let Err(e) =
                                    self.state
                                        .store_file_backup(apply_id, &path_str, &file_state)
                                {
                                    // tracing-ok: the rollback copy could not be stored; no row states it, the write it protects settles on its own
                                    tracing::warn!(
                                        "failed to store file backup for {}: {}",
                                        path.posix(),
                                        e
                                    );
                                }
                            }
                            Ok(None) => {
                                if let Err(e) = self.state.store_absent_backup(apply_id, &path_str)
                                {
                                    // tracing-ok: same, for the CREATE marker a rollback deletes by
                                    tracing::warn!(
                                        "failed to store absent marker for {}: {}",
                                        path.posix(),
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                // tracing-ok: same, one step earlier - the target could not be read at all
                                tracing::warn!(
                                    "failed to capture file state for backup of {}: {}",
                                    path.posix(),
                                    e
                                );
                            }
                        }
                    }

                    // Journal: record action start
                    let journal_id = self
                        .state
                        .journal_begin(
                            apply_id,
                            action_index,
                            phase.name.as_str(),
                            &action_type,
                            &resource_id,
                            None,
                        )
                        .ok();

                    let started = std::time::Instant::now();
                    // Owned HERE rather than returned in the `Ok` value: a copy
                    // is taken before the write it protects, so a failing write
                    // must still report it.
                    let mut action_sidecars = Vec::new();
                    let result = self.apply_action(
                        action,
                        resolved,
                        config_dir,
                        printer,
                        apply_id,
                        context,
                        module_actions,
                        &mut secret_env_collector,
                        shell_override,
                        abort,
                        &notes,
                        &mut action_sidecars,
                    );
                    let elapsed = started.elapsed();
                    let finished = completions.next();
                    // Drained unconditionally: a note left in the sink would
                    // attach to whichever action drains next, which is not the one
                    // that produced it.
                    let drained = notes.take();
                    let settled = self.settle_action(SettleInput {
                        action,
                        journal_id,
                        result,
                        sidecars: action_sidecars,
                        elapsed,
                        notes: drained,
                        body: Vec::new(),
                        finished,
                        ledger: &ledger,
                        results: &mut results,
                    });
                    let should_abort = settled.should_abort;
                    let desc = settled.desc;
                    match settled.outcome {
                        // One status line per plan action, always — except the two
                        // script shapes, whose line `execute_script` already
                        // emitted. Their notes still flow to the run-wide
                        // `caveats` collector rather than attaching here.
                        None => {
                            collect_caveats(&mut caveats, owner, &desc, settled.notes);
                        }
                        // `PhaseName::Modules` opens no block: its only actions
                        // are platform-gated skips, which the header's
                        // `Modules` row already annotates.
                        Some(outcome) => {
                            if let Some(section) = owner_section.as_ref() {
                                emit_action_line(printer, section, &outcome);
                            }
                            collect_caveats(
                                &mut caveats,
                                owner,
                                &outcome.subject,
                                outcome.notes.clone(),
                            );
                        }
                    }

                    // If a signal arrived while the action was running, the execute_script
                    // poll loop already killed the child and returned an error. Treat this
                    // as a cooperative abort (not a script failure) so the correct exit
                    // code (130 for SIGINT) and "aborted" DB row are recorded.
                    if let Some(code) = abort.aborted() {
                        abort_stop = Some(code);
                        break;
                    }

                    // If a pre-script failed without continueOnError, abort
                    let is_pre_script = matches!(
                        action,
                        Action::Script(ScriptAction::Run { phase: sp, .. })
                            if matches!(sp, ScriptPhase::PreApply | ScriptPhase::PreReconcile)
                    ) || matches!(
                        action,
                        Action::Module(ModuleAction {
                            kind: ModuleActionKind::RunScript { phase: sp, .. },
                            ..
                        }) if matches!(sp, ScriptPhase::PreApply | ScriptPhase::PreReconcile)
                    );
                    if should_abort && is_pre_script {
                        pre_script_stop = Some(condense_action_desc_for_display(action, &desc));
                        break;
                    }
                }
            }

            debug_assert!(
                recorded.is_empty(),
                "a lane outcome outlived the tree that was to render it"
            );

            // Guards close in declaration order's reverse, so the phase's tree
            // is complete before the run unwinds past it.
            drop(owner_section);
            drop(phase_section);

            plan_index_base += total;
            if let Some(display_desc) = pre_script_stop {
                return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                    message: format!("pre-script failed, aborting apply: {}", display_desc),
                }));
            }
            if let Some(code) = abort_stop {
                aborted_code = Some(code);
                break 'phases;
            }
        }

        // Cooperative abort: a signal stopped us between actions. Skip the
        // follow-up hooks (secret-env regen, onChange) — those represent a
        // completed apply — but still persist the managed-resource bookkeeping
        // for the actions that did run, then record an `Aborted` marker and
        // return the signal exit code. The lock releases via the caller's Drop.
        if let Some(code) = aborted_code {
            self.record_managed_resources(apply_id, &results, resolved, module_actions)?;
            self.update_module_state(module_actions, Some(apply_id), &results)?;
            let not_attempted = not_attempted_count(&results);
            let succeeded = results
                .iter()
                .filter(|r| r.success && !r.skipped && r.not_attempted.is_none())
                .count();
            let skipped = results.iter().filter(|r| r.success && r.skipped).count();
            // `total` is what the run PLANNED, not what it reached: an aborted
            // run's whole point is that those two numbers differ, and a stored
            // record whose total is the reached count reads as a clean sweep
            // of a smaller plan. `notRun` is the difference stated outright,
            // and it is the only place the actions the abort stopped are
            // accounted for — the dispatcher deliberately reports none of them
            // action by action.
            let not_run = planned_total.saturating_sub(results.len() - not_attempted);
            let summary = crate::state::ApplySummary::Actions {
                total: planned_total,
                succeeded,
                skipped,
                failed: results.len() - not_attempted - succeeded - skipped,
                not_attempted,
                not_run: Some(not_run),
                aborted: true,
            }
            .to_column();
            self.state
                .update_apply_status(apply_id, ApplyStatus::Aborted, Some(&summary))?;
            return Ok(ApplyResult {
                action_results: results,
                status: ApplyStatus::Aborted,
                apply_id,
                aborted: Some(code),
                planned_total,
                caveats,
            });
        }

        // --- Env regeneration: fold in inputs that only exist once the phases ran ---
        // One input lands too late for the Env phase, which by `PhaseName` order
        // runs before both Modules and Packages: a secret's resolved value.
        // Regenerating here converges the file inside the same apply instead of
        // leaving it right only from the next one on.
        //
        // A manager's PATH directories are not a late-arriving input anymore —
        // `plan()` already folds a Provision node's declared `creates_path_dirs`
        // into the planned content, so the Env phase's own write already
        // carries them for every manager whose `path_dirs()` mirrors its
        // `bootstrap_plan()` declaration, and `path_dirs_at_plan` above folds
        // the identical set. Comparing `path_dirs_now` against
        // `path_dirs_at_plan` stays as the convergence net for the one case
        // the planner cannot declare up front — npm, whose resolved global
        // prefix is only knowable once its install finishes — and for any run
        // where what actually got recorded diverges from what was declared.
        let path_dirs_now =
            super::env::recorded_manager_path_dirs(self.state, &resolved.merged, module_actions);
        // A phase-scoped run must stay inside the phase the caller asked for.
        // `--phase modules` bootstrapping a manager would otherwise reach out
        // and rewrite `~/.cfgd.env` plus the source lines in `~/.bashrc` —
        // surfaces the Env phase owns. The bootstrap record is durable either
        // way, so the next unfiltered apply still converges the file.
        let path_dirs_changed =
            phase_filter.is_none() && path_dirs_changed(&path_dirs_now, &path_dirs_at_plan);
        // The regeneration reads the DECLARED env, not the plan, so a caller
        // that pruned the env actions out of its plan would still see the
        // surface written here. `withhold_env_surface` is that caller saying
        // the surface is not this run's to touch; the inputs that would have
        // triggered the regeneration are durable, so the run that stops
        // withholding still converges it.
        if self.withhold_env_surface {
            tracing::debug!("env surface withheld: skipping post-phase regeneration");
        } else if !secret_env_collector.is_empty() || path_dirs_changed {
            let env_plan = self.plan_env(
                &resolved.merged.env,
                &resolved.merged.aliases,
                &resolved.merged.entry_owners,
                resolved.merged.env_scope,
                module_actions,
                &secret_env_collector,
                &path_dirs_now,
                &super::env::recorded_managed_env_files(self.state),
            );
            for env_action in &env_plan.actions {
                if let Action::Env(ea) = env_action {
                    // No phase section is open here and nothing will drain a
                    // sink, so a session-refresh warning settles on its own line
                    // — beside the failure line below it, which does the same.
                    match Self::apply_env_action(ea, printer, NoteSink::discarded()) {
                        Ok(desc) => {
                            let changed = !env_result_unchanged(&desc);
                            merge_env_result(
                                &mut results,
                                env_action,
                                self.registry,
                                desc,
                                changed,
                            );
                        }
                        Err(e) => {
                            printer
                                .status(Role::Fail, "regenerate shell env files")
                                .detail(e.to_string());
                            results.push(ActionResult {
                                phase: PhaseName::Prerequisites.as_str().to_string(),
                                description: format!(
                                    "env:{}:regenerate",
                                    super::env_engine::ENV_VERB_WRITE
                                ),
                                success: false,
                                error: Some(e.to_string()),
                                changed: false,
                                skipped: false,
                                not_attempted: None,
                                installed: None,
                                versions: Default::default(),
                                drift_rows: Vec::new(),
                            });
                        }
                    }
                }
            }
        }

        // --- onChange detection: run profile onChange scripts if anything changed ---
        let any_changed = results.iter().any(|r| r.changed);
        if any_changed && !skip_scripts && !resolved.merged.scripts.on_change.is_empty() {
            let profile_name = resolved
                .layers
                .last()
                .map(|l| l.profile_name.as_str())
                .unwrap_or("unknown");
            let env_vars = build_script_env(&ScriptEnvContext {
                config_dir,
                profile_name,
                context,
                phase: &ScriptPhase::OnChange,
                module_name: None,
                module_dir: None,
                path_dirs: &super::all_recorded_path_dirs(self.state),
            });
            let working = script_default_workdir(config_dir);
            for entry in &resolved.merged.scripts.on_change {
                match execute_script(
                    entry,
                    config_dir,
                    &working,
                    &env_vars,
                    crate::PROFILE_SCRIPT_TIMEOUT,
                    printer,
                    shell_override,
                    Some(abort),
                    ScriptReport {
                        subject: ScriptSubject::Hook(ScriptPhase::OnChange.display_name()),
                        non_fatal: effective_continue_on_error(entry, &ScriptPhase::OnChange),
                        ..ScriptReport::default()
                    },
                ) {
                    Ok((desc, changed, _)) => {
                        results.push(ActionResult {
                            phase: "post-scripts".to_string(),
                            description: desc,
                            success: true,
                            error: None,
                            changed,
                            // A script reports its own outcome line; the tree
                            // settles no role for it and this record invents none.
                            skipped: false,
                            not_attempted: None,
                            installed: None,
                            versions: Default::default(),
                            drift_rows: Vec::new(),
                        });
                    }
                    Err(e) => {
                        let continue_on_err =
                            effective_continue_on_error(entry, &ScriptPhase::OnChange);
                        results.push(ActionResult {
                            phase: "post-scripts".to_string(),
                            description: format!("onChange: {}", entry.run_str()),
                            success: false,
                            error: Some(format!("{}", e)),
                            changed: false,
                            skipped: false,
                            not_attempted: None,
                            installed: None,
                            versions: Default::default(),
                            drift_rows: Vec::new(),
                        });
                        if !continue_on_err {
                            return Err(e);
                        }
                    }
                }
            }
        }

        // --- Module-level onChange: run per-module onChange scripts if that module had changes ---
        if any_changed && !skip_scripts {
            let profile_name = resolved
                .layers
                .last()
                .map(|l| l.profile_name.as_str())
                .unwrap_or("unknown");
            let path_dirs = super::all_recorded_path_dirs(self.state);
            for module in module_actions {
                if module.on_change_scripts.is_empty() {
                    continue;
                }
                let prefix = format!("module:{}:", module.name);
                let module_changed = results
                    .iter()
                    .any(|r| r.changed && r.description.starts_with(&prefix));
                if !module_changed {
                    continue;
                }
                let env_vars = build_module_script_env(
                    &ScriptEnvContext {
                        config_dir,
                        profile_name,
                        context,
                        phase: &ScriptPhase::OnChange,
                        module_name: Some(&module.name),
                        module_dir: Some(&module.dir),
                        path_dirs: &path_dirs,
                    },
                    &module.env,
                );
                let working = script_default_workdir(config_dir);
                for entry in &module.on_change_scripts {
                    match execute_script(
                        entry,
                        &module.dir,
                        &working,
                        &env_vars,
                        MODULE_SCRIPT_TIMEOUT,
                        printer,
                        shell_override,
                        Some(abort),
                        ScriptReport {
                            subject: ScriptSubject::Hook(ScriptPhase::OnChange.display_name()),
                            non_fatal: effective_continue_on_error(entry, &ScriptPhase::OnChange),
                            ..ScriptReport::default()
                        },
                    ) {
                        Ok((desc, changed, _)) => {
                            results.push(ActionResult {
                                phase: "modules".to_string(),
                                description: desc,
                                success: true,
                                error: None,
                                changed,
                                skipped: false,
                                not_attempted: None,
                                installed: None,
                                versions: Default::default(),
                                drift_rows: Vec::new(),
                            });
                        }
                        Err(e) => {
                            let continue_on_err =
                                effective_continue_on_error(entry, &ScriptPhase::OnChange);
                            results.push(ActionResult {
                                phase: "modules".to_string(),
                                description: format!(
                                    "module:{}:onChange: {}",
                                    module.name,
                                    entry.run_str()
                                ),
                                success: false,
                                error: Some(format!("{}", e)),
                                changed: false,
                                skipped: false,
                                not_attempted: None,
                                installed: None,
                                versions: Default::default(),
                                drift_rows: Vec::new(),
                            });
                            if !continue_on_err {
                                return Err(e);
                            }
                        }
                    }
                }
            }
        }

        // `total` is what the run ATTEMPTED: a pre-skipped action has a result
        // row (its reason) and no place in the count the header promised.
        let not_attempted = not_attempted_count(&results);
        let total = results.len() - not_attempted;
        let failed = results.iter().filter(|r| !r.success).count();
        let status = if failed == 0 {
            ApplyStatus::Success
        } else if failed == total {
            ApplyStatus::Failed
        } else {
            ApplyStatus::Partial
        };

        // Update apply status from "in-progress" placeholder to final
        let skipped = results.iter().filter(|r| r.success && r.skipped).count();
        let summary = crate::state::ApplySummary::Actions {
            total,
            succeeded: total - failed - skipped,
            skipped,
            failed,
            not_attempted,
            not_run: None,
            aborted: false,
        }
        .to_column();
        // One transaction for the whole bookkeeping tail. Every write below is a
        // per-row insert in a loop over the run's results, its modules and its
        // touched files; individually committed, a large apply paid one WAL
        // commit per row for work that is only meaningful as a whole.
        //
        // The status update belongs INSIDE it, not before it: the apply row's
        // verdict and the ownership rows describing what the run now owns are
        // one fact. Committed separately, a tail that fails after packages were
        // installed leaves a row reading `Success` beside no `managed_resources`
        // rows at all — the next run's declarative prune cannot reach packages
        // this one installed, because nothing records that cfgd owns them.
        // Rolled back together, the row stays at its `in-progress` placeholder,
        // which is what a run that did not finish its bookkeeping actually is.
        self.state.in_transaction(|| {
            self.state
                .update_apply_status(apply_id, status.clone(), Some(&summary))?;
            self.record_managed_resources(apply_id, &results, resolved, module_actions)?;
            // Update module state and file manifests for successfully applied modules
            self.update_module_state(module_actions, Some(apply_id), &results)?;
            self.snapshot_touched_files(apply_id, resolved, module_actions)
        })?;

        Ok(ApplyResult {
            action_results: results,
            status,
            apply_id,
            aborted: None,
            planned_total,
            caveats,
        })
    }

    /// Post-apply snapshot: capture the resolved content (following symlinks)
    /// of the managed file targets THIS apply touched, so a rollback to it
    /// restores the bytes that were visible the moment it finished — which is
    /// not the same as the pre-action backup for a symlink-deployed file, whose
    /// target resolves through a link the action rewrote.
    ///
    /// Scoped to the touched set, read back from the backup rows the run itself
    /// wrote (a row lands immediately before any file action overwrites its
    /// target, and immediately before any module file is deployed). The
    /// unscoped form re-read and re-stored EVERY managed target in the profile
    /// on every apply — for a converged machine, that is the entire dotfile
    /// tree read, hashed and written into the state DB as blobs to record that
    /// nothing happened. A file the run did not touch still has its content
    /// recorded under the apply that last wrote it, which is the apply a
    /// rollback resolves it through; what is genuinely given up is undoing
    /// OUT-OF-BAND edits to files no apply since has touched, which is drift for
    /// `cfgd apply` to reconcile rather than history for a rollback to rewind.
    fn snapshot_touched_files(
        &self,
        apply_id: i64,
        resolved: &ResolvedProfile,
        modules: &[ResolvedModule],
    ) -> Result<()> {
        let touched = self.state.backed_up_paths_for_apply(apply_id)?;
        if touched.is_empty() {
            return Ok(());
        }
        let mut snapshot_paths = std::collections::HashSet::new();
        let managed_targets = resolved.merged.files.managed.iter().map(|m| &m.target);
        let module_targets = modules
            .iter()
            .flat_map(|module| module.files.iter().map(|f| &f.target));
        for target in managed_targets.chain(module_targets) {
            let target = crate::expand_tilde(target);
            let key = crate::to_posix_fs_key(&target);
            if !touched.contains(&key) || !snapshot_paths.insert(key.clone()) {
                continue;
            }
            if let Ok(Some(state)) = crate::capture_file_resolved_state(&target)
                && let Err(e) = self.state.store_file_backup(apply_id, &key, &state)
            {
                tracing::debug!("post-apply snapshot for {}: {}", key, e);
            }
        }
        Ok(())
    }

    /// Persist managed-resource tracking rows for the successfully-applied
    /// actions in `results`. Shared by the normal completion path and the
    /// cooperative-abort path, which both need state to reflect exactly the
    /// resources that actually changed.
    pub(super) fn record_managed_resources(
        &self,
        apply_id: i64,
        results: &[ActionResult],
        resolved: &ResolvedProfile,
        modules: &[ResolvedModule],
    ) -> Result<()> {
        for result in results {
            if !result.success {
                continue;
            }
            // An action this host was never going to run put nothing on the
            // machine: it manages no resource and heals no finding. The plan
            // already priced it out of the header's total; the store must agree.
            if result.not_attempted.is_some() {
                continue;
            }

            // Packages track per-resolved-name under "package"/"<mgr>/<pkg>" so the
            // set is usable for declarative prune. The generic parser is lossy for
            // multi-package installs and embeds the verb, so handle them explicitly:
            // install adds a tracking row per package, uninstall deletes it.
            if let Some((manager, verb, packages)) = parse_package_description(&result.description)
            {
                self.state
                    .resolve_drift_keys(apply_id, &result.drift_rows)?;
                for pkg in &packages {
                    let rid = crate::state::package_resource_id(&manager, pkg);
                    match verb.as_str() {
                        "install" => {
                            // Persist the scripted uninstall command (Some only for
                            // custom managers) so the package can still be pruned
                            // after its manager block leaves the config.
                            let uninstall_cmd = self
                                .registry
                                .package_managers()
                                .iter()
                                .find(|m| m.name() == manager)
                                .and_then(|m| m.persisted_uninstall());
                            self.state.upsert_package_resource(
                                &rid,
                                LOCAL_LAYER,
                                Some(apply_id),
                                uninstall_cmd.as_deref(),
                            )?;
                        }
                        "uninstall" => {
                            self.state.remove_managed_resource("package", &rid)?;
                        }
                        _ => {}
                    }
                }
                continue;
            }

            let description = env_result_key(&result.description);
            // A configurator that applied nothing manages nothing and heals
            // nothing: its planned `Skip` is the record that the tool is
            // missing, and an apply that ran it is exactly as unable as the
            // tick that recorded it. Everything else keys on the composition
            // alone (see `system_result_parts`).
            let description = match system_result_parts(description) {
                Some((_, SYSTEM_SKIPPED_DETAIL)) => continue,
                Some((key, _)) => key,
                None => description,
            };
            let (rtype, rid) = parse_resource_from_description(description);
            // A module skipped whole was never probed, so the run manages
            // nothing under it and heals nothing: the skip is information about
            // this host (a platform gate, an encryption incompatibility), not a
            // resource cfgd put on the machine. The sibling of the
            // `SYSTEM_SKIPPED_DETAIL` guard above.
            if rtype == "module" && super::module_row_facet(&rid) == Some("skip") {
                continue;
            }
            // The rows this action stands for, from the ONE producer the daemon
            // tick records through — never re-derived from `description`, which
            // is the `managed_resources` tracking id and spells a module's
            // deployment as a unit (`module:<name>:files:<n>`) no per-file check
            // can match. Reached only past the guards above: a configurator that
            // applied nothing converged nothing.
            self.state
                .resolve_drift_keys(apply_id, &result.drift_rows)?;
            // A manager node is cfgd's own scaffolding, never a resource the
            // user declared: a refreshed index, a provisioned manager and a
            // tool a cascade shelled out to are none of them things cfgd
            // prunes, restores or reports under `cfgd status`. The journal
            // still records the work; `managed_resources` does not. A landed
            // provision still settles its drift rows first — both producers
            // record the missing-tooling finding under
            // ("package", "provision:<mgr>") / ("package", "refuse:<mgr>"),
            // and this apply is the event that heals them. The description
            // names only the batch's leader, so the members ride in on
            // `result.versions`; a successful Refuse resolves nothing — the
            // manager it names is exactly as missing as before.
            if rtype == MANAGER_RESOURCE_TYPE {
                if let Some(leader) = rid.strip_prefix("provision:") {
                    let mut healed: Vec<(String, String)> = Vec::new();
                    for manager in
                        std::iter::once(leader).chain(result.versions.keys().map(String::as_str))
                    {
                        healed.push((
                            "package".to_string(),
                            ManagerAction::provision_resource_id(manager),
                        ));
                        healed.push((
                            "package".to_string(),
                            ManagerAction::refuse_resource_id(manager),
                        ));
                    }
                    self.state.resolve_drift_keys(apply_id, &healed)?;
                }
                continue;
            }
            self.state
                .upsert_managed_resource(&rtype, &rid, LOCAL_LAYER, None, Some(apply_id))?;
            if rtype == ENV_RESOURCE_TYPE {
                // An `env:inject:<rc>` action's subject is the shell rc file,
                // but the check that reads it records the source line under
                // `env-rc`, not `env` — so the injected line's row stood open
                // through the apply that wrote it. The verb is reconstructed
                // from the target, the one reading of that split — an ask the
                // live-session id is not a target for, so it is held back
                // before the question is put, as its sibling below holds it.
                if rid != crate::state::ENV_SESSION_RESOURCE_ID
                    && super::recorded_env_method(&rid) == super::ENV_VERB_INJECT
                {
                    self.state.resolve_drift(apply_id, "env-rc", &rid)?;
                }
                self.resolve_env_item_drift(apply_id, &rid, resolved, modules)?;
            }
            if let Some(module) =
                super::format::module_files_description_module(&result.description)
            {
                self.resolve_module_file_drift(apply_id, module, modules)?;
            }
        }
        Ok(())
    }

    /// Resolve the per-file `module` drift rows a successful file deployment
    /// converged.
    ///
    /// The deployment records ONE aggregate row per module
    /// (`module:<name>:files:<n>`, keyed on the declared count so a partial
    /// deploy lands where a full one does), while every live check records one
    /// row per file (`<module>/<target>`). Nothing matched the two, so healing
    /// a drifted file with `cfgd apply` left its row open until the next scan —
    /// and `cfgd status` went on advising the very command that had just run.
    ///
    /// The ids come from
    /// [`module_file_spec_resource_id`](super::module_file_spec_resource_id),
    /// which owns the `Patch`/unexpanded-target split the finding's own id was
    /// minted through; a hand-built one would spell a patched file two ways.
    ///
    /// Every DECLARED file of the module is resolved, not only the subset the
    /// action wrote. An entry elided as already-converged is one the machine
    /// matched before the run — the same claim a write makes, reached without
    /// writing — so its row is as stale as the written file's. A FAILED action
    /// resolves nothing: `record_managed_resources` skips it before reaching
    /// here, and the machine is exactly as drifted as the check found it.
    ///
    /// A file whose row `withholds_recorded_row` protects is left open. The
    /// prune edits a `DeployFiles` action's file list per target, so an action
    /// can succeed while one of its declared files was deliberately withheld
    /// from the run; resolving that row would heal a claim nothing checked.
    fn resolve_module_file_drift(
        &self,
        apply_id: i64,
        module: &str,
        modules: &[ResolvedModule],
    ) -> Result<()> {
        let Some(resolved_module) = modules.iter().find(|m| m.name == module) else {
            return Ok(());
        };
        let keys: Vec<(String, String)> = resolved_module
            .files
            .iter()
            .map(|file| super::module_file_spec_resource_id(module, file))
            .filter(|id| {
                !self
                    .withheld_rows
                    .is_some_and(|x| x.withholds_recorded_row("module", id))
            })
            .map(|id| ("module".to_string(), id))
            .collect();
        self.state.resolve_drift_keys(apply_id, &keys)
    }

    /// Resolve the per-item `env-var`/`alias` drift rows a successful write of
    /// the PRIMARY managed env file converged.
    ///
    /// `verify_env_items` records one row per declared entry, keyed by the
    /// entry's own name, but the action that heals every one of them is a
    /// single `env:write:<path>` whose description parses to
    /// `("env", <path>)` — so the file's own row resolved and the item rows
    /// beneath it stayed open forever, and a converged machine kept reporting
    /// drift about entries the file already holds. Nothing here rewrites an
    /// operand: the rows are resolved exactly as the file's own row is, so the
    /// stored `current` / `missing or changed` markers stay byte-exact.
    ///
    /// Gated on the PRIMARY file because that is the only one the per-item
    /// checks read; a write of `environment.d` or the launchd plist says
    /// nothing about whether the entry landed in the file that was verified.
    fn resolve_env_item_drift(
        &self,
        apply_id: i64,
        written: &str,
        resolved: &ResolvedProfile,
        modules: &[ResolvedModule],
    ) -> Result<()> {
        if written != to_posix_string(super::primary_env_file(&self.home)) {
            return Ok(());
        }
        let (env, aliases, _) = super::verify::merge_module_env_aliases(
            &resolved.merged.env,
            &resolved.merged.aliases,
            &resolved.merged.entry_owners,
            modules,
        );
        // One statement for the whole merged set: a per-entry resolve is its own
        // index seek and its own statement per declared env var and alias,
        // inside the apply transaction, where the set-based form seeks once.
        let keys: Vec<(String, String)> = env
            .iter()
            .map(|ev| ("env-var".to_string(), ev.name.clone()))
            .chain(
                aliases
                    .iter()
                    .map(|alias| ("alias".to_string(), alias.name.clone())),
            )
            .collect();
        self.state.resolve_drift_keys(apply_id, &keys)
    }

    /// Turn one finished action into its journal completion, its result row and
    /// the line the phase's tree will render.
    ///
    /// The ONE settle, shared by the sequential walk and the concurrent
    /// dispatcher, because the two orders differ and the LINE must not: a
    /// deferred tree written from a second derivation would disagree with a
    /// streaming one about role, detail or duration on the same action.
    fn settle_action(&self, input: SettleInput<'_, '_>) -> Settled {
        let SettleInput {
            action,
            journal_id,
            result,
            sidecars,
            elapsed,
            notes,
            body,
            finished,
            ledger,
            results,
        } = input;
        // Set by the `Err` arm below so the tree renders the failure on
        // the action's own line instead of a bespoke one above it.
        let mut failure_detail: Option<FailureDisplay> = None;

        // The copies an adopting action took, rendered as that action's own
        // detail rather than as a status line above it — on the failure row as
        // much as the success one, since the copy was taken either way.
        let sidecar_detail = sidecar_detail(&sidecars);
        let (desc, success, action_changed, installed, delivered, versions, error, should_abort) =
            match result {
                Ok(run) => {
                    if let Some(jid) = journal_id
                        && let Err(e) = self.state.journal_complete(
                            jid,
                            finished,
                            None,
                            run.script_output.as_deref(),
                        )
                    {
                        // tracing-ok: the journal row could not be closed; the action's own line is settled either way
                        tracing::warn!("failed to record journal completion: {e}");
                    }
                    // The mirror of the failure arm's record below: a manager this
                    // run PUT on the machine, so a later phase can tell a tool it
                    // delivered from one that was already here. See
                    // `Reconciler::provisioned`.
                    if let Action::Manager(node @ ManagerAction::Provision { .. }) = action {
                        let mut landed = self.provisioned.borrow_mut();
                        for manager in node.provisioned_managers() {
                            if !landed.iter().any(|m| m == manager) {
                                landed.push(manager.to_string());
                            }
                        }
                        self.provisioned_packages.borrow_mut().extend(
                            super::managers::provision_delivered_packages(self.registry, node),
                        );
                    }
                    (
                        run.description,
                        true,
                        run.changed,
                        run.installed,
                        run.delivered,
                        run.versions,
                        None,
                        false,
                    )
                }
                Err(e) => {
                    let desc = format_action_description(action);

                    // Check if this is a script action with continueOnError
                    let continue_on_err = if let Action::Script(ScriptAction::Run {
                        entry,
                        phase: script_phase,
                        ..
                    }) = action
                    {
                        effective_continue_on_error(entry, script_phase)
                    } else {
                        false
                    };

                    failure_detail = Some(FailureDisplay {
                        // The DETAIL fold, not the subject one: a failed command's
                        // message carries the child's own lines, and the renderer
                        // already lays them out as indented continuations. Flattening
                        // them here spent the row's ` — ` separator once per line.
                        detail: crate::output::captured_output_detail(&e),
                        continue_on_err,
                        ran: failed_action_ran(action, &e),
                    });
                    if let Some(jid) = journal_id
                        && let Err(je) = self.state.journal_fail(jid, finished, &e.to_string())
                    {
                        // tracing-ok: same, for the failure half
                        tracing::warn!("failed to record journal failure: {je}");
                    }
                    // The run's own verdict about what is on the machine, recorded
                    // where every finish lands so a later phase answers from it
                    // instead of re-probing: see `Reconciler::unprovisioned`.
                    if let Action::Manager(node) = action {
                        let mut withheld = self.unprovisioned.borrow_mut();
                        for manager in node.managers_left_unavailable() {
                            if !withheld.iter().any(|m| m == manager) {
                                withheld.push(manager.to_string());
                            }
                        }
                    }
                    (
                        desc,
                        false,
                        false,
                        None,
                        0,
                        Vec::new(),
                        Some(e.to_string()),
                        !continue_on_err,
                    )
                }
            };

        let changed = success && action_changed;
        // The ONE predicate the header priced the plan with decides the tally
        // too: an action the plan already withheld is recorded as not attempted,
        // never as a skip that ran, or the apply reports one outcome more than
        // the plan promised.
        let not_attempted = action
            .pre_skip_reason()
            .filter(|_| success)
            .map(str::to_string);
        results.push(ActionResult {
            phase: ledger.phase_name.as_str().to_string(),
            description: desc.clone(),
            success,
            error,
            changed,
            // An action that reports its own status line settles its own
            // outcome, so the tree never decides one for it and this record
            // must not invent one either.
            skipped: success
                && not_attempted.is_none()
                && !action_reports_its_own_status(action)
                && settled_success_role(action, action_changed) == Role::Skipped,
            not_attempted,
            // Only when the run landed FEWER than it named: the description
            // beside it is the planned set, so an equal count restates it.
            // Judged by the same producers the row's detail is worded from, so
            // a count `-o json` carries is one the report also states.
            installed: installed.filter(|_| {
                installed_packages_summary(action, installed, delivered).is_some()
                    || provisioned_managers_summary(action, installed, &[]).is_some()
            }),
            versions: versions.iter().cloned().collect(),
            // Off the ONE producer, so the rows a daemon tick recorded for this
            // action and the rows this settle heals are one set. Empty for a
            // Skip, whose row records that cfgd could not act at all.
            drift_rows: if super::apply_heals_action_rows(action) {
                super::action_drift_rows(action, self.registry)
                    .iter()
                    .map(super::DriftRow::key)
                    .collect()
            } else {
                Vec::new()
            },
        });

        if action_reports_its_own_status(action) {
            return Settled {
                desc,
                should_abort,
                outcome: None,
                notes,
            };
        }

        let subject = ledger
            .subjects
            .get(&action_key(action))
            .cloned()
            .unwrap_or_else(|| condense_action_desc_for_display(action, &desc));
        let outcome = match &failure_detail {
            Some(FailureDisplay {
                detail: message,
                continue_on_err,
                ran,
            }) => ActionOutcome {
                subject,
                role: if *continue_on_err {
                    Role::Warn
                } else {
                    Role::Fail
                },
                // A refusal's subject IS its reason by construction
                // (`cannot provision <m> — <reason>`), and the error it
                // settles through restates that reason for the journal. On
                // the line the two are one sentence printed twice. A sidecar
                // still rides along: the copy was taken before the write that
                // failed, and it is on disk whatever the write did.
                detail: join_detail(
                    (!matches!(action, Action::Manager(ManagerAction::Refuse { .. })))
                        .then(|| message.clone()),
                    sidecar_detail,
                ),
                detail_muted: false,
                duration: ran.then_some(elapsed),
                notes,
                body,
                // A failed deploy still lists what it was ASKED to write, not
                // what it actually wrote — the plan promised this exact list,
                // and a reader diagnosing the failure needs to see which
                // targets and methods were in flight, not a blank row.
                children: deploy_file_children(action).unwrap_or_default(),
            },
            None => {
                let noop = declared_noop_role(action);
                let role = settled_success_role(action, action_changed);
                let detail = if noop.is_none() && !action_changed {
                    Some(if desc.ends_with(ENV_NO_SESSION_MANAGER_SUFFIX) {
                        crate::NO_SESSION_MANAGER.to_string()
                    } else {
                        "unchanged".to_string()
                    })
                } else {
                    action_produced_detail(action, installed, delivered, &versions)
                };
                // Every action that DID something is timed, however briefly: a
                // threshold makes the suffix's absence ambiguous between "fast"
                // and "not measured", and a reader comparing two runs cannot
                // tell which. `Role::Ok` is exactly "not a declared noop, and it
                // changed something" — everything else did no work to time.
                let duration = (role == Role::Ok).then_some(elapsed);
                // A displaced original is a fact about the user's own file, not
                // a note about the write, so it is not muted the way an
                // `unchanged` aside is. It does not REPLACE what the row
                // already had to say either: a module `DeployFiles` that
                // adopted a target and then wrote nothing new is both
                // `unchanged` AND holding a copy, and dropping either half
                // describes a different run.
                let adopted = sidecar_detail.is_some();
                ActionOutcome {
                    subject,
                    role,
                    detail: join_detail(detail, sidecar_detail),
                    detail_muted: !adopted,
                    duration,
                    notes,
                    body,
                    children: deploy_file_children(action).unwrap_or_default(),
                }
            }
        };
        Settled {
            desc,
            should_abort,
            outcome: Some(outcome),
            notes: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_action(
        &self,
        action: &Action,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        apply_id: i64,
        context: ReconcileContext,
        module_actions: &[ResolvedModule],
        secret_env_collector: &mut Vec<(String, String)>,
        shell_override: Option<ScriptShell>,
        abort: &AbortFlag,
        notes: &NoteSink,
        sidecars: &mut Vec<SidecarOutcome>,
    ) -> Result<ActionRun> {
        match action {
            Action::System(sys) => self
                .apply_system_action(sys, &resolved.merged, module_actions, printer, notes)
                .map(|d| ActionRun::new(d, true)),
            Action::Package(pkg) => self.apply_package_action(pkg, printer, notes),
            Action::File(file) => self
                .apply_file_action(file, resolved.profile_name(), config_dir, printer, sidecars)
                .map(|d| ActionRun::new(d, true)),
            Action::Secret(secret) => self
                .apply_secret_action(secret, config_dir, secret_env_collector)
                .map(|d| ActionRun::new(d, true)),
            Action::Script(script) => self
                .apply_script_action(
                    script,
                    resolved,
                    config_dir,
                    printer,
                    context,
                    shell_override,
                    abort,
                )
                .map(|(d, c, output)| ActionRun {
                    script_output: output,
                    ..ActionRun::new(d, c)
                }),
            Action::Module(module) => self.apply_module_action(
                module,
                config_dir,
                printer,
                apply_id,
                context,
                resolved,
                module_actions,
                shell_override,
                abort,
                notes,
                sidecars,
            ),
            Action::Manager(manager) => self.apply_manager_action(manager, printer, notes),
            Action::Env(env) => Self::apply_env_action(env, printer, notes).map(|d| {
                let changed = !env_result_unchanged(&d);
                ActionRun::new(d, changed)
            }),
        }
    }
}

#[cfg(test)]
mod detail_tests {
    /// A row that both adopted a target and had something else to say carries
    /// BOTH, in the order they happened. `or`-ing them drops one: a module
    /// `DeployFiles` that adopted a target and then wrote nothing new is
    /// `unchanged` AND holding a copy, and a failed write is an error AND
    /// holding a copy — reporting either half alone describes a different run.
    #[test]
    fn a_row_with_two_things_to_say_says_both() {
        let j = super::join_detail;
        assert_eq!(
            j(
                Some("unchanged".into()),
                Some("backed up to ~/x.cfgd-backup".into())
            ),
            Some("unchanged, backed up to ~/x.cfgd-backup".to_string())
        );
        assert_eq!(
            j(Some("unchanged".into()), None),
            Some("unchanged".to_string())
        );
        assert_eq!(
            j(None, Some("backed up to ~/x.cfgd-backup".into())),
            Some("backed up to ~/x.cfgd-backup".to_string())
        );
        assert_eq!(j(None, None), None);
    }
}
