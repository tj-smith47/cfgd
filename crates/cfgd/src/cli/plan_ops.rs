use std::collections::HashSet;

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

/// Render the run's closing `Caveats` section: every note providers reported
/// during the run, plus (when this apply touched the shell env surface) the
/// re-source reminder that used to print under its own "Shell environment
/// changed" heading — folded into the `cfgd:env` group on real provenance
/// rather than a heading invented for it alone.
///
/// Groups render informational-first, `cfgd:env` always last: it is the one
/// group that is action-required (the reader has to actually run the command),
/// so it is the last thing printed before the prompt returns. Every other
/// group keeps the order `ApplyResult::caveats` collected it in.
///
/// The re-source text is gated purely on the descriptions `apply_env_action`
/// returns — it suffixes `:skipped` when the on-disk bytes already matched —
/// so nothing here re-stats the filesystem and races whatever ran after the
/// Env phase.
pub(in crate::cli) fn print_caveats(
    result: &cfgd_core::reconciler::ApplyResult,
    printer: &Printer,
) {
    let mut caveats = result.caveats.clone();
    let env_owner = cfgd_core::reconciler::Owner::cfgd("env");

    if let Some(reminder) = shell_env_reminder_note(result) {
        match caveats.iter_mut().find(|(owner, _)| *owner == env_owner) {
            Some((_, notes)) => notes.push(reminder),
            None => caveats.push((env_owner.clone(), vec![reminder])),
        }
    }

    // `cfgd:env` renders last, after every informational group: it is the one
    // group that is action-required (the reader has to actually run its
    // command). Not a general owner ordering — `Owner::sort_key` is the only
    // one of those, applied where `Phase::from_actions` builds the phase
    // tree's groups — so this stays a one-off split-and-append on the single
    // fixed `cfgd:env` owner rather than a second comparator.
    let (mut informational, env_last): (Vec<_>, Vec<_>) = caveats
        .into_iter()
        .partition(|(owner, _)| *owner != env_owner);
    informational.extend(env_last);

    cfgd_core::reconciler::render_caveats(printer, &informational);
}

