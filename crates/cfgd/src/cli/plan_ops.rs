use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::{FileStrategy, LOCAL_LAYER};
use cfgd_core::manager_family;
use cfgd_core::output::{Doc, Printer, Role};

// --- Plan output rendering ---

/// Basename of the bash/zsh managed env file the reconciler writes.
const UNIX_ENV_FILE: &str = ".cfgd.env";
/// Basename of the PowerShell managed env file the reconciler writes.
const PS_ENV_FILE: &str = ".cfgd-env.ps1";

/// Tell the user their already-running shell predates the env file this apply
/// wrote, so the freshly bootstrapped PATH entries are one command away instead
/// of requiring a re-login nobody thinks to try.
///
/// Gated purely on the descriptions `apply_env_action` returns — it suffixes
/// `:skipped` when the on-disk bytes already matched — so nothing here re-stats
/// the filesystem and races whatever ran after the Env phase.
pub(in crate::cli) fn print_shell_env_reminder(
    result: &cfgd_core::reconciler::ApplyResult,
    printer: &Printer,
) {
    let mut wrote_env = false;
    let mut candidates: Vec<&str> = Vec::new();
    for action in &result.action_results {
        if !action.success || action.description.ends_with(":skipped") {
            continue;
        }
        let desc = action.description.as_str();
        let Some(path) = desc
            .strip_prefix("env:write:")
            .or_else(|| desc.strip_prefix("env:inject:"))
        else {
            continue;
        };
        wrote_env = true;
        // The env phase writes several managed files (fish conf.d, systemd
        // environment.d, a LaunchAgent); only the shell env file is something a
        // running shell can usefully source, so the rest never name the command.
        if path.ends_with(UNIX_ENV_FILE) || path.ends_with(PS_ENV_FILE) {
            candidates.push(path);
        }
    }
    if !wrote_env {
        return;
    }

    // Windows can produce BOTH files in one apply, so the command is chosen by
    // the shell the user is standing in, never by which target was emitted
    // first. A run whose only env change was a source-line injection (or the
    // secret-env regeneration, whose id carries no path) still needs a command
    // to name, so the same choice supplies the fallback location.
    let preferred = current_shell_env_file();
    let shown = match candidates
        .iter()
        .find(|p| p.ends_with(preferred))
        .or_else(|| candidates.first())
    {
        Some(path) => fold_home_to_tilde(path),
        None => format!("~/{preferred}"),
    };
    let command = if shown.ends_with(PS_ENV_FILE) {
        format!(". {shown}")
    } else {
        format!("source {shown}")
    };

    let section = printer.section("Shell environment changed");
    section.bullet(format!("run: {command}"));
    section.bullet("or open a new shell");
}

/// The env file the shell the user is *standing in* can actually source.
///
/// On Windows both files can exist after a single apply: the env engine always
/// writes `.cfgd-env.ps1`, and additionally writes `.cfgd.env` when Git Bash is
/// installed. Naming whichever one was emitted first tells a Git Bash user to
/// run `. ~/.cfgd-env.ps1`, which their shell cannot read. `MSYSTEM` is exported
/// by every MSYS2 / Git Bash shell (`MINGW64`, `MINGW32`, `MSYS`, `CLANG64`, …)
/// and is the marker for that environment; `SHELL` is the secondary signal for
/// a POSIX shell launched some other way.
fn preferred_env_file(windows: bool, msystem: Option<&str>, shell: Option<&str>) -> &'static str {
    if !windows {
        return UNIX_ENV_FILE;
    }
    let in_msys = msystem.is_some_and(|v| !v.trim().is_empty());
    let posix_shell = shell.is_some_and(|s| {
        let normalized = cfgd_core::posixify_text(s);
        let file = normalized
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let stem = file.strip_suffix(".exe").unwrap_or(file.as_str());
        matches!(stem, "bash" | "sh" | "zsh" | "dash" | "fish" | "ksh")
    });
    if in_msys || posix_shell {
        UNIX_ENV_FILE
    } else {
        PS_ENV_FILE
    }
}

fn current_shell_env_file() -> &'static str {
    preferred_env_file(
        cfg!(windows),
        std::env::var("MSYSTEM").ok().as_deref(),
        std::env::var("SHELL").ok().as_deref(),
    )
}

/// Render an absolute env-file path as `~/…` when it sits under the current
/// home, so the reminder shows a command the user can retype verbatim.
fn fold_home_to_tilde(path: &str) -> String {
    let home = cfgd_core::to_posix_string(cfgd_core::expand_tilde(std::path::Path::new("~")));
    match path.strip_prefix(&format!("{}/", home.trim_end_matches('/'))) {
        Some(rest) if !home.is_empty() => format!("~/{rest}"),
        _ => path.to_string(),
    }
}

/// Derive a short action type string from a reconciler Action.
pub(in crate::cli) fn action_type_str(action: &reconciler::Action) -> &'static str {
    match action {
        reconciler::Action::File(fa) => match fa {
            FileAction::Create { .. } => "create",
            FileAction::Update { .. } => "update",
            FileAction::Delete { .. } => "delete",
            FileAction::SetPermissions { .. } => "chmod",
            FileAction::Skip { .. } => "skip",
        },
        reconciler::Action::Package(pa) => match pa {
            PackageAction::Install { .. } => "install",
            PackageAction::Uninstall { .. } => "uninstall",
            PackageAction::Skip { .. } => "skip",
        },
        reconciler::Action::Secret(sa) => match sa {
            SecretAction::Decrypt { .. } => "decrypt",
            SecretAction::Resolve { .. } => "resolve",
            SecretAction::ResolveEnv { .. } => "resolve-env",
            SecretAction::Skip { .. } => "skip",
        },
        reconciler::Action::System(sa) => match sa {
            reconciler::SystemAction::SetValue { .. } => "set",
            reconciler::SystemAction::Skip { .. } => "skip",
        },
        reconciler::Action::Script(_) => "run",
        reconciler::Action::Module(ma) => match &ma.kind {
            reconciler::ModuleActionKind::InstallPackages { .. } => "install",
            reconciler::ModuleActionKind::DeployFiles { .. } => "deploy",
            reconciler::ModuleActionKind::RunScript { .. } => "run",
            reconciler::ModuleActionKind::Skip { .. } => "skip",
        },
        reconciler::Action::Env(ea) => match ea {
            reconciler::EnvAction::WriteEnvFile { .. } => "write",
            reconciler::EnvAction::InjectSourceLine { .. } => "inject",
            reconciler::EnvAction::RefreshLiveSession { .. } => "refresh",
        },
        reconciler::Action::Manager(ma) => match ma {
            reconciler::ManagerAction::RefreshIndex { .. } => "refresh",
            reconciler::ManagerAction::Provision { .. } => "provision",
            reconciler::ManagerAction::Prerequisite { .. } => "prerequisite",
            reconciler::ManagerAction::Refuse { .. } => "refuse",
        },
    }
}

/// The `cfgd:managers` group's structured detail for a plan action —
/// `Some` only for `Action::Manager`, `None` (omitted from the wire) for every
/// other action kind.
///
/// `requires` is [`reconciler::ManagerAction::depends_on`] verbatim: full
/// `manager:...` node ids, identical in shape to a sibling row's
/// `description` — a consumer resolves an edge against another action in the
/// same phase without a second id scheme (see `ManagerActionOutput`'s doc).
pub(in crate::cli) fn manager_action_output(
    action: &reconciler::Action,
) -> Option<ManagerActionOutput> {
    let reconciler::Action::Manager(ma) = action else {
        return None;
    };
    let requires = ma.depends_on().to_vec();
    Some(match ma {
        reconciler::ManagerAction::RefreshIndex { manager } => ManagerActionOutput {
            manager: manager.clone(),
            state: "present".to_string(),
            via: None,
            requires,
            reason: None,
        },
        reconciler::ManagerAction::Provision { manager, via, .. } => ManagerActionOutput {
            manager: manager.clone(),
            state: "provisioned".to_string(),
            via: Some(via.clone()),
            requires,
            reason: None,
        },
        reconciler::ManagerAction::Prerequisite {
            tool, installer, ..
        } => ManagerActionOutput {
            manager: tool.clone(),
            state: "prerequisite".to_string(),
            via: Some(installer.clone()),
            requires,
            reason: None,
        },
        reconciler::ManagerAction::Refuse { manager, reason } => ManagerActionOutput {
            manager: manager.clone(),
            state: "refused".to_string(),
            via: None,
            requires,
            reason: Some(reason.clone()),
        },
    })
}

/// Absolute filesystem target path(s) a plan action writes, for structured
/// (`-o json`) consumers and blast-radius tooling. Empty for actions with no
/// direct filesystem target (package installs, system-configurator writes,
/// live-session refresh, secret-provider resolution into the env file).
pub(in crate::cli) fn action_targets(action: &reconciler::Action) -> Vec<String> {
    fn show(path: &std::path::Path) -> String {
        path.display().to_string()
    }
    match action {
        reconciler::Action::File(fa) => match fa {
            FileAction::Create { target, .. }
            | FileAction::Update { target, .. }
            | FileAction::Delete { target, .. }
            | FileAction::SetPermissions { target, .. }
            | FileAction::Skip { target, .. } => vec![show(target)],
        },
        reconciler::Action::Env(ea) => match ea {
            reconciler::EnvAction::WriteEnvFile { path, .. } => vec![show(path)],
            reconciler::EnvAction::InjectSourceLine { rc_path, .. } => vec![show(rc_path)],
            reconciler::EnvAction::RefreshLiveSession { .. } => vec![],
        },
        reconciler::Action::Secret(sa) => match sa {
            SecretAction::Decrypt { target, .. } | SecretAction::Resolve { target, .. } => {
                vec![show(target)]
            }
            SecretAction::ResolveEnv { .. } | SecretAction::Skip { .. } => vec![],
        },
        reconciler::Action::Module(ma) => match &ma.kind {
            reconciler::ModuleActionKind::DeployFiles { files } => {
                files.iter().map(|f| show(&f.target)).collect()
            }
            _ => vec![],
        },
        reconciler::Action::Package(_)
        | reconciler::Action::System(_)
        | reconciler::Action::Manager(_)
        | reconciler::Action::Script(_) => vec![],
    }
}

/// Source provenance of a plan action for structured (`-o json`) consumers:
/// `Some(source_name)` when a ConfigSource delivered the resource body, `None`
/// for consumer-local resources (and for action kinds with no provenance, e.g.
/// system writes / env / locally-authored scripts). Files/packages/secrets/
/// scripts carry origin as the sentinel `String` [`LOCAL_LAYER`]/`""`; modules carry
/// it as `Option<String>`. Both normalize to `None` for local here so the wire
/// field is omitted exactly when there is no remote provenance to report.
pub(in crate::cli) fn action_origin(action: &reconciler::Action) -> Option<String> {
    fn norm(origin: &str) -> Option<String> {
        if origin.is_empty() || origin == LOCAL_LAYER {
            None
        } else {
            Some(origin.to_string())
        }
    }
    match action {
        reconciler::Action::Module(ma) => ma.origin.clone(),
        reconciler::Action::File(fa) => match fa {
            FileAction::Create { origin, .. }
            | FileAction::Update { origin, .. }
            | FileAction::Delete { origin, .. }
            | FileAction::SetPermissions { origin, .. }
            | FileAction::Skip { origin, .. } => norm(origin),
        },
        reconciler::Action::Package(pa) => match pa {
            PackageAction::Install { origin, .. }
            | PackageAction::Uninstall { origin, .. }
            | PackageAction::Skip { origin, .. } => norm(origin),
        },
        reconciler::Action::Secret(sa) => match sa {
            SecretAction::Decrypt { origin, .. }
            | SecretAction::Resolve { origin, .. }
            | SecretAction::ResolveEnv { origin, .. }
            | SecretAction::Skip { origin, .. } => norm(origin),
        },
        reconciler::Action::System(sa) => match sa {
            reconciler::SystemAction::SetValue { origin, .. } => norm(origin),
            reconciler::SystemAction::Skip { origin, .. } => norm(origin),
        },
        reconciler::Action::Script(sa) => match sa {
            reconciler::ScriptAction::Run { origin, .. } => norm(origin),
        },
        // cfgd needed the manager; no source delivered it.
        reconciler::Action::Env(_) | reconciler::Action::Manager(_) => None,
    }
}