/// The `cfgd:env` re-source reminder, as a note — `None` when this apply
/// never touched a shell env file worth sourcing.
fn shell_env_reminder_note(
    result: &cfgd_core::reconciler::ApplyResult,
) -> Option<cfgd_core::providers::ActionNote> {
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
        return None;
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

    Some(cfgd_core::providers::ActionNote::untagged(
        Role::Warn,
        format!("run `{command}` — or open a new shell"),
    ))
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
            batched: Vec::new(),
            reason: None,
        },
        reconciler::ManagerAction::Provision {
            manager,
            via,
            batched,
            ..
        } => ManagerActionOutput {
            manager: manager.clone(),
            state: "provisioned".to_string(),
            via: Some(via.clone()),
            requires,
            batched: batched.clone(),
            reason: None,
        },
        reconciler::ManagerAction::Prerequisite {
            tool, installer, ..
        } => ManagerActionOutput {
            manager: tool.clone(),
            state: "prerequisite".to_string(),
            via: Some(installer.clone()),
            requires,
            batched: Vec::new(),
            reason: None,
        },
        reconciler::ManagerAction::Refuse { manager, reason } => ManagerActionOutput {
            manager: manager.clone(),
            state: "refused".to_string(),
            via: None,
            requires,
            batched: Vec::new(),
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
    ctx: &RunContext<'_>,
    state: &cfgd_core::state::StateStore,
    cfg: &cfgd_core::config::CfgdConfig,
    resolved: &cfgd_core::config::ResolvedProfile,
    config_parsed: bool,
    writes: DecisionWrites<'_>,
    actual: &reconciler::ActualPackages,
) -> anyhow::Result<(
    reconciler::WithheldDecisions,
    reconciler::SourcePolicyReview,
)> {
    let mut local = reconciler::local_profile(resolved);
    ctx.resolve_manifest_packages(&mut local.packages)?;

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
/// when a scoping flag (`--phase`/`--only`/`--skip`/`--skip-scripts`)
/// excluded real, pending work — telling the user the system is in sync when it
/// is not. `--module` is not one of these: it resolves atomically (an
/// unresolvable name is a propagated error, not a scope reduction), so a
/// `--module`-scoped plan that ends up empty genuinely has nothing pending
/// for that module.
pub(in crate::cli) struct ScopeReport {
    /// Any scoping flag that can narrow the plan to a subset was set.
    pub filter_active: bool,
    /// Total actions the plan held before `--skip`/`--only` pruning.
    pub unfiltered_total: usize,
    /// Display names of the phases that held actions before pruning.
    pub phases_with_work: Vec<String>,
    /// At least one `--skip`/`--only` token matched zero actions
    /// ([`TokenHits::misses`], set by `filter_plan` after it runs). A plan
    /// that ends up empty for THIS reason is not "up to date" — a filter
    /// token that never resolved anything is not evidence the machine
    /// converged, so this must override the `unfiltered_total == 0` shortcut
    /// below even when the profile itself had nothing else pending.
    pub filter_miss: bool,
}

impl ScopeReport {
    pub(in crate::cli) fn capture(plan: &reconciler::Plan, filter_active: bool) -> Self {
        Self {
            filter_active,
            unfiltered_total: plan.total_actions(),
            phases_with_work: plan
                .phases
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.name.display_name().to_string())
                .collect(),
            filter_miss: false,
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
    if !scope.filter_active || (scope.unfiltered_total == 0 && !scope.filter_miss) {
        printer.status_simple(Role::Ok, MSG_NOTHING_TO_DO);
        return;
    }
    printer.status_simple(
        Role::Warn,
        format!(
            "No actions in scope — the active filter excluded all {} planned; the system was not reconciled",
            cfgd_core::pluralize(scope.unfiltered_total, "action")
        ),
    );
    if !scope.phases_with_work.is_empty() {
        printer.hint(format!(
            "actions exist in {}: {}",
            cfgd_core::plural_noun(scope.phases_with_work.len(), "phase"),
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
        printer.status_simple(
            Role::Info,
            format!("{} planned", cfgd_core::pluralize(total_actions, "action")),
        );
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
                    printer
                        .section(target.display_posix())
                        .diff(&target_content, &source_content);
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

    // Check state store — if already tracked, it's managed. The id is minted
    // posix-folded (`reconciler::format`), so the lookup folds too: asked with
    // native separators, every managed file on Windows answers "unmanaged" and
    // the conflict pass copies cfgd's OWN files aside on every apply.
    let target_str = cfgd_core::to_posix_string(target);
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

/// Whether `target` already holds exactly the bytes the planned write would
/// put there.
///
/// The adoption short-circuit: a converged target is not a conflict, so it must
/// not be prompted about, copied aside, or rewritten. `desired_hash` is `None`
/// whenever the content is not knowable ahead of the write — a link strategy, a
/// `Patch` merge, an unreadable source — and that answers "not converged", so
/// the conflict path runs exactly as before.
///
/// Judged on `symlink_metadata`, never `exists()`: a symlink at the target is a
/// thing to replace, not content to compare, however its destination reads.
fn target_holds_desired_content(target: &Path, desired_hash: Option<&str>) -> bool {
    let Some(want) = desired_hash else {
        return false;
    };
    let Ok(meta) = target.symlink_metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    match std::fs::read(target) {
        Ok(bytes) => cfgd_core::sha256_hex(&bytes) == want,
        Err(_) => false,
    }
}

/// The hash of the bytes a module file deployment will write, when that is
/// answerable before the deployment runs.
///
/// Only a `Copy`/`Template` entry writes whole content (both read the source
/// verbatim in `reconciler::modules`); a link entry replaces the target with a
/// link and a `Patch` entry merges into whatever the target already holds, so
/// neither has a comparable "desired content" at all.
///
/// `strategy` is the RESOLVED strategy, matching what `reconciler::modules`
/// will act on: a file declaring none of its own under a global
/// `fileStrategy: copy` writes whole content just the same, and reading the
/// unresolved field would answer `None` and re-adopt it on every apply.
fn module_file_desired_hash(
    file: &cfgd_core::modules::ResolvedFile,
    strategy: FileStrategy,
) -> Option<String> {
    if !matches!(strategy, FileStrategy::Copy | FileStrategy::Template) {
        return None;
    }
    if !file.source.is_file() {
        return None;
    }
    std::fs::read(&file.source)
        .ok()
        .map(|bytes| cfgd_core::sha256_hex(&bytes))
}

/// A conflict policy that has been SETTLED for one target.
///
/// [`OnConflict::Ask`] is a request, never an outcome: by the time a target is
/// acted on, the question has been answered — by the prompt, by `--yes`, or by
/// there being nobody to ask. Giving the answer its own type without an `Ask`
/// variant is what makes "ask" unrepresentable at the executors, where it had
/// been folded into their `Overwrite` catch-all — so a run that asked to be
/// asked, and could not be, destroyed the file instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum ResolvedConflict {
    /// Copy the existing file aside, then write.
    Backup,
    /// Replace the existing file, keeping no copy of it.
    Overwrite,
    /// Leave the existing file alone and drop cfgd's write.
    Skip,
    /// Abort the apply without touching the file.
    Fail,
}

/// Resolve the run's `--on-conflict` request into the policy every target gets,
/// or `None` when each target must be asked about individually.
///
/// `--yes` means "do not stop to ask", never "discard my file", so the skipped
/// question lands on the safe policy rather than on the destructive one. Every
/// other policy is explicit and passes through. A run that WOULD ask but has
/// nobody to ask is settled one target at a time by
/// [`prompt_conflict_policy`], which lands on the same policy.
pub(in crate::cli) fn resolve_conflict_policy(
    requested: OnConflict,
    auto_yes: bool,
) -> Option<ResolvedConflict> {
    match requested {
        OnConflict::Ask if auto_yes => Some(ResolvedConflict::Backup),
        OnConflict::Ask => None,
        OnConflict::Backup => Some(ResolvedConflict::Backup),
        OnConflict::Overwrite => Some(ResolvedConflict::Overwrite),
        OnConflict::Skip => Some(ResolvedConflict::Skip),
        OnConflict::Fail => Some(ResolvedConflict::Fail),
    }
}

/// The prompt options, in policy order; index-matched to [`PROMPT_POLICIES`].
fn conflict_prompt_options() -> Vec<String> {
    vec![
        format!("Backup (copy to <target>{CFGD_BACKUP_SUFFIX}, then overwrite)"),
        "Overwrite (replace it, keeping no copy)".to_string(),
        "Skip (leave the file untouched)".to_string(),
        "Abort (stop the apply without touching the file)".to_string(),
    ]
}

/// The policy each [`conflict_prompt_options`] entry selects, same order.
///
/// Every `--on-conflict` value with an outcome appears here: the interactive
/// user chooses from the same vocabulary the flag offers, so nothing is
/// reachable only by re-running with a flag.
const PROMPT_POLICIES: [ResolvedConflict; 4] = [
    ResolvedConflict::Backup,
    ResolvedConflict::Overwrite,
    ResolvedConflict::Skip,
    ResolvedConflict::Fail,
];

/// The reason a target skipped for holding an unmanaged file reports, shared by
/// the profile action's `Skip` reason and the module arm's status line so the
/// two cannot describe the same decision differently.
const UNMANAGED_SKIP_REASON: &str = "skipped: target exists as unmanaged file";

pub(in crate::cli) fn handle_unmanaged_file_targets(
    plan: &mut reconciler::Plan,
    config_dir: &Path,
    state: &StateStore,
    printer: &Printer,
    auto_yes: bool,
    requested: OnConflict,
    default_strategy: FileStrategy,
) -> anyhow::Result<()> {
    let policy = resolve_conflict_policy(requested, auto_yes);
    let options = conflict_prompt_options();
    // Targets the pass decided to leave alone. Planning emits a `SetPermissions`
    // as a SIBLING of the write, so rewriting only the write leaves a chmod
    // behind — and "skip" would still change the mode of the file it promised
    // not to touch.
    let mut skipped: Vec<PathBuf> = Vec::new();

    for phase in &mut plan.phases {
        for (_owner, actions) in phase.groups_mut() {
            let mut i = 0;
            while i < actions.len() {
                // Profile file actions
                if let reconciler::Action::File(
                    FileAction::Create {
                        target,
                        strategy,
                        source_hash,
                        ..
                    }
                    | FileAction::Update {
                        target,
                        strategy,
                        source_hash,
                        ..
                    },
                ) = &actions[i]
                {
                    let target = target.clone();
                    let strategy = *strategy;
                    let desired = source_hash.clone();
                    if !adopts_in_place(strategy)
                        && !target_holds_desired_content(&target, desired.as_deref())
                        && is_unmanaged_file(&target, config_dir, state)
                    {
                        let chosen = resolve_for_target(policy, &target, None, printer, &options)?;
                        if chosen == ResolvedConflict::Skip {
                            skipped.push(target.clone());
                        }
                        apply_conflict_policy(chosen, &target, &mut actions[i], printer)?;
                    }
                }

                // Module file actions
                if let reconciler::Action::Module(ref mut ma) = actions[i]
                    && let reconciler::ModuleActionKind::DeployFiles { ref mut files } = ma.kind
                {
                    let module_name = ma.module_name.clone();
                    let mut j = 0;
                    while j < files.len() {
                        let file_target = cfgd_core::expand_tilde(&files[j].target);
                        let strategy = files[j].strategy.unwrap_or(default_strategy);
                        let desired = module_file_desired_hash(&files[j], strategy);
                        if !adopts_in_place(strategy)
                            && !target_holds_desired_content(&file_target, desired.as_deref())
                            && is_unmanaged_file(&file_target, config_dir, state)
                        {
                            let chosen = resolve_for_target(
                                policy,
                                &file_target,
                                Some(&module_name),
                                printer,
                                &options,
                            )?;
                            match chosen {
                                ResolvedConflict::Backup => {
                                    backup_file(&file_target, printer)?;
                                }
                                ResolvedConflict::Skip => {
                                    // A dropped module file leaves no action to
                                    // render, so the decision is reported here
                                    // or nowhere — the profile arm's `Skip`
                                    // action says the same thing in the tree.
                                    printer.status_simple(
                                        Role::Skipped,
                                        format!(
                                            "module '{}': {} — {}",
                                            module_name,
                                            file_target.posix(),
                                            UNMANAGED_SKIP_REASON
                                        ),
                                    );
                                    skipped.push(file_target);
                                    files.remove(j);
                                    continue;
                                }
                                ResolvedConflict::Fail => {
                                    return Err(unmanaged_conflict_error(
                                        &file_target,
                                        Some(&module_name),
                                    ));
                                }
                                ResolvedConflict::Overwrite => {}
                            }
                        }
                        j += 1;
                    }
                }

                i += 1;
            }
        }
    }

    prune_skipped_leftovers(plan, &skipped);
    Ok(())
}

/// Clear away what a skipped target leaves behind in the plan.
///
/// Two leftovers, both of which contradict what "skip" was told to mean:
///
/// - the sibling `SetPermissions` planning pairs with every `Create`/`Update`.
///   Left in place, `--on-conflict skip` still changes the mode of the file it
///   undertook to leave untouched — a smaller edit than the write, and the same
///   broken promise. Swept over the whole plan rather than the neighbouring
///   index, so a phase that groups the pair differently cannot reintroduce it
/// - a module deployment whose every file was skipped, which would otherwise
///   render and journal a deployment of nothing
fn prune_skipped_leftovers(plan: &mut reconciler::Plan, skipped: &[PathBuf]) {
    for phase in &mut plan.phases {
        phase.retain_actions(|action| match action {
            reconciler::Action::File(FileAction::SetPermissions { target, .. }) => {
                !skipped.iter().any(|s| s == target)
            }
            reconciler::Action::Module(ma) => !matches!(
                &ma.kind,
                reconciler::ModuleActionKind::DeployFiles { files } if files.is_empty()
            ),
            _ => true,
        });
    }
    plan.phases.retain(|p| !p.is_empty());
}

/// The policy to apply to one conflicting target: the run's policy, or the
/// answer to a per-file prompt when the run's policy is `Ask`.
fn resolve_for_target(
    policy: Option<ResolvedConflict>,
    target: &Path,
    module_name: Option<&str>,
    printer: &Printer,
    options: &[String],
) -> anyhow::Result<ResolvedConflict> {
    if let Some(settled) = policy {
        announce_conflict(target, module_name, printer);
        return Ok(settled);
    }
    prompt_conflict_policy(target, module_name, printer, options)
}

/// Say which file is in the way before anything is done about it.
fn announce_conflict(target: &Path, module_name: Option<&str>, printer: &Printer) {
    let msg = match module_name {
        Some(m) => format!(
            "Module '{}': target exists as unmanaged file: {}",
            m,
            target.posix()
        ),
        None => format!("Target exists as unmanaged file: {}", target.posix()),
    };
    printer.status_simple(Role::Warn, msg);
}

/// The `--on-conflict fail` abort, worded the same for a profile file and a
/// module one.
fn unmanaged_conflict_error(target: &Path, module_name: Option<&str>) -> anyhow::Error {
    match module_name {
        Some(m) => anyhow::anyhow!(
            "module '{}': target exists as unmanaged file: {} (--on-conflict fail)",
            m,
            target.posix()
        ),
        None => anyhow::anyhow!(
            "target exists as unmanaged file: {} (--on-conflict fail)",
            target.posix()
        ),
    }
}

/// Ask the user how to handle one unmanaged file target.
///
/// Two failures to read an answer are not the same event and must not resolve
/// the same way. A prompt that cannot be REACHED — no tty, structured output —
/// answers `Backup`, matching what [`resolve_conflict_policy`] would have
/// chosen had the run known in advance there was nobody to ask. A prompt the
/// user INTERRUPTED (Ctrl-C) or cancelled (Esc) is an answer: stop. Landing
/// that on a policy would carry out, file by file, the work they interrupted to
/// prevent, and interrupting again would only be read the same way.
fn prompt_conflict_policy(
    target: &Path,
    module_name: Option<&str>,
    printer: &Printer,
    options: &[String],
) -> anyhow::Result<ResolvedConflict> {
    announce_conflict(target, module_name, printer);
    match printer.prompt_select("How should cfgd handle this file?", options) {
        Ok(choice) => Ok(options
            .iter()
            .position(|o| o == choice)
            .and_then(|idx| PROMPT_POLICIES.get(idx).copied())
            .unwrap_or(ResolvedConflict::Backup)),
        Err(e) => settle_prompt_failure(e, target),
    }
}

/// Turn a failure to READ an answer into either an abort or the safe policy.
///
/// Split out from [`prompt_conflict_policy`] because no harness can seed an
/// interrupted prompt, while the classification is exactly the part that must
/// not regress: a Ctrl-C read as "nobody to ask" carries on doing the work it
/// was pressed to stop.
fn settle_prompt_failure(
    err: inquire::InquireError,
    target: &Path,
) -> anyhow::Result<ResolvedConflict> {
    match err {
        inquire::InquireError::OperationInterrupted | inquire::InquireError::OperationCanceled => {
            Err(anyhow::anyhow!(
                "interrupted at the unmanaged-file prompt for {}; nothing was applied",
                target.posix()
            ))
        }
        _ => Ok(ResolvedConflict::Backup),
    }
}

/// Copy an unmanaged target aside before cfgd overwrites it, and return where
/// the copy landed.
///
/// A COPY, never a rename. The rename this replaced left a window in which the
/// user's file was at neither path: a crash between the rename and the apply's
/// own write lost it outright. Copying leaves the original at `target` until
/// the write rename-replaces it, so at every instant the content exists at the
/// sidecar, at the target, or at both.
///
/// The copied bytes are re-read and hashed before the copy is reported, so a
/// short write or a full disk is an error rather than a sidecar that silently
/// holds less than the file it claims to preserve.
pub(in crate::cli) fn backup_file(target: &Path, printer: &Printer) -> anyhow::Result<PathBuf> {
    let meta = target
        .symlink_metadata()
        .map_err(|e| anyhow::anyhow!("Failed to backup {}: {}", target.posix(), e))?;

    if meta.file_type().is_symlink() {
        let dest = target
            .read_link()
            .map_err(|e| anyhow::anyhow!("Failed to backup symlink {}: {}", target.posix(), e))?;
        // Reserved unoccupied, so the link is created rather than replacing
        // whatever a previous adoption left — including a dangling link, which
        // `symlink_metadata` still counts as an entry someone made.
        let backup_path = reserve_backup_path(target, None)?;
        cfgd_core::create_symlink(&dest, &backup_path)
            .map_err(|e| anyhow::anyhow!("Failed to backup {}: {}", target.posix(), e))?;
        printer.status_simple(Role::Ok, format!("Backed up to {}", backup_path.posix()));
        return Ok(backup_path);
    }

    if meta.is_dir() {
        // Same reservation, and load-bearing here: `copy_dir_recursive` writes
        // INTO an existing directory, so an occupied sidecar would silently
        // merge two different originals into one tree.
        let backup_path = reserve_backup_path(target, None)?;
        cfgd_core::copy_dir_recursive(target, &backup_path)
            .map_err(|e| anyhow::anyhow!("Failed to backup {}: {}", target.posix(), e))?;
        printer.status_simple(Role::Ok, format!("Backed up to {}", backup_path.posix()));
        return Ok(backup_path);
    }

    let content = std::fs::read(target)
        .map_err(|e| anyhow::anyhow!("Failed to backup {}: {}", target.posix(), e))?;
    let hash = cfgd_core::sha256_hex(&content);
    let backup_path = reserve_backup_path(target, Some(&hash))?;
    // An earlier adoption already preserved these exact bytes; rewriting the
    // sidecar would only widen the window in which it is half-written.
    if sidecar_holds(&backup_path, &hash) {
        printer.status_simple(
            Role::Ok,
            format!("Already backed up at {}", backup_path.posix()),
        );
        return Ok(backup_path);
    }
    cfgd_core::atomic_write(&backup_path, &content)
        .map_err(|e| anyhow::anyhow!("Failed to backup {}: {}", target.posix(), e))?;
    if !sidecar_holds(&backup_path, &hash) {
        return Err(anyhow::anyhow!(
            "Backup of {} to {} did not verify (content hash mismatch)",
            target.posix(),
            backup_path.posix()
        ));
    }
    // Full `0o7777`: a sidecar is the file it preserves, and a setuid or sticky
    // bit dropped in the copy is not restorable from it.
    if let Some(mode) = cfgd_core::file_permissions_mode_full(&meta) {
        cfgd_core::set_file_permissions(&backup_path, mode)
            .map_err(|e| backup_error(target, &backup_path, e))?;
    }
    printer.status_simple(Role::Ok, format!("Backed up to {}", backup_path.posix()));
    Ok(backup_path)
}

/// How many `-N` disambiguators are tried before a reservation gives up.
///
/// A bound rather than an unbounded scan: past this many distinct originals
/// adopted at one target within one second, the situation is a loop and the
/// honest answer is an error, not a hundredth sidecar.
const BACKUP_DISAMBIGUATOR_LIMIT: u32 = 64;

/// Where this backup may be written without destroying an older one.
///
/// The primary `<target>.cfgd-backup` is what `profile update` and module
/// removal offer to restore, so it keeps the FIRST content adopted there — the
/// one that predates cfgd. A second, different original is stamped instead of
/// clobbering it, because a sidecar overwritten by the file that displaced it
/// is the same data loss the copy exists to prevent.
///
/// The stamp has one-second resolution, so it is a hint at a free name and
/// never a guarantee of one: two adoptions of the same target inside one second
/// derive the same stamp, and the second would clobber the first. Every
/// candidate is therefore checked, and a taken one moves to `-1`, `-2`, … until
/// a free name is found or the limit is reached.
fn reserve_backup_path(target: &Path, hash: Option<&str>) -> anyhow::Result<PathBuf> {
    let primary = cfgd_backup_path(target, "");
    if !sidecar_occupied(&primary, hash) {
        return Ok(primary);
    }
    let stamp = cfgd_core::utc_now_backup_stamp();
    let stamped = cfgd_backup_path(target, &format!(".{stamp}"));
    if !sidecar_occupied(&stamped, hash) {
        return Ok(stamped);
    }
    for n in 1..=BACKUP_DISAMBIGUATOR_LIMIT {
        let candidate = cfgd_backup_path(target, &format!(".{stamp}-{n}"));
        if !sidecar_occupied(&candidate, hash) {
            return Ok(candidate);
        }
    }
    Err(anyhow::anyhow!(
        "No free backup path beside {}: {} and {} disambiguators are all taken",
        target.posix(),
        stamped.posix(),
        BACKUP_DISAMBIGUATOR_LIMIT
    ))
}

/// Whether a candidate sidecar path is spoken for.
///
/// Judged with `symlink_metadata`, so a dangling link or a directory counts as
/// an entry someone made — writing over either is the loss the reservation
/// exists to avoid. `hash` is `Some` only for a regular-file backup, where a
/// sidecar already holding exactly these bytes is not an obstacle but the same
/// backup, and reusing it is the whole point.
fn sidecar_occupied(path: &Path, hash: Option<&str>) -> bool {
    if path.symlink_metadata().is_err() {
        return false;
    }
    match hash {
        Some(h) => !sidecar_holds(path, h),
        None => true,
    }
}

/// Whether the sidecar at `path` is a regular file holding exactly `hash`.
fn sidecar_holds(path: &Path, hash: &str) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_file())
        && std::fs::read(path).is_ok_and(|bytes| cfgd_core::sha256_hex(&bytes) == hash)
}

fn backup_error(target: &Path, backup_path: &Path, e: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "Failed to backup {} to {}: {}",
        target.posix(),
        backup_path.posix(),
        e
    )
}

/// Carry out one resolved policy against one profile-file action.
pub(in crate::cli) fn apply_conflict_policy(
    policy: ResolvedConflict,
    target: &Path,
    action: &mut reconciler::Action,
    printer: &Printer,
) -> anyhow::Result<()> {
    match policy {
        ResolvedConflict::Backup => {
            backup_file(target, printer)?;
        }
        ResolvedConflict::Skip => {
            let origin = match action {
                reconciler::Action::File(FileAction::Create { origin, .. })
                | reconciler::Action::File(FileAction::Update { origin, .. }) => origin.clone(),
                _ => LOCAL_LAYER.to_string(),
            };
            *action = reconciler::Action::File(FileAction::Skip {
                target: target.to_path_buf(),
                reason: UNMANAGED_SKIP_REASON.to_string(),
                origin,
            });
        }
        ResolvedConflict::Fail => return Err(unmanaged_conflict_error(target, None)),
        ResolvedConflict::Overwrite => {}
    }
    Ok(())
}

/// Apply --skip and --only filters to a plan, modifying it in place.
///
/// Also emits the diagnostics that only filtering can produce: the
/// deprecation for a legacy `modules[.name]` pattern, the stranded-install
/// warning for a filter that removed a package manager's bootstrap without
/// removing the installs that need it, and the SSOT zero-match accounting —
/// a warning for every `--skip`/`--only` token that matched zero actions. All
/// three live here so no call site can forget one, and no command hand-rolls
/// a second "matched nothing" message of its own (see `warn_zero_match_tokens`).
///
/// Returns whether any token matched zero actions, so the caller's
/// [`ScopeReport`] can refuse `MSG_NOTHING_TO_DO` on the strength of a filter
/// that never matched anything rather than a machine that is actually in
/// sync.
pub(in crate::cli) fn filter_plan(
    plan: &mut reconciler::Plan,
    skip: &[String],
    only: &[String],
    phase_filter: Option<&reconciler::PhaseFilter>,
    printer: &Printer,
    registry: &ProviderRegistry,
    known_modules: &HashSet<String>,
) -> bool {
    // Every selector the user supplied is materialised into the plan HERE, so
    // one pass owns the whole question of what this run will do. `--phase` is
    // resolved as a predicate downstream, which cannot split a node — and a
    // batched provision is exactly the node a manager-name selector must
    // split, or `--phase prerequisites.pipx` provisions npm as well.
    if let Some(reconciler::PhaseFilter::Selector(phase, selector)) = phase_filter {
        reconciler::restrict_provision_batches(plan, phase, selector);
    }
    if skip.is_empty() && only.is_empty() {
        return false;
    }
    warn_legacy_module_patterns(printer, skip, only);
    let skip = &normalize_legacy_phase_patterns(printer, "--skip", skip);
    let only = &normalize_legacy_phase_patterns(printer, "--only", only);

    // Owner tokens present before any pruning — the "did you mean one of
    // these" hint a zero-match token gets must describe what this run's plan
    // actually held, not what survives the very filter it failed to match.
    let owners_present: Vec<String> = {
        let mut tokens: Vec<String> = plan
            .phases
            .iter()
            .flat_map(|p| p.owned_actions())
            .map(|(owner, _)| owner.token())
            .collect();
        tokens.sort();
        tokens.dedup();
        tokens
    };

    let mut skip_hits = TokenHits::new(skip);
    let mut only_hits = TokenHits::new(only);

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
                                &mut skip_hits,
                                &mut only_hits,
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
                                &mut skip_hits,
                                &mut only_hits,
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

                // A provision node needs per-MANAGER granularity for the same
                // reason an install needs per-package: one node can carry a
                // batch of managers, and a pattern naming one of them must
                // take that one out rather than the whole `apt-get install`.
                // A solo provision runs the same path, so there is one rule
                // for both and no shape where they can disagree.
                if let reconciler::Action::Manager(
                    ma @ reconciler::ManagerAction::Provision {
                        via, depends_on, ..
                    },
                ) = &action
                {
                    let mut kept: Vec<String> = Vec::new();
                    for member in ma.provisioned_managers() {
                        let path = format!("{}.{}", phase_name.as_str(), member);
                        let matching_skips: Vec<&String> = skip
                            .iter()
                            .filter(|s| pattern_matches_action(s, owner, &path))
                            .collect();
                        for s in &matching_skips {
                            skip_hits.record(s);
                        }
                        let matching_onlys: Vec<&String> = only
                            .iter()
                            .filter(|o| {
                                pattern_matches_action(o, owner, &path) || pattern_matches(&path, o)
                            })
                            .collect();
                        for o in &matching_onlys {
                            only_hits.record(o);
                        }
                        let passes_only = only.is_empty() || !matching_onlys.is_empty();
                        if matching_skips.is_empty() && passes_only {
                            kept.push(member.to_string());
                        } else {
                            // Filtered away, a provision strands the installs
                            // that needed the manager.
                            removals.record(member, matching_skips.first().map(|s| s.as_str()));
                        }
                    }
                    if let Some((first, rest)) = kept.split_first() {
                        filtered_actions.push(reconciler::Action::Manager(
                            reconciler::ManagerAction::Provision {
                                manager: first.clone(),
                                via: via.clone(),
                                batched: rest.to_vec(),
                                depends_on: depends_on.clone(),
                            },
                        ));
                    }
                    continue;
                }

                // Non-package actions: action-level filtering
                let path = action_path(&phase_name, &action);
                let matching_skips: Vec<&String> = skip
                    .iter()
                    .filter(|s| pattern_matches_action(s, owner, &path))
                    .collect();
                for s in &matching_skips {
                    skip_hits.record(s);
                }
                let matching_onlys: Vec<&String> = only
                    .iter()
                    .filter(|o| {
                        pattern_matches_action(o, owner, &path) || pattern_matches(&path, o)
                    })
                    .collect();
                for o in &matching_onlys {
                    only_hits.record(o);
                }
                let passes_only = only.is_empty() || !matching_onlys.is_empty();

                if matching_skips.is_empty() && passes_only {
                    filtered_actions.push(action);
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

    let skip_missed =
        warn_zero_match_tokens(plan, "skip", &skip_hits, &owners_present, known_modules);
    let only_missed =
        warn_zero_match_tokens(plan, "only", &only_hits, &owners_present, known_modules);
    skip_missed || only_missed
}

/// Per-token match accounting for one `filter_plan` run's `--skip`/`--only`
/// tokens — the SSOT every selector/filter surface in the CLI that can
/// select zero of something routes through (directly, for `--skip`/`--only`;
/// by the same shape, for a command's own miss check) rather than hand-rolling
/// a second "matched nothing" message. Every supplied token starts at zero, so
/// a token shadowed entirely by an earlier one is still reported as a miss —
/// silence is never mistaken for coverage.
pub(in crate::cli) struct TokenHits {
    counts: std::collections::HashMap<String, usize>,
    order: Vec<String>,
}

impl TokenHits {
    pub(in crate::cli) fn new(tokens: &[String]) -> Self {
        let mut counts = std::collections::HashMap::new();
        let mut order = Vec::new();
        for t in tokens {
            if !counts.contains_key(t) {
                order.push(t.clone());
            }
            counts.entry(t.clone()).or_insert(0);
        }
        Self { counts, order }
    }

    pub(in crate::cli) fn record(&mut self, token: &str) {
        if let Some(c) = self.counts.get_mut(token) {
            *c += 1;
        }
    }

    /// Distinct supplied tokens that matched zero actions, in the order they
    /// were first supplied.
    pub(in crate::cli) fn misses(&self) -> Vec<&str> {
        self.order
            .iter()
            .filter(|t| self.counts.get(t.as_str()).copied().unwrap_or(0) == 0)
            .map(String::as_str)
            .collect()
    }
}

/// Every module name cfgd already knows about — declared locally in
/// `modules/`, or recorded in the source lockfile — read from disk ONCE by
/// the caller before filtering starts. `filter_plan` previously took
/// `config_dir` itself and re-read both the module tree and the lockfile
/// inside `module_known_but_unresolved`, once per zero-match token, putting
/// filesystem I/O behind a `&Path` threaded through every filter call site
/// (~30 of them in tests alone) purely so this one hint could ask a question
/// only it needed answered. A load failure (no `modules/`
/// directory, no lockfile) contributes nothing rather than erroring — the
/// hint degrades to "unknown", the same as it always did.
pub(in crate::cli) fn known_module_names(config_dir: &Path) -> HashSet<String> {
    let mut known = HashSet::new();
    if let Ok(local) = modules::load_modules(config_dir) {
        known.extend(local.into_keys());
    }
    if let Ok(lockfile) = modules::load_lockfile(config_dir) {
        known.extend(lockfile.modules.into_iter().map(|entry| entry.name));
    }
    known
}

/// Is `name` a module cfgd already knows about (declared locally, or a remote
/// module recorded in the lockfile), per the set [`known_module_names`]
/// already read once for this run. Distinguishes a genuinely unknown module
/// name (a typo) from a real module that simply is not part of THIS run's
/// graph, which gets a more actionable hint (`--module <name>`) than the
/// generic owner-token list. Pure lookup — no I/O — so it can be called once
/// per zero-match token with no cost.
fn module_known_but_unresolved(known_modules: &HashSet<String>, name: &str) -> bool {
    known_modules.contains(name)
}

/// Warn on every token in `hits` that matched zero actions — the zero-match
/// accounting every `--skip`/`--only` pass renders through. Pushed into
/// [`reconciler::Plan::warnings`] rather than printed directly, the same
/// route the undecidable-package-batch warning already takes: one producer
/// (here), one render ([`reconciler::ApplyRun::header`], via `alert` so it
/// stays visible at any depth and any verbosity — the same always-visible
/// guarantee [`warn_stranded_installs`] gives itself directly), and one
/// serialization (`build_plan_output`'s `warnings` field), so a `-o json`
/// consumer sees the same miss a human reading the header does instead of an
/// empty `phases` array with no explanation. Returns whether any token
/// missed.
fn warn_zero_match_tokens(
    plan: &mut reconciler::Plan,
    flag: &str,
    hits: &TokenHits,
    owners_present: &[String],
    known_modules: &HashSet<String>,
) -> bool {
    let misses = hits.misses();
    for token in &misses {
        let hint = token
            .strip_prefix("module:")
            .filter(|name| module_known_but_unresolved(known_modules, name))
            .map(|name| format!("to resolve a module outside the profile: --module {name}"))
            .or_else(|| {
                (!owners_present.is_empty())
                    .then(|| format!("owners present: {}", owners_present.join(", ")))
            });
        // `escape_control_chars`: the token is echoed verbatim below and, on
        // an interactive terminal, is untrusted input the user typed —
        // without this a `\r`/`\x1b[2K` token could repaint or erase the
        // very line describing it.
        let message = format!(
            "`--{flag} {}` matched no actions in this plan",
            cfgd_core::escape_control_chars(token)
        );
        plan.warnings.push(match hint {
            Some(hint) => format!("{message}; {hint}"),
            None => message,
        });
    }
    !misses.is_empty()
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
            .package_managers()
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
        "{culprit} removes {}; {} still {} a manager that is not installed and will not apply. Use `{flags}` to drop that work too.",
        cfgd_core::pluralize(removals.count, "bootstrap"),
        cfgd_core::pluralize(stranded_actions, "package action"),
        cfgd_core::agreeing_verb(stranded_actions, "name"),
    ));
}

/// Filter individual packages from an install/uninstall list based on skip/only
/// patterns, recording each token's hits into `skip_hits`/`only_hits` as it goes.
#[allow(clippy::too_many_arguments)]
fn filter_package_list(
    phase: &str,
    owner: &reconciler::Owner,
    manager: &str,
    packages: &[String],
    skip: &[String],
    only: &[String],
    skip_hits: &mut TokenHits,
    only_hits: &mut TokenHits,
) -> Vec<String> {
    packages
        .iter()
        .filter(|pkg| {
            let pkg_path = format!("{}.{}.{}", phase, manager, pkg);

            // Check skip: pattern can target the specific package, manager, phase or owner
            let matching_skips: Vec<&String> = skip
                .iter()
                .filter(|s| pattern_matches_action(s, owner, &pkg_path))
                .collect();
            for s in &matching_skips {
                skip_hits.record(s);
            }

            // Check only: the pattern must cover this package.
            // "packages" covers "packages.brew.ripgrep" (broad → specific)
            // "packages.brew.ripgrep" covers "packages.brew.ripgrep" (exact)
            // But "packages.brew.ripgrep" does NOT cover "packages.brew.fd"
            let matching_onlys: Vec<&String> = only
                .iter()
                .filter(|o| {
                    pattern_matches_action(o, owner, &pkg_path) || pattern_matches(&pkg_path, o)
                })
                .collect();
            for o in &matching_onlys {
                only_hits.record(o);
            }
            let pkg_only = only.is_empty() || !matching_onlys.is_empty();

            matching_skips.is_empty() && pkg_only
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