/// Everything a run must withhold because of a source decision.
///
/// The ONE decision gate `cfgd plan` and `cfgd apply` share, so a preview and
/// the apply it precedes withhold the same set — and so a manual apply cannot
/// install the item the daemon's policy declines. Three inputs decide it:
///
/// - **The subscribed sources**, which say whose rows still mean anything. A
///   run whose config did not parse has no authoritative list, so it withholds
///   everything rather than treating a fabricated empty list as "no source
///   objects to anything".
/// - **The operator's own declarations**, which a source's decision may never
///   settle. Manifest files are resolved into the local view first: a
///   `brew.file: Brewfile` is a declaration like any other, and a guard reading
///   only the layers would leave that whole declaration style unprotected.
/// - **The auto-apply policy**, whose `Reject` / `Ignore` tiers decline an item
///   outright (silently, per `docs/sources.md` — those paths prune the plan but
///   render nothing, because the instruction is already in the config) and
///   whose `Notify` tier withholds the item until the operator answers for it.
///   A `Notify` item is withheld from the FIRST run that classifies it, before
///   any row exists: the contract is ask-before-install on every path, so the
///   window between a source delivering an item and the daemon's next tick
///   cannot be the window a manual apply installs it in.
///
/// [`DecisionWrites`] says whether this run may record what it classified.
/// The [`reconciler::SourcePolicyReview`] rides back with the withheld set so
/// a caller whose writes are DEFERRED — `cfgd apply` mints only after the
/// operator confirms the run — can hand the same classification to
/// [`reconciler::mint_decisions`] later instead of classifying twice.
pub(in crate::cli) fn withheld_for_run(
    state: &cfgd_core::state::StateStore,
    cfg: &cfgd_core::config::CfgdConfig,
    resolved: &cfgd_core::config::ResolvedProfile,
    config_dir: &Path,
    config_parsed: bool,
    writes: DecisionWrites<'_>,
    actual: &reconciler::ActualPackages,
) -> anyhow::Result<(
    reconciler::WithheldDecisions,
    reconciler::SourcePolicyReview,
)> {
    let mut local = reconciler::local_profile(resolved);
    packages::resolve_manifest_packages(&mut local.packages, config_dir)?;

    let scope = if config_parsed {
        reconciler::DecisionScope::new(cfg.spec.sources.iter().map(|s| s.name.as_str()), &local)
    } else {
        reconciler::DecisionScope::unverified(&local)
    };

    // Fail-CLOSED on both reads: a run that cannot tell a decided resource from
    // an undecided one must not guess, and the half it would guess wrong is the
    // half that installs.
    if !config_parsed {
        return Ok((
            reconciler::WithheldDecisions::read(state, &scope)?,
            reconciler::SourcePolicyReview::default(),
        ));
    }
    let review = reconciler::review_source_policies(
        state,
        cfg,
        resolved,
        reconciler::configured_auto_apply(cfg),
        actual,
    )?;
    // Minting first is what makes the rows readable below, so a minting run
    // names the same rows `cfgd status` and `cfgd decide` will. It is also why
    // the read comes after — `with_unrecorded` then has nothing left to add
    // for the items this run recorded. The mint is NARROWED to the items the
    // answering run actually names: recording the rest of the classification
    // (or any source hash) would consume the daemon's one notification for
    // items the operator never touched.
    if let DecisionWrites::Mint(targets) = writes {
        reconciler::mint_decisions(state, &review.narrowed_to(&targets));
    }
    let withheld = reconciler::WithheldDecisions::read(state, &scope)?
        .with_policy_declined(review.declined.clone())
        .with_unrecorded(&review.to_mint, &scope)
        .with_undecidable(review.undecidable.clone())
        .with_auto_accepted(&review.auto_accepted);
    Ok((withheld, review))
}

/// Whether a run may record the decisions its policy review classified NOW.
///
/// `cfgd plan` is a preview and writes nothing, so it withholds a newly
/// classified item without minting a row for it. `cfgd decide` is the
/// answering surface and mints immediately — narrowed to the item(s) it is
/// answering, and stamping no source hashes — so an unrecorded-but-classified
/// item is answerable in the same invocation that named it while everything
/// it did not name stays unrecorded and still owed its notification. `cfgd
/// apply` passes `ReadOnly` here and mints AFTER the operator confirms the
/// run, through the review this function returns — declining the prompt must
/// leave the store untouched. All withhold identically — only the record
/// differs.
#[derive(Debug, Clone, Copy)]
pub(in crate::cli) enum DecisionWrites<'a> {
    Mint(reconciler::DecisionTargets<'a>),
    ReadOnly,
}

/// Build a PlanOutput from a reconciler Plan, applying an optional phase filter.
///
/// The phase/owner/action walk is the reconciler's own
/// [`reconciler::in_scope_tree`] — the same one the human tree renders — so
/// membership and group order in the payload cannot drift from what the CLI
/// draws. It is taken at [`reconciler::PhaseCoverage::Complete`]: a structured
/// consumer diffing plans across hosts is exactly who needs to see the
/// `Modules` phase of platform-gated skips that the tree folds into its header.
pub(in crate::cli) fn build_plan_output(
    plan: &reconciler::Plan,
    context_name: &str,
    phase_filter: Option<&PhaseFilter>,
    pending_backups: &[String],
    withheld: &reconciler::WithheldDecisions,
) -> PlanOutput {
    let phases: Vec<PlanPhaseOutput> =
        reconciler::in_scope_tree(plan, phase_filter, reconciler::PhaseCoverage::Complete)
            .into_iter()
            .map(|(phase_item, groups)| PlanPhaseOutput {
                phase: phase_item.name.display_name().to_string(),
                groups: groups
                    .into_iter()
                    .map(|(group, actions)| {
                        PlanGroupOutput::new(
                            group.owner.clone(),
                            actions
                                .into_iter()
                                .map(|action| PlanActionOutput {
                                    description: reconciler::format_plan_item(action),
                                    action_type: action_type_str(action).to_string(),
                                    targets: action_targets(action),
                                    origin: action_origin(action),
                                    manager: manager_action_output(action),
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            })
            .collect();
    let total_actions = phases
        .iter()
        .flat_map(|p| p.groups.iter())
        .map(|g| g.actions().len())
        .sum();
    PlanOutput {
        context: context_name.to_string(),
        phases,
        total_actions,
        warnings: plan.warnings.clone(),
        pending_backups: pending_backups.to_vec(),
        pending_decisions: withheld.pending.clone(),
        rejected_decisions: withheld.rejected.clone(),
    }
}

/// The manager every `PackageAction` names.
///
/// One or-pattern over all three variants rather than two arms and a
/// fallthrough, so a fourth variant fails to compile here instead of silently
/// escaping `--skip-scripts`.
fn package_manager_name(a: &PackageAction) -> &str {
    match a {
        PackageAction::Install { manager, .. }
        | PackageAction::Uninstall { manager, .. }
        | PackageAction::Skip { manager, .. } => manager,
    }
}

/// Anything `--skip-scripts` must remove, whichever phase it landed in.
///
/// Classifying by action shape rather than by phase is what keeps the gate
/// correct: a module's `manager: script` package plans as package work and
/// applies in the `Packages` phase, so a phase test would run a script the user
/// asked to skip.
fn is_script_work(a: &reconciler::Action) -> bool {
    match a {
        reconciler::Action::Module(reconciler::ModuleAction {
            kind: reconciler::ModuleActionKind::RunScript { .. },
            ..
        }) => true,
        // `any`, not `first`: one manager per action is a `plan_modules`
        // invariant rather than a property of this type, and passing over a
        // script entry because it is not first would run a script
        // `--skip-scripts` excluded.
        reconciler::Action::Module(reconciler::ModuleAction {
            kind: reconciler::ModuleActionKind::InstallPackages { resolved },
            ..
        }) => resolved.iter().any(|p| p.manager == "script"),
        reconciler::Action::Package(pa) => package_manager_name(pa) == "script",
        reconciler::Action::Script(_) => true,
        _ => false,
    }
}

/// Strip all script-related actions from a plan.
/// Removes PreScripts/PostScripts phases, module-level RunScript actions,
/// and script-based package installs (manager: "script").
pub(in crate::cli) fn strip_scripts_from_plan(plan: &mut reconciler::Plan) {
    plan.phases
        .retain(|p| !matches!(p.name, PhaseName::PreScripts | PhaseName::PostScripts));
    for phase in &mut plan.phases {
        phase.retain_actions(|a| !is_script_work(a));
    }
    // A group made entirely of the filtered-out kind survives the retain above
    // with zero actions, and a phase can lose every group. Drop both here, the
    // same way `Reconciler::plan` drops an action-less phase at construction, so
    // neither reaches display or `-o json`.
    plan.phases.retain(|p| !p.is_empty());
}

/// Pre-filter snapshot of a plan's scope, captured *before* `--skip`/`--only`
/// destructively prune it, so a later zero-action outcome can be reported
/// honestly. Without this, `apply`/`plan` claim "everything is up to date" even
/// when a scoping flag (`--phase`/`--only`/`--skip`/`--skip-scripts`/`--module`)
/// excluded real, pending work — telling the user the system is in sync when it
/// is not.
pub(in crate::cli) struct ScopeReport {
    /// Any scoping flag that can narrow the plan to a subset was set.
    pub filter_active: bool,
    /// Total actions the plan held before `--skip`/`--only` pruning.
    pub unfiltered_total: usize,
    /// Display names of the phases that held actions before pruning.
    pub phases_with_work: Vec<String>,
    /// Set to the requested module name when `--module <name>` resolved to
    /// nothing (typo / not found / unreadable) rather than to real actions.
    pub module_miss: Option<String>,
}

impl ScopeReport {
    pub(in crate::cli) fn capture(
        plan: &reconciler::Plan,
        filter_active: bool,
        module_miss: Option<String>,
    ) -> Self {
        Self {
            filter_active,
            unfiltered_total: plan.total_actions(),
            phases_with_work: plan
                .phases
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.name.display_name().to_string())
                .collect(),
            module_miss,
        }
    }
}

/// Emit the message for a plan that ended up with no in-scope actions.
///
/// Distinguishes a system that is genuinely in sync (`Ok` — "nothing to do")
/// from one where a scoping flag excluded pending work (`Warn` — the system was
/// *not* reconciled). Shared by both `apply` and `plan`/dry-run so the two
/// surfaces never diverge.
pub(in crate::cli) fn report_no_in_scope_actions(printer: &Printer, scope: &ScopeReport) {
    if let Some(name) = &scope.module_miss {
        printer.status_simple(
            Role::Warn,
            format!(
                "Module '{name}' matched no actions — it was not found or could not be resolved"
            ),
        );
        return;
    }
    if !scope.filter_active || scope.unfiltered_total == 0 {
        printer.status_simple(Role::Ok, MSG_NOTHING_TO_DO);
        return;
    }
    printer.status_simple(
        Role::Warn,
        format!(
            "No actions in scope — the active filter excluded all {} planned action(s); the system was not reconciled",
            scope.unfiltered_total
        ),
    );
    if !scope.phases_with_work.is_empty() {
        printer.hint(format!(
            "actions exist in phase(s): {}",
            scope.phases_with_work.join(", ")
        ));
    }
}

/// The one line that closes a preview: the planned count, or — when the plan
/// holds no in-scope work — the verdict [`report_no_in_scope_actions`] chooses.
///
/// `scope` is `None` for the surfaces that expose no scoping flag at all
/// (`cfgd init --apply`, `cfgd module create --apply`), where the only verdict
/// reachable is `MSG_NOTHING_TO_DO`; taking it directly there says that, where
/// a `ScopeReport` built solely to land on the same arm would not.
pub(in crate::cli) fn report_plan_verdict(
    printer: &Printer,
    total_actions: usize,
    scope: Option<&ScopeReport>,
) {
    if total_actions > 0 {
        printer.status_simple(Role::Info, format!("{total_actions} action(s) planned"));
        return;
    }
    match scope {
        Some(scope) => report_no_in_scope_actions(printer, scope),
        None => printer.status_simple(Role::Ok, MSG_NOTHING_TO_DO),
    }
}

/// Bundles `display_plan_preview`'s non-core arguments (everything but the
/// plan/printer/state it acts on) so the call stays under clippy's
/// too-many-arguments budget as fields accrue.
#[derive(Clone, Copy)]
pub(in crate::cli) struct PlanPreviewArgs<'a> {
    pub context: &'a str,
    pub phase_filter: Option<&'a PhaseFilter>,
    pub dry_run_fm: Option<&'a CfgdFileManager>,
    pub scope: &'a ScopeReport,
    pub pending_backups: &'a [String],
    /// The decisions this preview's plan was pruned with. The same value the
    /// run carries, so the block naming what is missing and the payload keys
    /// reporting it cannot describe different sets.
    pub withheld: &'a reconciler::WithheldDecisions,
}

pub(in crate::cli) fn display_plan_preview(
    run: &reconciler::ApplyRun<'_>,
    plan: &reconciler::Plan,
    printer: &Printer,
    args: &PlanPreviewArgs<'_>,
) {
    let PlanPreviewArgs {
        context,
        phase_filter,
        dry_run_fm,
        scope,
        pending_backups,
        withheld,
    } = *args;

    // The run's own rows and warnings, before anything this command adds: the
    // header is what states the scope every block below is read against, and
    // it is also what names the decisions the plan was pruned with.
    run.header(printer);

    // Build structured output
    let plan_output = build_plan_output(plan, context, phase_filter, pending_backups, withheld);

    // Structured-output routing: when -o yaml/json/etc., emit the plan as the
    // doc's data payload and skip the human render.
    if printer.is_structured() {
        printer.emit(Doc::new().with_data(&plan_output));
        return;
    }

    // Table mode display
    run.preview(printer);

    // Schedule-less backups are not reconciler actions (they always run, no
    // diff against desired state), so the preview tree above never holds one —
    // surface them separately so a preview doesn't silently omit work a real
    // (non-dry-run) apply would do.
    if !pending_backups.is_empty() {
        let section = printer.section("Backups (run on apply)");
        for name in pending_backups {
            section.status_simple(Role::Info, name);
        }
    }

    // Show diffs for file updates
    if let Some(fm) = dry_run_fm {
        for phase_item in &plan.phases {
            if phase_item.name != PhaseName::Files {
                continue;
            }
            for action in phase_item.actions() {
                if let reconciler::Action::File(FileAction::Update {
                    source,
                    target,
                    patch,
                    ..
                }) = action
                    && let Ok(target_content) = std::fs::read_to_string(target)
                {
                    // A `Patch` action has no source file: its preview is the
                    // target against what re-running the merge would produce.
                    let source_content = if let Some(spec) = patch {
                        match fm.evaluate_spec(spec, target, reconciler::ReconcileContext::Apply) {
                            Ok(outcome) => outcome.patched,
                            Err(e) => {
                                printer
                                    .status(
                                        Role::Warn,
                                        format!("cannot preview {}", target.posix()),
                                    )
                                    .detail(cfgd_core::output::collapse_to_subject_line(e));
                                continue;
                            }
                        }
                    } else if crate::files::is_tera_template(source) {
                        fm.render_template_for_display(source).unwrap_or_default()
                    } else {
                        std::fs::read_to_string(source).unwrap_or_default()
                    };
                    // `printer.diff` bypasses section header flushing; wrapping the
                    // file label in `section()` would render the header after the diff.
                    printer.heading(target.display_posix());
                    printer.diff(&target_content, &source_content);
                }
            }
        }
    }

    report_plan_verdict(printer, plan_output.total_actions, Some(scope));
}

// --- Plan filtering for --skip and --only ---

/// Compute the dot-notation resource path for an action.
/// Returns the phase-level prefix and the action-specific path components.
///
/// Examples:
///   PackageAction::Install { manager: "brew", packages: ["ripgrep"] } → "packages.brew"
///   SystemAction::SetValue { configurator: "sysctl", key: "net.ipv4.ip_forward" } → "system.sysctl.net.ipv4.ip_forward"
///   FileAction::Create { target: "/etc/foo" } → "files./etc/foo"
///   SecretAction::Resolve { provider: "1password" } → "secrets.1password"
///   ScriptAction::Run { path: "scripts/setup.sh" } → "scripts.scripts/setup.sh"
pub(in crate::cli) fn action_path(phase: &PhaseName, action: &reconciler::Action) -> String {
    let prefix = phase.as_str();
    match action {
        reconciler::Action::Package(pa) => {
            let manager = match pa {
                PackageAction::Install { manager, .. } => manager,
                PackageAction::Uninstall { manager, .. } => manager,
                PackageAction::Skip { manager, .. } => manager,
            };
            format!("{}.{}", prefix, manager)
        }
        reconciler::Action::System(sa) => match sa {
            reconciler::SystemAction::SetValue {
                configurator, key, ..
            } => format!("{}.{}.{}", prefix, configurator, key),
            reconciler::SystemAction::Skip { configurator, .. } => {
                format!("{}.{}", prefix, configurator)
            }
        },
        reconciler::Action::File(fa) => {
            let target = match fa {
                FileAction::Create { target, .. } => target,
                FileAction::Update { target, .. } => target,
                FileAction::Delete { target, .. } => target,
                FileAction::SetPermissions { target, .. } => target,
                FileAction::Skip { target, .. } => target,
            };
            format!("{}:{}", prefix, cfgd_core::to_posix_string(target))
        }
        reconciler::Action::Secret(sa) => match sa {
            SecretAction::Decrypt { target, .. } => {
                format!("{}:{}", prefix, cfgd_core::to_posix_string(target))
            }
            SecretAction::Resolve {
                provider,
                reference,
                ..
            } => format!("{}.{}.{}", prefix, provider, reference),
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                ..
            } => format!("{}.{}.{}:[{}]", prefix, provider, reference, envs.join(",")),
            SecretAction::Skip { source, .. } => {
                format!("{}.{}", prefix, source)
            }
        },
        reconciler::Action::Script(sa) => match sa {
            reconciler::ScriptAction::Run { entry, .. } => {
                format!("{}:{}", prefix, entry.run_str())
            }
        },
        reconciler::Action::Module(ma) => {
            // The owner gets its own segment: without it a module named `brew`
            // and the brew manager would share `packages.brew`.
            format!(
                "{}.{}:{}",
                prefix,
                reconciler::OwnerKind::Module.as_str(),
                ma.module_name
            )
        }
        reconciler::Action::Env(ea) => match ea {
            reconciler::EnvAction::WriteEnvFile { path, .. } => {
                format!("{}:{}", prefix, cfgd_core::to_posix_string(path))
            }
            reconciler::EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("{}:{}", prefix, cfgd_core::to_posix_string(rc_path))
            }
            reconciler::EnvAction::RefreshLiveSession { .. } => {
                format!("{}:live-session", prefix)
            }
        },
        // `ManagerAction::filter_subject`, so `--skip prerequisites.brew`
        // reaches brew's provision and `--skip prerequisites.curl` its
        // prerequisite — keyed on the TOOL, not the installer, in agreement
        // with `reconciler::action_matches_phase_filter`'s `--phase` matcher.
        // A sub-manager is already folded onto its family's node at plan
        // time, so the family name is what a pattern has to name.
        reconciler::Action::Manager(ma) => format!("{}.{}", prefix, ma.filter_subject()),
    }
}

/// The owner token a phase-qualified group alias (`prerequisites.managers`,
/// `prerequisites.env`, `prerequisites.session`) resolves to, if `pattern`
/// spells one — the alternate grammar for the `kind:name` owner-token check
/// `pattern_matches_action` already understands directly.
///
/// Boundary-checked against `action_path`'s own phase segment (everything
/// before its first `.`/`:`), so `foo.env` cannot alias `cfgd:env` for an
/// action that did not actually land in phase `foo` — the phase name is part
/// of what the user selected, not just the group.
fn phase_qualified_group_owner_token(pattern: &str, action_path: &str) -> Option<String> {
    let (phase, group) = pattern.split_once('.')?;
    if !reconciler::CFGD_GROUP_ORDER.contains(&group) {
        return None;
    }
    let phase_end = action_path.find(['.', ':'])?;
    if &action_path[..phase_end] != phase {
        return None;
    }
    Some(reconciler::Owner::cfgd(group).token())
}

/// Check if a pattern matches an action path.
/// A pattern is a prefix match: "packages.brew" matches "packages.brew.ripgrep".
/// For file/script paths using `:`, "files:" matches all files.
pub(in crate::cli) fn pattern_matches(pattern: &str, action_path: &str) -> bool {
    if action_path == pattern {
        return true;
    }
    // "packages" matches "packages.brew.ripgrep"
    // "packages.brew" matches "packages.brew.ripgrep"
    if action_path.starts_with(pattern) && action_path[pattern.len()..].starts_with(['.', ':']) {
        return true;
    }
    // "packages" should also match "packages:..." (colon-separated paths)
    false
}

/// Does `pattern` select this action? `owner` is the enclosing group's.
///
/// The four rules are ordered; none but the last is total, and the last
/// (`pattern_matches`) is the fallback every earlier rule that declines to
/// match falls through to. Rule 1 cannot shadow rule 3 because every
/// `action_path` begins with a `PhaseName::as_str()` and none of those is an
/// `OwnerKind` token; rule 2 cannot shadow it either, because after the
/// kind-phase routing the only paths starting with `modules.` are the
/// platform-gated skips rule 2 selects anyway. Rule 4 (the phase-qualified
/// group alias, `prerequisites.managers`/`.env`/`.session`) falls through
/// rather than returning `false` on a miss, so a pattern that only
/// COINCIDENTALLY looks like a group alias (a system configurator that
/// happens to be named `env`) still gets the literal match it would have
/// gotten without this rule existing.
pub(in crate::cli) fn pattern_matches_action(
    pattern: &str,
    owner: &reconciler::Owner,
    action_path: &str,
) -> bool {
    if let Some((kind, _name)) = pattern.split_once(':')
        && reconciler::OwnerKind::from_token(kind).is_some()
    {
        return owner.token() == pattern;
    }
    if pattern == LEGACY_MODULE_PATTERN {
        return owner.kind == reconciler::OwnerKind::Module;
    }
    if let Some(name) = pattern.strip_prefix(LEGACY_MODULE_PREFIX) {
        return owner.kind == reconciler::OwnerKind::Module && owner.name == name;
    }
    if let Some(token) = phase_qualified_group_owner_token(pattern, action_path)
        && owner.token() == token
    {
        return true;
    }
    pattern_matches(pattern, action_path)
}

/// The pre-routing spelling of "every module", kept working for one grammar
/// generation and announced as deprecated when it is used.
const LEGACY_MODULE_PATTERN: &str = "modules";
const LEGACY_MODULE_PREFIX: &str = "modules.";

/// The pre-merge spelling of the phase that now also provisions package
/// managers, as the leading segment of a `--skip`/`--only` path.
const LEGACY_ENV_PHASE: &str = "env";

/// `pattern` with a leading `env` phase segment rewritten to the phase's
/// current name, or `None` when it opens with anything else.
///
/// A path's phase segment ends at the first `.` (`env.something`) or `:`
/// (`env:/home/you/.bashrc`), so the whole grammar is covered by finding
/// either. An owner token (`cfgd:env`) opens with its kind and is untouched.
fn legacy_env_pattern_rewritten(pattern: &str) -> Option<String> {
    let end = pattern.find(['.', ':']).unwrap_or(pattern.len());
    (&pattern[..end] == LEGACY_ENV_PHASE)
        .then(|| format!("{}{}", PhaseName::Prerequisites.as_str(), &pattern[end..]))
}

/// Rewrite every legacy `env` phase segment to the phase's current name,
/// announcing each distinct pattern once.
///
/// Left alone, such a pattern would stop matching the moment the phase was
/// renamed — silently, since a pattern that selects nothing is indistinguishable
/// from one whose work is absent, and a run meaning to SKIP the env writes would
/// perform them.
fn normalize_legacy_phase_patterns(
    printer: &Printer,
    flag: &str,
    patterns: &[String],
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let Some(rewritten) = legacy_env_pattern_rewritten(pattern) else {
            out.push(pattern.clone());
            continue;
        };
        if !seen.contains(pattern) {
            seen.push(pattern.clone());
            printer.deprecation(format!(
                "`{flag} {pattern}` is deprecated: that phase now provisions package managers \
                 as well as writing the env file. Use `{flag} {rewritten}`."
            ));
        }
        out.push(rewritten);
    }
    out
}

fn is_legacy_module_pattern(pattern: &str) -> bool {
    pattern == LEGACY_MODULE_PATTERN || pattern.starts_with(LEGACY_MODULE_PREFIX)
}

/// One-line deprecation for each distinct legacy `modules[.name]` pattern the
/// run was given.
///
/// Per distinct pattern rather than once per run: a run passing two of them
/// would otherwise be told about one and keep the other until it breaks.
fn warn_legacy_module_patterns(printer: &Printer, skip: &[String], only: &[String]) {
    let mut seen: Vec<&str> = Vec::new();
    for (flag, pattern) in skip
        .iter()
        .map(|p| ("--skip", p))
        .chain(only.iter().map(|p| ("--only", p)))
    {
        if !is_legacy_module_pattern(pattern) || seen.contains(&pattern.as_str()) {
            continue;
        }
        seen.push(pattern);
        let replacement = match pattern.strip_prefix(LEGACY_MODULE_PREFIX) {
            Some(name) => {
                format!("Use `{flag} module:{name}` (all phases) or `{flag} files.module:{name}`.")
            }
            None => format!("Use `{flag} module:<name>` to select one module."),
        };
        printer.deprecation(format!(
            "`{flag} {pattern}` is deprecated: module work now applies in the phase whose kind it is. {replacement}"
        ));
    }
}

/// Check if a file target is an unmanaged file — exists on disk but not tracked by cfgd.
/// A cfgd-managed symlink (pointing into config_dir) is NOT unmanaged.
pub(in crate::cli) fn is_unmanaged_file(
    target: &Path,
    config_dir: &Path,
    state: &StateStore,
) -> bool {
    // Target must exist on disk
    if !target.exists() && target.symlink_metadata().is_err() {
        return false;
    }

    // If it's a symlink pointing into the config dir, it's cfgd-managed
    if let Ok(link_target) = target.read_link() {
        if link_target.starts_with(config_dir) {
            return false;
        }
        // Also check ~/.cache/cfgd/modules/ for module symlinks
        {
            let module_cache = cfgd_core::expand_tilde(Path::new("~/.cache/cfgd/modules"));
            if link_target.starts_with(&module_cache) {
                return false;
            }
        }
    }

    // Check state store — if already tracked, it's managed
    let target_str = target.display().to_string();
    if let Ok(managed) = state.is_resource_managed("file", &target_str) {
        return !managed;
    }

    true
}

/// Whether a strategy adopts an existing unmanaged target in place instead of
/// replacing it.
///
/// `Patch` merges into the target's own bytes, so the unmanaged-file prompt
/// must never fire for it: every one of its choices is wrong. "Adopt
/// (overwrite)" misdescribes a merge, and "Backup" renames the target away
/// *before* apply — the merge would then read an empty current content and
/// write only the ensured keys, destroying exactly the content the strategy
/// exists to preserve.
fn adopts_in_place(strategy: FileStrategy) -> bool {
    matches!(strategy, FileStrategy::Patch)
}

pub(in crate::cli) fn handle_unmanaged_file_targets(
    plan: &mut reconciler::Plan,
    config_dir: &Path,
    state: &StateStore,
    printer: &Printer,
    auto_yes: bool,
) -> anyhow::Result<()> {
    let options = vec![
        "Adopt (overwrite with cfgd-managed version)".to_string(),
        "Backup (save as .cfgd-backup, then overwrite)".to_string(),
        "Skip (leave file untouched)".to_string(),
    ];

    for phase in &mut plan.phases {
        for (_owner, actions) in phase.groups_mut() {
            let mut i = 0;
            while i < actions.len() {
                // Profile file actions
                if let reconciler::Action::File(
                    FileAction::Create {
                        target, strategy, ..
                    }
                    | FileAction::Update {
                        target, strategy, ..
                    },
                ) = &actions[i]
                {
                    let target = target.clone();
                    let strategy = *strategy;
                    if !adopts_in_place(strategy)
                        && is_unmanaged_file(&target, config_dir, state)
                        && !auto_yes
                    {
                        let choice = prompt_backup_choice(&target, None, printer, &options)?;
                        apply_backup_choice(choice, &target, &mut actions[i], printer)?;
                    }
                }

                // Module file actions
                if let reconciler::Action::Module(ref ma) = actions[i]
                    && let reconciler::ModuleActionKind::DeployFiles { files } = &ma.kind
                {
                    let needs_prompt = !auto_yes
                        && files.iter().any(|f| {
                            let t = cfgd_core::expand_tilde(&f.target);
                            !f.strategy.is_some_and(adopts_in_place)
                                && is_unmanaged_file(&t, config_dir, state)
                        });
                    if needs_prompt {
                        let module_name = ma.module_name.clone();
                        if let reconciler::Action::Module(ref mut ma) = actions[i]
                            && let reconciler::ModuleActionKind::DeployFiles { ref mut files } =
                                ma.kind
                        {
                            let mut j = 0;
                            while j < files.len() {
                                let file_target = cfgd_core::expand_tilde(&files[j].target);
                                if !files[j].strategy.is_some_and(adopts_in_place)
                                    && is_unmanaged_file(&file_target, config_dir, state)
                                {
                                    let choice = prompt_backup_choice(
                                        &file_target,
                                        Some(&module_name),
                                        printer,
                                        &options,
                                    )?;
                                    if choice.starts_with("Backup") {
                                        backup_file(&file_target, printer)?;
                                    } else if choice.starts_with("Skip") {
                                        files.remove(j);
                                        continue;
                                    }
                                }
                                j += 1;
                            }
                        }
                    }
                }

                i += 1;
            }
        }
    }

    Ok(())
}

/// Prompt the user to choose how to handle an unmanaged file target.
fn prompt_backup_choice<'a>(
    target: &Path,
    module_name: Option<&str>,
    printer: &Printer,
    options: &'a [String],
) -> anyhow::Result<&'a String> {
    let msg = if let Some(m) = module_name {
        format!(
            "Module '{}': target exists as unmanaged file: {}",
            m,
            target.posix()
        )
    } else {
        format!("Target exists as unmanaged file: {}", target.posix())
    };
    printer.status_simple(Role::Warn, msg);
    Ok(printer
        .prompt_select("How should cfgd handle this file?", options)
        .unwrap_or(&options[0]))
}

pub(in crate::cli) fn backup_file(target: &Path, printer: &Printer) -> anyhow::Result<()> {
    let backup_path = PathBuf::from(format!("{}.cfgd-backup", target.display()));
    std::fs::rename(target, &backup_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to backup {} to {}: {}",
            target.posix(),
            backup_path.posix(),
            e
        )
    })?;
    printer.status_simple(Role::Ok, format!("Backed up to {}", backup_path.posix()));
    Ok(())
}

pub(in crate::cli) fn apply_backup_choice(
    choice: &str,
    target: &Path,
    action: &mut reconciler::Action,
    printer: &Printer,
) -> anyhow::Result<()> {
    if choice.starts_with("Backup") {
        backup_file(target, printer)?;
    } else if choice.starts_with("Skip") {
        let origin = match action {
            reconciler::Action::File(FileAction::Create { origin, .. })
            | reconciler::Action::File(FileAction::Update { origin, .. }) => origin.clone(),
            _ => LOCAL_LAYER.to_string(),
        };
        *action = reconciler::Action::File(FileAction::Skip {
            target: target.to_path_buf(),
            reason: "skipped by user (unmanaged file exists)".to_string(),
            origin,
        });
    }
    Ok(())
}

/// Apply --skip and --only filters to a plan, modifying it in place.
///
/// Also emits the two diagnostics that only filtering can produce: the
/// deprecation for a legacy `modules[.name]` pattern, and the stranded-install
/// warning for a filter that removed a package manager's bootstrap without
/// removing the installs that need it. Both live here so neither call site can
/// forget one.
pub(in crate::cli) fn filter_plan(
    plan: &mut reconciler::Plan,
    skip: &[String],
    only: &[String],
    printer: &Printer,
    registry: &ProviderRegistry,
) {
    if skip.is_empty() && only.is_empty() {
        return;
    }
    warn_legacy_module_patterns(printer, skip, only);
    let skip = &normalize_legacy_phase_patterns(printer, "--skip", skip);
    let only = &normalize_legacy_phase_patterns(printer, "--only", only);

    let mut removals = BootstrapRemovals::default();
    for phase in &mut plan.phases {
        let phase_name = phase.name.clone();
        for (owner, actions) in phase.groups_mut() {
            let mut filtered_actions = Vec::new();

            for action in std::mem::take(actions) {
                // Package install/uninstall actions need per-package granularity
                if let reconciler::Action::Package(ref pa) = action {
                    match pa {
                        PackageAction::Install {
                            manager,
                            packages,
                            origin,
                        } => {
                            let kept = filter_package_list(
                                phase_name.as_str(),
                                owner,
                                manager,
                                packages,
                                skip,
                                only,
                            );
                            if !kept.is_empty() {
                                filtered_actions.push(reconciler::Action::Package(
                                    PackageAction::Install {
                                        manager: manager.clone(),
                                        packages: kept,
                                        origin: origin.clone(),
                                    },
                                ));
                            }
                            continue;
                        }
                        PackageAction::Uninstall {
                            manager,
                            packages,
                            origin,
                        } => {
                            let kept = filter_package_list(
                                phase_name.as_str(),
                                owner,
                                manager,
                                packages,
                                skip,
                                only,
                            );
                            if !kept.is_empty() {
                                filtered_actions.push(reconciler::Action::Package(
                                    PackageAction::Uninstall {
                                        manager: manager.clone(),
                                        packages: kept,
                                        origin: origin.clone(),
                                    },
                                ));
                            }
                            continue;
                        }
                        _ => {}
                    }
                }

                // Non-package actions: action-level filtering
                let path = action_path(&phase_name, &action);
                let matched_skip = skip
                    .iter()
                    .find(|s| pattern_matches_action(s, owner, &path));
                let passes_only = only.is_empty()
                    || only.iter().any(|o| {
                        pattern_matches_action(o, owner, &path) || pattern_matches(&path, o)
                    });

                if matched_skip.is_none() && passes_only {
                    filtered_actions.push(action);
                    continue;
                }
                // The `Prerequisites` node that provisions the manager. Filtered
                // away, it strands the installs that needed the manager.
                if let reconciler::Action::Manager(reconciler::ManagerAction::Provision {
                    manager,
                    ..
                }) = &action
                {
                    removals.record(manager, matched_skip.map(String::as_str));
                }
            }

            *actions = filtered_actions;
        }
        phase.prune_empty_groups();
    }

    // A `--skip`/`--only` pattern can empty a group without touching its
    // siblings, and can empty a phase outright, so both must be re-checked here
    // rather than assumed empty-safe from `Reconciler::plan` alone.
    plan.phases.retain(|p| !p.is_empty());

    // Skipping a PACKAGE that was a manager's last surviving consumer leaves
    // that manager's Provision/RefreshIndex nodes with nothing left to serve —
    // silently prune them (the machine's own bookkeeping), distinct from
    // skipping the manager ITSELF, which strands its consumers and earns the
    // alert below.
    //
    // Skip-direction only: `--only` is explicit selection, and a node the
    // user named directly is its own justification. `--only
    // prerequisites.managers` (the docs' own recovery command) keeps every
    // manager node and nothing else — running the consumer-prune against
    // that plan would see zero surviving package installs (`--only` dropped
    // them all) and delete every manager node it just kept, which is not a
    // prune, it is undoing the selection.
    if only.is_empty() {
        reconciler::prune_to_surviving_consumers(plan);
        plan.phases.retain(|p| !p.is_empty());
    }

    warn_stranded_installs(plan, printer, registry, &removals);
}

/// The bootstraps a filter removed, and the `--skip` pattern that removed each.
#[derive(Default)]
struct BootstrapRemovals {
    /// Manager families (`manager.split('-').next()`) whose bootstrap is gone.
    families: Vec<String>,
    /// Distinct `--skip` patterns responsible. Empty when `--only` did it, since
    /// no single pattern is then to blame.
    patterns: Vec<String>,
    /// Bootstrap actions removed, counted rather than derived from `families`
    /// because two managers can share a family.
    count: usize,
}

impl BootstrapRemovals {
    fn record(&mut self, manager: &str, pattern: Option<&str>) {
        self.count += 1;
        let family = manager_family(manager).to_string();
        if !self.families.contains(&family) {
            self.families.push(family);
        }
        if let Some(p) = pattern
            && !self.patterns.iter().any(|seen| seen == p)
        {
            self.patterns.push(p.to_string());
        }
    }
}

/// Warn when filtering removed a bootstrap but left installs that need the
/// manager it would have provided.
///
/// The removed-bootstrap set is pre-filter minus post-filter, so the warning is
/// pattern-agnostic: `--skip packages.brew` strands `packages.brew-tap` by
/// exactly the mechanism `--skip cfgd:managers` strands `packages.brew`, and
/// `pattern_matches`' segment boundary guarantees the user's own flag did not
/// cover it.
fn warn_stranded_installs(
    plan: &reconciler::Plan,
    printer: &Printer,
    registry: &ProviderRegistry,
    removals: &BootstrapRemovals,
) {
    if removals.count == 0 {
        return;
    }
    // Two counts, deliberately: the number the user is warned about is how many
    // ACTIONS will not apply, while the `--skip` flags they are handed are per
    // MANAGER. Reporting `stranded.len()` for both undercounts every time one
    // manager strands more than one install.
    let mut stranded: Vec<String> = Vec::new();
    let mut stranded_actions = 0usize;
    for action in plan.phases.iter().flat_map(|p| p.actions()) {
        let reconciler::Action::Package(PackageAction::Install { manager, .. }) = action else {
            continue;
        };
        if !removals
            .families
            .iter()
            .any(|f| f == manager_family(manager))
        {
            continue;
        }
        let available = registry
            .package_managers
            .iter()
            .any(|pm| pm.name() == manager && pm.is_available());
        if !available {
            stranded_actions += 1;
            if !stranded.iter().any(|m| m == manager) {
                stranded.push(manager.clone());
            }
        }
    }
    if stranded.is_empty() {
        return;
    }
    let culprit = match removals.patterns.as_slice() {
        [one] => format!("`--skip {one}`"),
        _ => "the active filter".to_string(),
    };
    let flags = stranded
        .iter()
        .map(|m| format!("--skip packages.{m}"))
        .collect::<Vec<_>>()
        .join(" ");
    printer.alert(format!(
        "{culprit} removes {} bootstrap(s); {stranded_actions} package action(s) still name a manager that is not installed. They will not apply. Use `{flags}` to drop that work too.",
        removals.count,
    ));
}

/// Filter individual packages from an install/uninstall list based on skip/only patterns.
fn filter_package_list(
    phase: &str,
    owner: &reconciler::Owner,
    manager: &str,
    packages: &[String],
    skip: &[String],
    only: &[String],
) -> Vec<String> {
    packages
        .iter()
        .filter(|pkg| {
            let pkg_path = format!("{}.{}.{}", phase, manager, pkg);

            // Check skip: pattern can target the specific package, manager, phase or owner
            let pkg_skip = skip
                .iter()
                .any(|s| pattern_matches_action(s, owner, &pkg_path));

            // Check only: the pattern must cover this package.
            // "packages" covers "packages.brew.ripgrep" (broad → specific)
            // "packages.brew.ripgrep" covers "packages.brew.ripgrep" (exact)
            // But "packages.brew.ripgrep" does NOT cover "packages.brew.fd"
            let pkg_only = only.is_empty()
                || only.iter().any(|o| {
                    pattern_matches_action(o, owner, &pkg_path) || pattern_matches(&pkg_path, o)
                });

            !pkg_skip && pkg_only
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
